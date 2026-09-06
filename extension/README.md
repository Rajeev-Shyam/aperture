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

## Install (unpacked — the only step is yours)

The extension's ID is **pinned**: `manifest.json` carries a `key`, so an
unpacked load on any machine gets the same ID
(`gkfkhokbibedjcgepmaakaelhdoboomj`, `nm_bridge::EXTENSION_ID`). Aperture
registers the native-messaging host for that ID **on every launch** (host
manifest under `%LOCALAPPDATA%\Aperture\nm\`, HKCU keys for Chrome — which
Opera / Opera GX / Brave read — and Edge; per-user, no admin). So:

1. Start Aperture once (any launch registers the host).
2. `opera://extensions` (or `chrome://extensions`, `edge://extensions`) →
   Developer mode → *Load unpacked* → this directory. Installed:
   `%LOCALAPPDATA%\Aperture\extension\`; dev: the checkout's `extension/`.
3. The worker connects on the next tab event. Dashboard → Advanced → the
   diagnostics block shows "Browser extension: connected".

Manual / repair registration (dev, or a different extension ID):
`cargo run -p aperture-capture --bin aperture-nm-host -- install [--extension-id <ID>] [--browser chrome|opera|edge]`
— the same `nm_bridge::install_host_manifest` call the app makes.

Why this matters: Opera exposes no UI Automation tree (verified on-target
2026-09-06), so the address-bar fallback can never read it — the extension is
the only URL source for Opera, and without URLs there are no "resume this
page" bubbles.

Regenerating the key (only if it leaks): `openssl genrsa 2048 | openssl rsa
-pubout -outform DER`, base64 → `key`; ID = first 16 bytes of SHA-256(DER) as
hex with `0-9a-f` → `a-p`; update `EXTENSION_ID` in the same change.

`[VERIFY]` at store publication: the store-assigned IDs must be added to
`allowed_origins` (ADR-027c).
