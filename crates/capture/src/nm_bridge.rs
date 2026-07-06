//! Native-messaging bridge — the core side of the browser-extension feed
//! (ADR-027/ADR-028, doc 05 §3, FIX 2.1/2.2).
//!
//! Transport chain: extension (MV3) → browser stdio → `aperture-nm-host`
//! (this crate's bin target) → **Windows named pipe** → this server → the
//! normalizer → bus. Native messaging is the *primary* transport (stdio — no
//! socket); the named pipe is same-machine kernel IPC, **not** a network
//! socket, so the two-emitter rule stays literally true (doc 13 §2). The
//! ADR-028 authenticated-loopback requirements are applied to the pipe anyway
//! (defense in depth): a per-install token gates every connection, and the
//! pipe's default DACL already scopes it to the same user.
//!
//! Toggle obedience (FIX 2.1): OFF flips [`NmBridge::set_forwarding`] `false`,
//! which (a) drops everything server-side and (b) pushes `{"type":"toggle",
//! "capturing":false}` to every connected host, which relays it to the
//! extension — the extension path is part of "everything stops", never a route
//! around the toggle.
//!
//! Exclusion (FIX 2.2): extension-fed URLs traverse the **same**
//! exclusion+redaction pipeline as UIA-sourced ones — both funnel through
//! [`Normalizer`] methods that run `apply_exclusion` (incl. `url_pattern`).
//! Incognito never arrives (manifest `"incognito": "not_allowed"`) and is
//! dropped again here if a message claims it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::normalizer::{ExtensionUrlSource, Normalizer};
use crate::sampler::{epoch_ms, ForegroundContext};

/// Default pipe name (host binary + server must agree). Overridable for tests
/// via [`NmBridgeConfig`]; the host bin honors `APERTURE_NM_PIPE`.
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\aperture.bridge.v1";

/// Media `position_s` delta below which a tick is coalesced away
/// [ASSUMPTION — pause/hidden events always publish, so the exact
/// leaving-position is captured; ticks are insurance between them].
const MEDIA_MIN_DELTA_S: f64 = 15.0;
/// Publish at least this often for a continuously-playing video.
const MEDIA_MAX_SILENCE_MS: i64 = 30_000;

/// Wire config for the bridge (paths pinned at composition; tests override).
#[derive(Debug, Clone)]
pub struct NmBridgeConfig {
    pub pipe_name: String,
    /// Per-install auth token file. Created (0-length-safe) on first server
    /// spawn; the host binary reads the same file.
    pub token_path: PathBuf,
}

impl Default for NmBridgeConfig {
    fn default() -> Self {
        Self {
            pipe_name: DEFAULT_PIPE_NAME.to_string(),
            token_path: default_token_path(),
        }
    }
}

/// `%LOCALAPPDATA%\Aperture\nm-token` — same-user readable, which matches the
/// threat model (same-user malware is out of scope, doc 13 §1).
pub fn default_token_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Aperture").join("nm-token")
}

/// A message from the extension, relayed verbatim by the host (protocol v1).
/// Unknown fields are tolerated (doc 15 §6).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtMessage {
    Navigation {
        url: String,
        #[serde(default)]
        title: Option<String>,
        browser: String,
        #[serde(default)]
        incognito: Option<bool>,
    },
    MediaState {
        #[serde(default)]
        url: Option<String>,
        video_id: String,
        #[serde(default)]
        position_s: Option<f64>,
        #[serde(default)]
        state: Option<String>,
        #[serde(default)]
        title: Option<String>,
        browser: String,
        #[serde(default)]
        incognito: Option<bool>,
    },
}

/// Last-published media snapshot per browser brand (coalescing state).
#[derive(Debug, Clone)]
struct MediaTrack {
    video_id: String,
    position_s: Option<f64>,
    state: Option<String>,
    published_ts: i64,
}

/// Shared bridge state: the toggle-obeying forwarding flag, the per-browser
/// URL cache (the extension-primary source the normalizer consults), media
/// coalescing, and control channels to connected hosts.
pub struct NmBridge {
    config: NmBridgeConfig,
    forwarding: AtomicBool,
    /// brand → freshest active-tab URL (extension-primary source, ADR-027).
    last_urls: Mutex<HashMap<String, String>>,
    /// brand → connection id that owns it (cleared on that host's disconnect).
    brand_owner: Mutex<HashMap<String, u64>>,
    media: Mutex<HashMap<String, MediaTrack>>,
    /// Control senders to each connected host (toggle relay, FIX 2.1).
    controls: Mutex<Vec<(u64, tokio::sync::mpsc::UnboundedSender<String>)>>,
    next_conn_id: AtomicU64,
}

impl NmBridge {
    pub fn new(config: NmBridgeConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            // Mirrors the toggle's initial Off state — the composition root
            // flips it when orchestration drives capture ON (doc 12 §6).
            forwarding: AtomicBool::new(false),
            last_urls: Mutex::new(HashMap::new()),
            brand_owner: Mutex::new(HashMap::new()),
            media: Mutex::new(HashMap::new()),
            controls: Mutex::new(Vec::new()),
            next_conn_id: AtomicU64::new(1),
        })
    }

    /// FIX 2.1: obey the capture toggle. `false` drops everything server-side
    /// and tells every connected host (which relays to the extension) to stop
    /// forwarding; `true` restores. Sync + non-blocking — callable from the
    /// toggle's blocking teardown step.
    pub fn set_forwarding(&self, on: bool) {
        self.forwarding.store(on, Ordering::SeqCst);
        let line = format!(
            "{}\n",
            serde_json::json!({ "type": "toggle", "capturing": on })
        );
        let mut controls = self.controls.lock().expect("controls lock");
        controls.retain(|(_, tx)| tx.send(line.clone()).is_ok());
        if !on {
            // OFF also drops the caches: a URL observed pre-OFF must not leak
            // into a post-ON verdict as "current".
            self.last_urls.lock().expect("url lock").clear();
            self.media.lock().expect("media lock").clear();
        }
    }

    pub fn forwarding(&self) -> bool {
        self.forwarding.load(Ordering::SeqCst)
    }

    /// Read-or-create the per-install token (hex uuid, 128-bit).
    pub fn ensure_token(&self) -> std::io::Result<String> {
        if let Ok(existing) = std::fs::read_to_string(&self.config.token_path) {
            let trimmed = existing.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }
        if let Some(dir) = self.config.token_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        std::fs::write(&self.config.token_path, &token)?;
        Ok(token)
    }

    /// Handle one parsed extension message (already authenticated). Returns the
    /// events it published (tests assert on them).
    fn handle_message(
        &self,
        msg: ExtMessage,
        normalizer: &Normalizer,
        foreground: &Mutex<ForegroundContext>,
    ) -> usize {
        if !self.forwarding() {
            return 0; // toggle OFF: nothing processed, nothing cached (FIX 2.1)
        }
        let now = epoch_ms();
        match msg {
            ExtMessage::Navigation {
                url,
                title,
                browser,
                incognito,
            } => {
                if incognito.unwrap_or(false) || !url.starts_with("http") {
                    return 0;
                }
                self.last_urls
                    .lock()
                    .expect("url lock")
                    .insert(browser.clone(), url.clone());
                // Only a *foreground* browser's navigation is a navigation
                // event (doc 03 §2 parity with the WinEvent-driven UIA path);
                // background updates just keep the cache fresh for the next
                // focus-driven resolve.
                let fg_is_this_browser = {
                    let fg = foreground.lock().expect("foreground lock");
                    fg.identity
                        .process
                        .as_deref()
                        .and_then(brand_for_process)
                        .is_some_and(|b| b == browser)
                };
                if !fg_is_this_browser {
                    return 0;
                }
                normalizer.extension_navigation(&browser, url, title, now);
                1
            }
            ExtMessage::MediaState {
                url,
                video_id,
                position_s,
                state,
                title,
                browser,
                incognito,
            } => {
                if incognito.unwrap_or(false) {
                    return 0;
                }
                if !self.should_publish_media(&browser, &video_id, position_s, &state, now) {
                    return 0;
                }
                let published = normalizer
                    .extension_media(&browser, url, video_id.clone(), position_s, state.clone(), title, now)
                    .is_some();
                if published {
                    self.media.lock().expect("media lock").insert(
                        browser,
                        MediaTrack {
                            video_id,
                            position_s,
                            state,
                            published_ts: now,
                        },
                    );
                }
                usize::from(published)
            }
        }
    }

    /// Coalescing (volume control, [ASSUMPTION]): publish on video change,
    /// play/pause change, ≥15 s position delta (seeks), or 30 s of silence.
    /// The content script reports pause/hidden with the exact position, so the
    /// leaving-position — the one US1 resumes at — is always captured.
    fn should_publish_media(
        &self,
        brand: &str,
        video_id: &str,
        position_s: Option<f64>,
        state: &Option<String>,
        now: i64,
    ) -> bool {
        let media = self.media.lock().expect("media lock");
        match media.get(brand) {
            None => true,
            Some(last) => {
                last.video_id != video_id
                    || last.state != *state
                    || now - last.published_ts >= MEDIA_MAX_SILENCE_MS
                    || match (last.position_s, position_s) {
                        (Some(a), Some(b)) => (b - a).abs() >= MEDIA_MIN_DELTA_S,
                        (a, b) => a.is_some() != b.is_some(),
                    }
            }
        }
    }

    fn register_control(
        &self,
        conn_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) {
        // A just-connected host learns the current toggle state immediately.
        let line = format!(
            "{}\n",
            serde_json::json!({ "type": "toggle", "capturing": self.forwarding() })
        );
        let _ = tx.send(line);
        self.controls
            .lock()
            .expect("controls lock")
            .push((conn_id, tx));
    }

    fn drop_connection(&self, conn_id: u64) {
        self.controls
            .lock()
            .expect("controls lock")
            .retain(|(id, _)| *id != conn_id);
        // Forget URLs owned by this host: a dead extension feed must not keep
        // answering "current URL" (the normalizer then falls back to UIA).
        let mut owner = self.brand_owner.lock().expect("owner lock");
        let brands: Vec<String> = owner
            .iter()
            .filter(|(_, c)| **c == conn_id)
            .map(|(b, _)| b.clone())
            .collect();
        for b in brands {
            owner.remove(&b);
            self.last_urls.lock().expect("url lock").remove(&b);
            self.media.lock().expect("media lock").remove(&b);
        }
    }

    fn claim_brand(&self, brand: &str, conn_id: u64) {
        self.brand_owner
            .lock()
            .expect("owner lock")
            .insert(brand.to_string(), conn_id);
    }
}

/// The extension-primary URL source (ADR-027): the normalizer asks here first;
/// a `None` (no live feed for that browser) falls back to the UIA read.
impl ExtensionUrlSource for NmBridge {
    fn current_url(&self, process: &str) -> Option<String> {
        let brand = brand_for_process(process)?;
        self.last_urls.lock().expect("url lock").get(brand).cloned()
    }
}

/// Map a Windows process name to the extension's browser brand.
pub fn brand_for_process(process: &str) -> Option<&'static str> {
    match process.to_ascii_lowercase().as_str() {
        "chrome.exe" => Some("chrome"),
        "opera.exe" | "opera_gx.exe" => Some("opera"),
        "msedge.exe" => Some("edge"),
        _ => None,
    }
}

/// Inverse mapping for synthesizing event identity from a brand.
pub fn process_for_brand(brand: &str) -> String {
    match brand {
        "chrome" => "chrome.exe".to_string(),
        "opera" => "opera.exe".to_string(),
        "edge" => "msedge.exe".to_string(),
        other => format!("{other}.exe"),
    }
}

/// Spawn the named-pipe server: accepts host connections, authenticates each
/// with the per-install token, then pumps messages into the normalizer.
/// Returns the accept-loop handle (abort on shutdown).
#[cfg(windows)]
pub fn spawn_server(
    bridge: Arc<NmBridge>,
    normalizer: Arc<Normalizer>,
    foreground: Arc<Mutex<ForegroundContext>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let token = match bridge.ensure_token() {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("nm-bridge: cannot create token file: {e} — extension feed disabled");
                return;
            }
        };
        let pipe_name = bridge.config.pipe_name.clone();
        let mut first = true;
        loop {
            let server = {
                let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
                if first {
                    opts.first_pipe_instance(true);
                }
                match opts.create(&pipe_name) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("nm-bridge: pipe create failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                        continue;
                    }
                }
            };
            first = false;
            if let Err(e) = server.connect().await {
                tracing::debug!("nm-bridge: pipe connect aborted: {e}");
                continue;
            }
            let bridge = Arc::clone(&bridge);
            let normalizer = Arc::clone(&normalizer);
            let foreground = Arc::clone(&foreground);
            let token = token.clone();
            tokio::spawn(async move {
                serve_connection(server, bridge, normalizer, foreground, token).await;
            });
        }
    })
}

/// One host connection: NDJSON lines in; control lines out. First line must be
/// the hello `{"v":1,"hello":{"token":"..."}}` with the right token.
#[cfg(windows)]
async fn serve_connection(
    pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    bridge: Arc<NmBridge>,
    normalizer: Arc<Normalizer>,
    foreground: Arc<Mutex<ForegroundContext>>,
    token: String,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let conn_id = bridge.next_conn_id.fetch_add(1, Ordering::SeqCst);
    let (read_half, mut write_half) = tokio::io::split(pipe);
    let mut lines = BufReader::new(read_half).lines();

    // Authenticate (ADR-028 defense-in-depth): first line or bust.
    let authed = match lines.next_line().await {
        Ok(Some(first)) => serde_json::from_str::<serde_json::Value>(&first)
            .ok()
            .and_then(|v| {
                v.get("hello")?
                    .get("token")
                    .and_then(|t| t.as_str())
                    .map(|t| t == token)
            })
            .unwrap_or(false),
        _ => false,
    };
    if !authed {
        tracing::warn!("nm-bridge: connection {conn_id} failed auth — dropped");
        return;
    }
    tracing::info!("nm-bridge: host connected (conn {conn_id})");

    let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    bridge.register_control(conn_id, ctl_tx);

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(raw)) => {
                        match serde_json::from_str::<ExtMessage>(&raw) {
                            Ok(msg) => {
                                let brand = match &msg {
                                    ExtMessage::Navigation { browser, .. }
                                    | ExtMessage::MediaState { browser, .. } => browser.clone(),
                                };
                                bridge.claim_brand(&brand, conn_id);
                                bridge.handle_message(msg, &normalizer, &foreground);
                            }
                            Err(e) => tracing::debug!("nm-bridge: unparseable line ignored: {e}"),
                        }
                    }
                    Ok(None) | Err(_) => break, // host gone
                }
            }
            ctl = ctl_rx.recv() => {
                match ctl {
                    Some(line) => {
                        if write_half.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
    bridge.drop_connection(conn_id);
    tracing::info!("nm-bridge: host disconnected (conn {conn_id})");
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::exclusion::{ExclusionList, ExclusionRule};
    use crate::hooks::WindowIdentity;
    use aperture_contracts::EventType;
    use aperture_event_bus::EventBus;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn test_bridge(name: &str) -> (Arc<NmBridge>, NmBridgeConfig) {
        let dir = std::env::temp_dir().join("aperture-nm-test");
        std::fs::create_dir_all(&dir).unwrap();
        let config = NmBridgeConfig {
            pipe_name: format!(r"\\.\pipe\aperture.test.{name}.{}", std::process::id()),
            token_path: dir.join(format!("token-{name}-{}", std::process::id())),
        };
        (NmBridge::new(config.clone()), config)
    }

    fn foreground_chrome() -> Arc<Mutex<ForegroundContext>> {
        Arc::new(Mutex::new(ForegroundContext {
            identity: WindowIdentity {
                app: Some("Chrome".into()),
                process: Some("chrome.exe".into()),
                window_title: Some("Tab - Google Chrome".into()),
                window_class: None,
            },
            url: None,
        }))
    }

    #[tokio::test]
    async fn navigation_flows_pipe_to_bus_with_auth() {
        let (bridge, config) = test_bridge("nav");
        bridge.set_forwarding(true);
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let normalizer = Arc::new(Normalizer::new(bus, ExclusionList::shipped_defaults()));
        let _server = spawn_server(Arc::clone(&bridge), normalizer, foreground_chrome());

        // Give the accept loop a beat to create the first instance.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let token = bridge.ensure_token().unwrap();
        let mut client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&config.pipe_name)
            .expect("client connect");
        let hello = format!("{}\n", serde_json::json!({ "v": 1, "hello": { "token": token } }));
        client.write_all(hello.as_bytes()).await.unwrap();
        let nav = format!(
            "{}\n",
            serde_json::json!({ "kind": "navigation", "url": "https://docs.rs/tokio",
                                 "title": "tokio - Rust", "browser": "chrome" })
        );
        client.write_all(nav.as_bytes()).await.unwrap();

        let ev = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("event within 3s")
            .expect("event");
        assert_eq!(ev.r#type, EventType::Navigation);
        assert_eq!(ev.payload["url"], serde_json::json!("https://docs.rs/tokio"));
        assert_eq!(ev.payload["browser"], serde_json::json!("chrome"));
        // The bridge is now the extension-primary URL source for chrome.exe.
        assert_eq!(
            bridge.current_url("chrome.exe").as_deref(),
            Some("https://docs.rs/tokio")
        );
    }

    #[tokio::test]
    async fn bad_token_is_dropped_before_any_processing() {
        let (bridge, config) = test_bridge("auth");
        bridge.set_forwarding(true);
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let normalizer = Arc::new(Normalizer::new(bus, ExclusionList::shipped_defaults()));
        let _server = spawn_server(Arc::clone(&bridge), normalizer, foreground_chrome());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        bridge.ensure_token().unwrap();

        let mut client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&config.pipe_name)
            .expect("client connect");
        let hello = format!("{}\n", serde_json::json!({ "v": 1, "hello": { "token": "wrong" } }));
        client.write_all(hello.as_bytes()).await.unwrap();
        let nav = format!(
            "{}\n",
            serde_json::json!({ "kind": "navigation", "url": "https://example.com", "browser": "chrome" })
        );
        let _ = client.write_all(nav.as_bytes()).await;

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(rx.try_recv().is_err(), "unauthenticated messages never publish");
    }

    #[tokio::test]
    async fn toggle_off_drops_messages_and_notifies_the_host() {
        let (bridge, config) = test_bridge("toggle");
        bridge.set_forwarding(true);
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let normalizer = Arc::new(Normalizer::new(bus, ExclusionList::shipped_defaults()));
        let _server = spawn_server(Arc::clone(&bridge), normalizer, foreground_chrome());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let token = bridge.ensure_token().unwrap();

        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&config.pipe_name)
            .expect("client connect");
        let (read_half, mut write_half) = tokio::io::split(client);
        let mut host_rx = BufReader::new(read_half).lines();
        write_half
            .write_all(format!("{}\n", serde_json::json!({ "v": 1, "hello": { "token": token } })).as_bytes())
            .await
            .unwrap();
        // Initial state push after hello.
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), host_rx.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(first.contains("\"capturing\":true"));

        // OFF: host is told, and subsequent messages are dropped (FIX 2.1).
        bridge.set_forwarding(false);
        let off = tokio::time::timeout(std::time::Duration::from_secs(3), host_rx.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(off.contains("\"capturing\":false"));
        write_half
            .write_all(
                format!(
                    "{}\n",
                    serde_json::json!({ "kind": "navigation", "url": "https://example.com", "browser": "chrome" })
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(rx.try_recv().is_err(), "toggle OFF halts extension forwarding");
        assert_eq!(bridge.current_url("chrome.exe"), None, "OFF also drops caches");
    }

    #[tokio::test]
    async fn media_ticks_coalesce_and_url_pattern_exclusion_holds() {
        let (bridge, _config) = test_bridge("media");
        bridge.set_forwarding(true);
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let list = ExclusionList::compile(vec![ExclusionRule {
            url_pattern: Some(r"^https://www\.youtube\.com/watch\?v=secret".into()),
            label: "excluded video".into(),
            ..Default::default()
        }]);
        let normalizer = Normalizer::new(bus, list);
        let fg = foreground_chrome();

        let media = |video: &str, pos: f64| ExtMessage::MediaState {
            url: Some(format!("https://www.youtube.com/watch?v={video}")),
            video_id: video.to_string(),
            position_s: Some(pos),
            state: Some("playing".into()),
            title: Some("T".into()),
            browser: "chrome".into(),
            incognito: None,
        };

        // First tick publishes; a +5 s tick coalesces; a +20 s tick publishes.
        assert_eq!(bridge.handle_message(media("abcdef123456", 10.0), &normalizer, &fg), 1);
        assert_eq!(bridge.handle_message(media("abcdef123456", 15.0), &normalizer, &fg), 0);
        assert_eq!(bridge.handle_message(media("abcdef123456", 35.0), &normalizer, &fg), 1);
        let ev = rx.try_recv().expect("first publish");
        assert_eq!(ev.r#type, EventType::MediaState);
        assert_eq!(ev.payload["video_id"], serde_json::json!("abcdef123456"));
        let _second = rx.try_recv().expect("second publish");
        assert!(rx.try_recv().is_err());

        // An excluded URL is dropped entirely (FIX 2.2 — no payload, no row).
        assert_eq!(
            bridge.handle_message(media("secretvid001", 5.0), &normalizer, &fg),
            0
        );
        assert!(rx.try_recv().is_err(), "excluded media never publishes");
    }
}
