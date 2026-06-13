//! At-rest key management (doc 13 §6) [VERIFY API surface].
//!
//! The history DB is encrypted with SQLCipher-style page encryption (doc 03,
//! doc 13 §6). The page key is:
//!   1. generated **once per install** as cryptographically-random bytes;
//!   2. **wrapped by DPAPI (current user)** — `CryptProtectData`, so the wrapped
//!      blob is bound to the Windows user account;
//!   3. **stored in Windows Credential Manager** (`CredWrite`, generic cred).
//!
//! On open, the wrapped blob is read back (`CredRead`), unwrapped
//! (`CryptUnprotectData`), and handed to [`aperture_db::Db::open_encrypted`] as
//! `wrapped_key`. Key loss ⇒ **DB unreadable, by design** — documented plainly
//! to the user ("your history cannot be recovered without your Windows
//! account", doc 13 §6). Key wrapping isolates the blast radius of an
//! encryption-lib CVE (doc 13 §9).
//!
//! INVARIANT (2): no network here; DPAPI + Credential Manager are local OS APIs.

use crate::PrivacyError;

/// Credential Manager target name under which the wrapped key is stored
/// (doc 13 §6). One blob per install, per Windows user [VERIFY naming at M9].
pub const CREDENTIAL_TARGET: &str = "Aperture/db-key/v1";

/// Length of the generated raw page key in bytes (256-bit) [VERIFY against the
/// chosen SQLCipher-style library's expected key size at M9].
pub const KEY_LEN: usize = 32;

/// The unwrapped DB page key. Held only as long as needed to open the DB;
/// zeroized on drop [VERIFY: wire up a zeroizing wrapper at M9].
pub struct DbKey {
    // TODO(M9): hold the raw key bytes in a zeroize-on-drop buffer; expose
    // `as_bytes()` to pass to `Db::open_encrypted`.
}

impl DbKey {
    /// Borrow the raw key bytes to hand to `aperture_db::Db::open_encrypted`.
    pub fn as_bytes(&self) -> &[u8] {
        // TODO(M9): return the unwrapped key slice.
        todo!("M9: expose unwrapped DB key bytes (doc 13 §6)")
    }
}

/// Get the install's DB key, creating + persisting it on first run.
///
/// Flow (doc 13 §6) [VERIFY API surface]:
///   - try Credential Manager (`CredRead` @ [`CREDENTIAL_TARGET`]);
///     - hit  -> `CryptUnprotectData` -> [`DbKey`];
///     - miss -> generate [`KEY_LEN`] random bytes -> `CryptProtectData`
///               -> `CredWrite` -> return the fresh key.
///   - DPAPI unwrap failure (e.g. wrong user / corrupted blob) -> the DB is
///     unreadable by design; surface [`PrivacyError::KeyManager`].
pub fn get_or_create_key() -> Result<DbKey, PrivacyError> {
    // TODO(M9): implement read-or-create via the windows crate
    // (Win32_Security_Credentials + Win32_Security_Cryptography). All map errors
    // to PrivacyError::KeyManager.
    todo!("M9: DPAPI-wrapped per-install key via Credential Manager (doc 13 §6)")
}

/// Generate a fresh random page key (first run only). Uses an OS CSPRNG.
fn generate_key() -> Result<[u8; KEY_LEN], PrivacyError> {
    // TODO(M9): fill KEY_LEN bytes from a CSPRNG (e.g. BCryptGenRandom). [VERIFY]
    todo!("M9: CSPRNG key generation (doc 13 §6)")
}

/// Wrap `raw` with DPAPI (current user) -> opaque blob (`CryptProtectData`).
fn dpapi_wrap(_raw: &[u8]) -> Result<Vec<u8>, PrivacyError> {
    // TODO(M9): CryptProtectData(CRYPTPROTECT_*; current-user scope). [VERIFY]
    todo!("M9: DPAPI wrap (doc 13 §6)")
}

/// Unwrap a DPAPI blob (`CryptUnprotectData`). Failure ⇒ DB unreadable by design.
fn dpapi_unwrap(_blob: &[u8]) -> Result<Vec<u8>, PrivacyError> {
    // TODO(M9): CryptUnprotectData; map failure to PrivacyError::KeyManager. [VERIFY]
    todo!("M9: DPAPI unwrap (doc 13 §6)")
}

/// Persist the wrapped blob to Credential Manager (`CredWrite`, generic cred).
fn cred_write(_target: &str, _wrapped: &[u8]) -> Result<(), PrivacyError> {
    // TODO(M9): CredWrite a CRED_TYPE_GENERIC entry. [VERIFY]
    todo!("M9: Credential Manager write (doc 13 §6)")
}

/// Read the wrapped blob from Credential Manager (`CredRead`); `Ok(None)` on miss.
fn cred_read(_target: &str) -> Result<Option<Vec<u8>>, PrivacyError> {
    // TODO(M9): CredRead; ERROR_NOT_FOUND -> Ok(None). [VERIFY]
    todo!("M9: Credential Manager read (doc 13 §6)")
}
