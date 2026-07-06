// Aperture Capture Bridge — MV3 service worker (ADR-027).
//
// Forwards exactly two things to the local core via the native-messaging host
// (ADR-028, stdio — no sockets): the active-tab URL ("navigation") and the
// YouTube playback position relayed from the content script ("media_state").
// Never page DOM/content (ADR-029). Incognito never reaches this worker
// (manifest "incognito": "not_allowed") but is guarded again below anyway.
//
// The capture toggle propagates here (FIX 2.1): the host pushes
// {type:"toggle", capturing:false} and this worker drops everything until it
// flips back on. Host disconnect (core not running / capture OFF teardown)
// also stops forwarding — silence, then capped-backoff reconnect.

const HOST_NAME = "com.aperture.bridge";
const PROTOCOL_VERSION = 1;

const IS_OPERA = /\bOPR\//.test(navigator.userAgent);
const BROWSER_ID = IS_OPERA ? "opera" : "chrome";

let port = null;
let capturing = true; // host-controlled (FIX 2.1); default on until told otherwise
let reconnectDelayMs = 1000;
const RECONNECT_MAX_MS = 60_000;
let reconnectTimer = null;

function connect() {
  if (port) return;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
  } catch (_e) {
    port = null;
    scheduleReconnect();
    return;
  }
  port.onMessage.addListener(onHostMessage);
  port.onDisconnect.addListener(() => {
    port = null;
    scheduleReconnect();
  });
  reconnectDelayMs = 1000;
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, reconnectDelayMs);
  reconnectDelayMs = Math.min(reconnectDelayMs * 2, RECONNECT_MAX_MS);
}

function onHostMessage(msg) {
  if (msg && msg.type === "toggle") {
    capturing = !!msg.capturing;
  }
}

function send(payload) {
  if (!capturing) return;
  if (!port) {
    connect();
    if (!port) return; // drop — the core is not there; never queue user data
  }
  try {
    port.postMessage({ v: PROTOCOL_VERSION, browser: BROWSER_ID, ...payload });
  } catch (_e) {
    port = null;
    scheduleReconnect();
  }
}

function sendNavigation(tab) {
  if (!tab || tab.incognito) return;
  if (!tab.url || !/^https?:/.test(tab.url)) return; // no chrome://, file://, etc.
  send({
    kind: "navigation",
    url: tab.url,
    title: tab.title || "",
    ts_ms: Date.now(),
  });
}

async function sendActiveTab(windowId) {
  try {
    const query = { active: true };
    if (windowId !== undefined && windowId !== chrome.windows.WINDOW_ID_NONE) {
      query.windowId = windowId;
    } else {
      query.lastFocusedWindow = true;
    }
    const [tab] = await chrome.tabs.query(query);
    if (tab) sendNavigation(tab);
  } catch (_e) {
    /* window gone mid-query — nothing to report */
  }
}

chrome.tabs.onActivated.addListener(({ tabId }) => {
  chrome.tabs.get(tabId).then(sendNavigation, () => {});
});

chrome.tabs.onUpdated.addListener((_tabId, changeInfo, tab) => {
  // Only the active tab of a window matters (foreground context), and only on
  // a URL change or load completion — not every favicon/loading tick.
  if (!tab.active) return;
  if (changeInfo.url || changeInfo.status === "complete") sendNavigation(tab);
});

chrome.windows.onFocusChanged.addListener((windowId) => {
  if (windowId === chrome.windows.WINDOW_ID_NONE) return;
  sendActiveTab(windowId);
});

// Content-script relay (YouTube position — Doc 10 §3 rung 1).
chrome.runtime.onMessage.addListener((msg, sender) => {
  if (!msg || msg.kind !== "media_state") return;
  if (sender.tab && sender.tab.incognito) return;
  send({
    kind: "media_state",
    url: msg.url,
    video_id: msg.video_id,
    position_s: msg.position_s,
    state: msg.state,
    title: (sender.tab && sender.tab.title) || "",
    ts_ms: Date.now(),
  });
});

chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();
