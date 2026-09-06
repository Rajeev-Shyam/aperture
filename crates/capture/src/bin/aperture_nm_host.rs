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
//! HKCU registry key (no admin needed) through `nm_bridge::install_host_manifest`
//! — the same call the app makes on every launch. See `extension/README.md`.

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

    /// `install [--extension-id <ID>]... [--browser chrome|opera|edge]`
    /// Writes the host manifest + the per-browser HKCU registry key through the
    /// library (`nm_bridge::install_host_manifest`, 2026-09-06 — the app runs the
    /// same call on every launch, so this subcommand is for dev / repair). With
    /// no `--extension-id` the pinned [`aperture_capture::nm_bridge::EXTENSION_ID`]
    /// is used; Chrome and Opera share Chrome's hive, Edge has its own.
    pub fn install(args: &[String]) -> Result<(), String> {
        use aperture_capture::nm_bridge::{install_host_manifest, BrowserHive, EXTENSION_ID};
        let mut extension_ids = Vec::new();
        let mut hive = BrowserHive::Chrome;
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
                    let name = args.get(i).ok_or("--browser needs a value")?;
                    hive = BrowserHive::parse(name)
                        .ok_or_else(|| format!("unsupported browser: {name}"))?;
                }
                other => return Err(format!("unknown arg: {other}")),
            }
            i += 1;
        }
        if extension_ids.is_empty() {
            extension_ids.push(EXTENSION_ID.to_string());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let manifest_path = install_host_manifest(&exe, &extension_ids, &[hive])?;
        println!(
            "installed: manifest {} + HKCU\\{}",
            manifest_path.display(),
            hive.registry_key()
        );
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
