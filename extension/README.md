# Aperture Capture Bridge (browser extension, ADR-027)

One Manifest V3 codebase for **Chrome + Opera GX** (Edge fast-follow). It is the
*primary* source for two — and only two — capture signals (ADR-029):

- **`navigation`** — the active tab's URL + title (tabs API), and
- **`media_state`** — YouTube playback position (`video.currentTime` read by the
  content script; Doc 10 §3 rung 1).

Never page DOM/content. Incognito is excluded at the manifest level
(`"incognito": "not_allowed"`) and re-guarded in code.

## Transport (ADR-028)

`background.js` → `chrome.runtime.connectNative("com.aperture.bridge")` → the
**native-messaging host** (`aperture-nm-host.exe`, a bin target of
`crates/capture`) → Windows named pipe → the running Aperture core. stdio + a
named pipe: **no sockets anywhere on this path**, so the two-emitter rule stays
literally true. Extension-fed URLs traverse the same exclusion + redaction
pipeline as UIA-sourced ones inside the core (FIX 2.2).

The capture toggle propagates outward (FIX 2.1): toggle OFF → core signals the
host → host pushes `{type:"toggle", capturing:false}` → the worker drops
everything until re-enabled. Host/core absence also silences forwarding —
nothing is ever queued.

## Install (dev, unpacked)

1. `chrome://extensions` → Developer mode → *Load unpacked* → this directory.
   Note the extension ID.
2. Register the host manifest (fills `path` + `allowed_origins` from the
   template and writes the registry key):
   `cargo run -p aperture-capture --bin aperture-nm-host -- install --extension-id <ID>`
   (add `--browser opera` for Opera GX; repeat per browser).
3. Start Aperture; the worker connects on the next tab event.

Registry keys written (HKCU, per-user, no admin):
- Chrome: `HKCU\Software\Google\Chrome\NativeMessagingHosts\com.aperture.bridge`
- Opera GX uses Chrome's key path on Windows (Chromium default); Edge:
  `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.aperture.bridge`

`[VERIFY]` at first on-target install: both store extension IDs must appear in
`allowed_origins` before store publication (ADR-027c).
