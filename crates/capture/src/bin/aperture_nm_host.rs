//! `aperture-nm-host` — the native-messaging host (ADR-027/028).
//!
//! Spawned BY THE BROWSER (per its registered host manifest) whenever the
//! Aperture Capture Bridge extension connects. It speaks two protocols:
//!
//! * **stdio ↔ browser**: Chrome native-messaging framing — a `u32` LE length
//!   prefix + UTF-8 JSON. stdout carries ONLY framed messages to the
//!   extension; all logging goes to stderr (the browser captures it).
//! * **named pipe ↔ core**: NDJSON lines to the running Aperture app's
//!   `nm_bridge` server, authenticated by the per-install token file. Core →
//!   host lines (toggle control, FIX 2.1) are relayed to the extension.
//!
//! Invariant honesty: this process opens **no sockets** — stdio + a same-user
//! named pipe only (doc 13 §2). When the core is not running (or capture is
//! OFF and the pipe closes), incoming browser messages are **dropped, never
//! queued** — user data does not accumulate outside the encrypted store.
//!
//! `install` subcommand: writes the host manifest JSON and the per-browser
//! HKCU registry key (no admin needed). See `extension/README.md`.

#[cfg(windows)]
mod host {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ClientOptions;

    const HOST_NAME: &str = "com.aperture.bridge";
    /// Browser→host messages are tiny ({url,title,...}); anything huge means a
    /// corrupted stream — exit rather than resync.
    const MAX_FRAME: u32 = 256 * 1024;

    fn pipe_name() -> String {
        std::env::var("APERTURE_NM_PIPE")
            .unwrap_or_else(|_| aperture_capture::nm_bridge::DEFAULT_PIPE_NAME.to_string())
    }

    fn token_path() -> PathBuf {
        std::env::var_os("APERTURE_NM_TOKEN_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(aperture_capture::nm_bridge::default_token_path)
    }

    /// Blocking stdin reader: Chrome framing → JSON strings. Runs on its own
    /// thread; channel closure signals "browser hung up" to the async side.
    fn spawn_stdin_reader() -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            loop {
                let mut len_buf = [0u8; 4];
                if stdin.read_exact(&mut len_buf).is_err() {
                    break; // EOF: extension port closed / browser exiting
                }
                let len = u32::from_le_bytes(len_buf);
                if len == 0 || len > MAX_FRAME {
                    eprintln!("aperture-nm-host: bad frame length {len}; exiting");
                    break;
                }
                let mut buf = vec![0u8; len as usize];
                if stdin.read_exact(&mut buf).is_err() {
                    break;
                }
                match String::from_utf8(buf) {
                    Ok(s) => {
                        if tx.send(s).is_err() {
                            break;
                        }
                    }
                    Err(_) => eprintln!("aperture-nm-host: non-UTF8 frame dropped"),
                }
            }
            // tx drops here → receiver sees None → clean shutdown.
        });
        rx
    }

    /// Blocking stdout writer: JSON strings → Chrome framing.
    fn spawn_stdout_writer() -> tokio::sync::mpsc::UnboundedSender<String> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        std::thread::spawn(move || {
            let mut stdout = std::io::stdout().lock();
            while let Some(msg) = rx.blocking_recv() {
                let bytes = msg.as_bytes();
                let len = (bytes.len() as u32).to_le_bytes();
                if stdout.write_all(&len).is_err()
                    || stdout.write_all(bytes).is_err()
                    || stdout.flush().is_err()
                {
                    std::process::exit(0); // browser gone
                }
            }
        });
        tx
    }

    pub async fn run() {
        eprintln!("aperture-nm-host: starting (pipe {})", pipe_name());
        let mut from_browser = spawn_stdin_reader();
        let to_browser = spawn_stdout_writer();
        let mut backoff_ms: u64 = 500;

        'outer: loop {
            // Try to reach the core. While unreachable, browser messages are
            // dropped (never queued — see module docs).
            let token = std::fs::read_to_string(token_path())
                .map(|t| t.trim().to_string())
                .unwrap_or_default();
            let pipe = if token.is_empty() {
                None // core has never run — nothing to authenticate with
            } else {
                ClientOptions::new().open(pipe_name()).ok()
            };

            let Some(pipe) = pipe else {
                let deadline = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms));
                tokio::pin!(deadline);
                backoff_ms = (backoff_ms * 2).min(30_000);
                loop {
                    tokio::select! {
                        _ = &mut deadline => continue 'outer,
                        m = from_browser.recv() => {
                            if m.is_none() { return; } // browser hung up
                            // else: drop the message (core unreachable)
                        }
                    }
                }
            };
            backoff_ms = 500;

            let (read_half, mut write_half) = tokio::io::split(pipe);
            let mut core_lines = BufReader::new(read_half).lines();
            let hello = format!(
                "{}\n",
                serde_json::json!({ "v": 1, "hello": { "token": token, "host": HOST_NAME } })
            );
            if write_half.write_all(hello.as_bytes()).await.is_err() {
                continue 'outer;
            }
            eprintln!("aperture-nm-host: connected to core");

            loop {
                tokio::select! {
                    m = from_browser.recv() => match m {
                        Some(json) => {
                            let line = format!("{json}\n");
                            if write_half.write_all(line.as_bytes()).await.is_err() {
                                continue 'outer; // core gone → reconnect
                            }
                        }
                        None => return, // browser hung up → exit
                    },
                    line = core_lines.next_line() => match line {
                        Ok(Some(ctl)) => {
                            // Toggle control etc. (FIX 2.1) → relay to extension.
                            let _ = to_browser.send(ctl);
                        }
                        _ => continue 'outer, // core gone → reconnect
                    },
                }
            }
        }
    }

    /// `install --extension-id <ID> [--browser chrome|edge] [--extension-id <ID2> ...]`
    /// Writes the host manifest + the per-browser HKCU registry key. Chrome and
    /// Opera GX both read Chrome's registry location on Windows; Edge has its
    /// own hive. [VERIFY on-target: Opera GX manifest discovery.]
    pub fn install(args: &[String]) -> Result<(), String> {
        let mut extension_ids = Vec::new();
        let mut browser = "chrome".to_string();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--extension-id" => {
                    i += 1;
                    extension_ids.push(
                        args.get(i)
                            .ok_or("--extension-id needs a value")?
                            .clone(),
                    );
                }
                "--browser" => {
                    i += 1;
                    browser = args.get(i).ok_or("--browser needs a value")?.clone();
                }
                other => return Err(format!("unknown arg: {other}")),
            }
            i += 1;
        }
        if extension_ids.is_empty() {
            return Err("at least one --extension-id is required".into());
        }

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let manifest_dir = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA unset")?
            .join("Aperture")
            .join("nm");
        std::fs::create_dir_all(&manifest_dir).map_err(|e| e.to_string())?;
        let manifest_path = manifest_dir.join(format!("{HOST_NAME}.json"));

        // Merge origins with any existing manifest (multi-browser installs).
        let mut origins: Vec<String> = std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| {
                v.get("allowed_origins").and_then(|a| {
                    a.as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|o| o.as_str().map(str::to_string))
                            .collect()
                    })
                })
            })
            .unwrap_or_default();
        for id in &extension_ids {
            let origin = format!("chrome-extension://{id}/");
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }

        let manifest = serde_json::json!({
            "name": HOST_NAME,
            "description": "Aperture native-messaging host (ADR-028): stdio bridge between the Capture Bridge extension and the local Aperture core. No sockets.",
            "path": exe.display().to_string(),
            "type": "stdio",
            "allowed_origins": origins,
        });
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        let key_path = match browser.as_str() {
            // Chromium browsers (Chrome, Opera/Opera GX) read Chrome's hive.
            "chrome" | "opera" => format!(r"Software\Google\Chrome\NativeMessagingHosts\{HOST_NAME}"),
            "edge" => format!(r"Software\Microsoft\Edge\NativeMessagingHosts\{HOST_NAME}"),
            other => return Err(format!("unsupported browser: {other}")),
        };
        write_hkcu_default_value(&key_path, &manifest_path.display().to_string())?;
        println!(
            "installed: manifest {} + HKCU\\{key_path}",
            manifest_path.display()
        );
        Ok(())
    }

    /// `HKCU\<key_path>` default value = `value` (REG_SZ). Per-user, no admin.
    fn write_hkcu_default_value(key_path: &str, value: &str) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
        };

        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let key_w = wide(key_path);
        let value_w = wide(value);
        let value_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(value_w.as_ptr().cast::<u8>(), value_w.len() * 2)
        };

        unsafe {
            let mut hkey = HKEY::default();
            let rc = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(key_w.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            );
            if rc.is_err() {
                return Err(format!("RegCreateKeyExW failed: {rc:?}"));
            }
            let rc = RegSetValueExW(hkey, PCWSTR::null(), 0, REG_SZ, Some(value_bytes));
            let _ = RegCloseKey(hkey);
            if rc.is_err() {
                return Err(format!("RegSetValueExW failed: {rc:?}"));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("install") {
        if let Err(e) = host::install(&args[1..]) {
            eprintln!("aperture-nm-host install: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Normal launch: the browser passes the extension origin (and
    // --parent-window on Windows) as args — tolerated, unused.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(host::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("aperture-nm-host is Windows-only");
    std::process::exit(1);
}
