//! At-rest key management (doc 13 §6).
//!
//! The history DB is encrypted with SQLCipher-style page encryption (doc 03,
//! doc 13 §6). The page key is:
//!   1. generated **once per install** as cryptographically-random bytes
//!      (`BCryptGenRandom`, system-preferred RNG);
//!   2. **wrapped by DPAPI (current user)** — `CryptProtectData`, so the wrapped
//!      blob is bound to the Windows user account;
//!   3. **stored in Windows Credential Manager** (`CredWriteW`, generic cred).
//!
//! On open, the wrapped blob is read back (`CredReadW`), unwrapped
//! (`CryptUnprotectData`), and handed to [`aperture_db::Db::open_encrypted`].
//! Key loss ⇒ **DB unreadable, by design** — documented plainly to the user
//! ("your history cannot be recovered without your Windows account", doc 13 §6).
//! Key wrapping isolates the blast radius of an encryption-lib CVE (doc 13 §9).
//!
//! **Why two layers.** Credential Manager alone stores a secret retrievable by
//! any process running as the user; DPAPI alone has nowhere durable to live.
//! Together the blob is both bound to the account *and* has a home. Neither
//! defends against same-user malware — explicitly out of the threat model
//! (doc 13 §1).
//!
//! INVARIANT (2): no network here; DPAPI + Credential Manager are local OS APIs.

use crate::PrivacyError;

/// Credential Manager target name under which the wrapped key is stored
/// (doc 13 §6). One blob per install, per Windows user.
pub const CREDENTIAL_TARGET: &str = "Aperture/db-key/v1";

/// Length of the generated raw page key in bytes (256-bit) — SQLCipher's raw-key
/// form (`PRAGMA key = "x'…'"`) takes exactly 32 bytes with no KDF.
pub const KEY_LEN: usize = 32;

/// The unwrapped DB page key. Zeroized on drop so the raw bytes do not linger in
/// process memory after the connection is opened.
pub struct DbKey(zeroize::Zeroizing<Vec<u8>>);

impl DbKey {
    /// Borrow the raw key bytes to hand to [`aperture_db::Db::open_encrypted`].
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for DbKey {
    /// Never print key material, even accidentally via `{:?}` on a parent struct.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DbKey({} bytes, redacted)", self.0.len())
    }
}

/// Get the install's DB key, creating + persisting it on first run (doc 13 §6).
///
/// - Credential Manager hit  → `CryptUnprotectData` → [`DbKey`].
/// - Credential Manager miss → generate [`KEY_LEN`] CSPRNG bytes →
///   `CryptProtectData` → `CredWriteW` → return the fresh key.
/// - DPAPI unwrap failure (wrong user / corrupted blob) → the DB is unreadable
///   by design; surfaced as [`PrivacyError::KeyManager`].
pub fn get_or_create_key() -> Result<DbKey, PrivacyError> {
    get_or_create_key_at(CREDENTIAL_TARGET)
}

/// [`get_or_create_key`] against an explicit Credential Manager target. Exists so
/// the round-trip is testable without touching the real install's key.
pub fn get_or_create_key_at(target: &str) -> Result<DbKey, PrivacyError> {
    if let Some(wrapped) = cred_read(target)? {
        let raw = dpapi_unwrap(&wrapped)?;
        if raw.len() != KEY_LEN {
            return Err(PrivacyError::KeyManager(format!(
                "stored key is {} bytes, expected {KEY_LEN} (doc 13 §6)",
                raw.len()
            )));
        }
        return Ok(DbKey(zeroize::Zeroizing::new(raw)));
    }
    let fresh = generate_key()?;
    let wrapped = dpapi_wrap(&fresh)?;
    cred_write(target, &wrapped)?;
    tracing::info!(target, "generated + stored a new DB page key (doc 13 §6)");
    Ok(DbKey(zeroize::Zeroizing::new(fresh.to_vec())))
}

/// Delete the stored key. Used by tests and by an explicit "forget this install"
/// action — after this the existing DB is permanently unreadable (doc 13 §6).
pub fn delete_key_at(target: &str) -> Result<(), PrivacyError> {
    cred_delete(target)
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use super::{PrivacyError, KEY_LEN};

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_NOT_FOUND, LocalFree, HLOCAL};
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
        CRED_TYPE_GENERIC,
    };
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, CryptProtectData, CryptUnprotectData, BCRYPT_ALG_HANDLE,
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, CRYPT_INTEGER_BLOB,
    };

    /// Null-terminated UTF-16, kept alive by the caller while the PCWSTR is used.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            // DPAPI does not mutate the input blob; the cast is required by the
            // C signature, which is not const-correct.
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    /// Copy an out-blob into a Vec and release the OS allocation.
    ///
    /// SAFETY: `out` must be a blob DPAPI populated via `LocalAlloc`.
    unsafe fn take_blob(out: &CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let owned = slice.to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as *mut std::ffi::c_void));
        owned
    }

    pub fn generate_key() -> Result<[u8; KEY_LEN], PrivacyError> {
        let mut key = [0u8; KEY_LEN];
        // SAFETY: `key` is a valid writable buffer for its own length; the
        // system-preferred RNG needs no algorithm handle.
        let status = unsafe {
            BCryptGenRandom(
                BCRYPT_ALG_HANDLE(std::ptr::null_mut()),
                &mut key,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status.is_ok() {
            Ok(key)
        } else {
            Err(PrivacyError::KeyManager(format!(
                "BCryptGenRandom failed: {status:?}"
            )))
        }
    }

    pub fn dpapi_wrap(raw: &[u8]) -> Result<Vec<u8>, PrivacyError> {
        let input = blob(raw);
        let mut out = CRYPT_INTEGER_BLOB::default();
        let descr = wide("Aperture DB key");
        // SAFETY: `input` points at `raw` for the duration of the call; `out` is
        // written by DPAPI and freed in `take_blob`. Current-user scope (no
        // CRYPTPROTECT_LOCAL_MACHINE flag) binds the blob to this account.
        unsafe {
            CryptProtectData(
                &input,
                PCWSTR(descr.as_ptr()),
                None,
                None,
                None,
                0,
                &mut out,
            )
            .map_err(|e| PrivacyError::KeyManager(format!("CryptProtectData failed: {e}")))?;
            Ok(take_blob(&out))
        }
    }

    pub fn dpapi_unwrap(wrapped: &[u8]) -> Result<Vec<u8>, PrivacyError> {
        let input = blob(wrapped);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as `dpapi_wrap`. A wrong-user or corrupted blob returns Err —
        // the "DB unreadable by design" path (doc 13 §6).
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, 0, &mut out).map_err(|e| {
                PrivacyError::KeyManager(format!("CryptUnprotectData failed: {e}"))
            })?;
            Ok(take_blob(&out))
        }
    }

    pub fn cred_write(target: &str, wrapped: &[u8]) -> Result<(), PrivacyError> {
        let mut target_w = wide(target);
        let cred = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target_w.as_mut_ptr()),
            CredentialBlobSize: wrapped.len() as u32,
            CredentialBlob: wrapped.as_ptr() as *mut u8,
            // Local machine persistence: survives logoff, never roams to a
            // domain profile (the key is DPAPI-bound to this machine's account).
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: every pointer in `cred` outlives the call; CredWriteW copies.
        unsafe { CredWriteW(&cred, 0) }
            .map_err(|e| PrivacyError::KeyManager(format!("CredWriteW failed: {e}")))
    }

    pub fn cred_read(target: &str) -> Result<Option<Vec<u8>>, PrivacyError> {
        let target_w = wide(target);
        let mut out: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `out` receives an OS allocation freed by CredFree below; the
        // blob is copied out before the free.
        unsafe {
            match CredReadW(PCWSTR(target_w.as_ptr()), CRED_TYPE_GENERIC, 0, &mut out) {
                Ok(()) => {
                    if out.is_null() {
                        return Ok(None);
                    }
                    let cred = &*out;
                    let bytes = std::slice::from_raw_parts(
                        cred.CredentialBlob,
                        cred.CredentialBlobSize as usize,
                    )
                    .to_vec();
                    CredFree(out as *const std::ffi::c_void);
                    Ok(Some(bytes))
                }
                // A missing credential is first run, not an error (doc 13 §6).
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
                Err(e) => Err(PrivacyError::KeyManager(format!("CredReadW failed: {e}"))),
            }
        }
    }

    pub fn cred_delete(target: &str) -> Result<(), PrivacyError> {
        let target_w = wide(target);
        // SAFETY: `target_w` outlives the call.
        unsafe {
            match CredDeleteW(PCWSTR(target_w.as_ptr()), CRED_TYPE_GENERIC, 0) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
                Err(e) => Err(PrivacyError::KeyManager(format!("CredDeleteW failed: {e}"))),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Non-Windows: Aperture is a Windows-11 product (doc 01). The stubs exist only
// so the crate type-checks in a cross-platform `cargo check`; they never run.
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
mod imp {
    use super::{PrivacyError, KEY_LEN};

    const UNSUPPORTED: &str = "at-rest key management requires Windows (DPAPI + Credential Manager, doc 13 §6)";

    pub fn generate_key() -> Result<[u8; KEY_LEN], PrivacyError> {
        Err(PrivacyError::KeyManager(UNSUPPORTED.into()))
    }
    pub fn dpapi_wrap(_raw: &[u8]) -> Result<Vec<u8>, PrivacyError> {
        Err(PrivacyError::KeyManager(UNSUPPORTED.into()))
    }
    pub fn dpapi_unwrap(_blob: &[u8]) -> Result<Vec<u8>, PrivacyError> {
        Err(PrivacyError::KeyManager(UNSUPPORTED.into()))
    }
    pub fn cred_write(_target: &str, _wrapped: &[u8]) -> Result<(), PrivacyError> {
        Err(PrivacyError::KeyManager(UNSUPPORTED.into()))
    }
    pub fn cred_read(_target: &str) -> Result<Option<Vec<u8>>, PrivacyError> {
        Err(PrivacyError::KeyManager(UNSUPPORTED.into()))
    }
    pub fn cred_delete(_target: &str) -> Result<(), PrivacyError> {
        Err(PrivacyError::KeyManager(UNSUPPORTED.into()))
    }
}

use imp::{cred_delete, cred_read, cred_write, dpapi_unwrap, dpapi_wrap, generate_key};

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Unique-ish per test so a parallel run never collides in the real store.
    fn scratch_target(name: &str) -> String {
        format!("Aperture/test/{name}/{}", std::process::id())
    }

    #[test]
    fn generated_keys_are_the_right_length_and_not_constant() {
        let a = generate_key().expect("CSPRNG");
        let b = generate_key().expect("CSPRNG");
        assert_eq!(a.len(), KEY_LEN);
        assert_ne!(a, b, "BCryptGenRandom must not return a constant");
        assert!(a.iter().any(|&x| x != 0), "key must not be all zeroes");
    }

    #[test]
    fn dpapi_roundtrips_the_key() {
        let key = generate_key().expect("CSPRNG");
        let wrapped = dpapi_wrap(&key).expect("wrap");
        assert_ne!(wrapped.as_slice(), key.as_slice(), "wrapped blob is not the plaintext key");
        let unwrapped = dpapi_unwrap(&wrapped).expect("unwrap");
        assert_eq!(unwrapped, key.to_vec());
    }

    #[test]
    fn corrupted_blob_fails_closed() {
        let key = generate_key().expect("CSPRNG");
        let mut wrapped = dpapi_wrap(&key).expect("wrap");
        let last = wrapped.len() - 1;
        wrapped[last] ^= 0xFF;
        // Tampering must surface an error, never silently return garbage bytes.
        assert!(dpapi_unwrap(&wrapped).is_err(), "DB unreadable by design (doc 13 §6)");
    }

    #[test]
    fn credential_manager_roundtrips_and_reports_a_miss_as_none() {
        let target = scratch_target("credroundtrip");
        let _ = delete_key_at(&target);
        assert!(cred_read(&target).expect("read miss").is_none(), "miss => first run");

        cred_write(&target, b"wrapped-bytes").expect("write");
        assert_eq!(cred_read(&target).expect("read").as_deref(), Some(&b"wrapped-bytes"[..]));

        delete_key_at(&target).expect("delete");
        assert!(cred_read(&target).expect("read after delete").is_none());
    }

    #[test]
    fn get_or_create_is_stable_across_calls_then_gone_after_delete() {
        let target = scratch_target("getorcreate");
        let _ = delete_key_at(&target);

        let first = get_or_create_key_at(&target).expect("create");
        assert_eq!(first.as_bytes().len(), KEY_LEN);
        let second = get_or_create_key_at(&target).expect("read back");
        assert_eq!(
            first.as_bytes(),
            second.as_bytes(),
            "the same install must get the same key, or its DB becomes unreadable"
        );

        delete_key_at(&target).expect("delete");
        let third = get_or_create_key_at(&target).expect("regenerate");
        assert_ne!(
            first.as_bytes(),
            third.as_bytes(),
            "after key loss a NEW key is minted — the old DB stays unreadable (doc 13 §6)"
        );
        let _ = delete_key_at(&target);
    }

    #[test]
    fn debug_never_prints_key_material() {
        let key = get_or_create_key_at(&scratch_target("debugfmt"));
        if let Ok(k) = key {
            let rendered = format!("{k:?}");
            assert!(rendered.contains("redacted"));
            let hex: String = k.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
            assert!(!rendered.contains(&hex), "Debug must not leak the key");
            let _ = delete_key_at(&scratch_target("debugfmt"));
        }
    }
}
