//! System tray — the app's permanent, findable home.
//!
//! The overlay skips the taskbar and is click-through, so without a tray there
//! is no way to find, control, or quit Aperture. The tray therefore carries the
//! app's "window chrome": open the dashboard (left-click or menu), toggle
//! capture, toggle start-at-login, quit.
//!
//! Truthfulness: the capture checkmark mirrors the `capture_indicator` event —
//! the OBSERVED outcome the capture driver emits (doc 13 §8) — never the
//! requested state, so the tray can never disagree with the overlay dot.

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Listener, Manager,
};

use crate::app_state::AppState;
use crate::{commands, events};

/// Build the tray icon + menu. Called once from Tauri's setup.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let capture = CheckMenuItem::with_id(
        app,
        "capture",
        "Capture my screen",
        true,
        false, // starts OFF; the indicator listener below keeps it truthful
        None::<&str>,
    )?;
    let dashboard = MenuItem::with_id(app, "dashboard", "Open Dashboard", true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start when I sign in",
        true,
        commands::autostart_enabled(app),
        None::<&str>,
    )?;
    // 2026-09-06: the way back after the HUD orb's hide button — once the HUD
    // is hidden the overlay has no chrome of its own. Checked = visible. Seeded
    // from the stored `ui.hud_hidden`; the settings_changed listener below
    // keeps it truthful whichever side (orb or tray) flips the key.
    let show_hud = CheckMenuItem::with_id(
        app,
        "show_hud",
        "Show overlay controls",
        true,
        !commands::ui_flag(&app.state::<AppState>().db, "hud_hidden"),
        None::<&str>,
    )?;
    // v2 (Doc 22 §3.3, locked decision 5): the hard stop must reach the loop
    // even if the overlay is wedged — the tray runs on its own thread. Always
    // enabled: a stop with no task is a harmless no-op, and a greyed item
    // that lags the real state is worse than a click that does nothing.
    let stop_agent = MenuItem::with_id(app, "stop_agent", "Stop agent task", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Aperture", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &dashboard,
            &capture,
            &show_hud,
            &PredefinedMenuItem::separator(app)?,
            &stop_agent,
            &PredefinedMenuItem::separator(app)?,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("aperture-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Aperture — capture off");
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let capture_for_menu = capture.clone();
    let show_hud_for_menu = show_hud.clone();
    let tray = builder
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "dashboard" => {
                let _ = events::emit_dashboard_open(app);
            }
            "show_hud" => {
                // The check item flipped itself; is_checked() is what the user
                // now WANTS (checked = visible). Persist the inverse flag and
                // announce it — the overlay's Hud re-reads `ui` on the
                // broadcast and mounts / unmounts the orb.
                let want_visible = show_hud_for_menu.is_checked().unwrap_or(true);
                let state = app.state::<AppState>();
                match commands::persist_ui_key(
                    &state.db,
                    "hud_hidden",
                    serde_json::Value::Bool(!want_visible),
                ) {
                    Ok(()) => {
                        let _ = events::emit_settings_changed(app, vec!["ui".into()]);
                    }
                    Err(e) => {
                        tracing::error!(%e, "tray show-overlay-controls toggle failed");
                        let _ = show_hud_for_menu.set_checked(!want_visible);
                    }
                }
            }
            "capture" => {
                // The check item flipped itself on click; is_checked() is the
                // state the user now WANTS. Drive it through the same
                // consent-gated command the overlay toggle uses.
                let want = capture_for_menu.is_checked().unwrap_or(false);
                let item = capture_for_menu.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    // First-run must be answered in the overlay's consent flow,
                    // never bypassed from the tray. Don't just snap the box
                    // back (that read as "broken", 2026-08-15 review) — raise
                    // the overlay so the consent dialog is actually seen.
                    if !state.consent.lock().await.state().first_run_completed {
                        let _ = item.set_checked(false);
                        if let Some(w) = app.get_webview_window(crate::overlay::OVERLAY_LABEL) {
                            let _ = w.set_focus();
                        }
                        return;
                    }
                    if let Err(e) =
                        commands::toggle_capture(want, app.clone(), app.state()).await
                    {
                        tracing::error!(%e, "tray capture toggle failed");
                        // Every error path leaves capture OFF (an ON that
                        // failed never started; an OFF that failed to persist
                        // still stopped) — so `false`, not `!want`, is the
                        // truthful eager state; the indicator listener stays
                        // authoritative when the next event lands.
                        let _ = item.set_checked(false);
                    }
                });
            }
            "stop_agent" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    if crate::agent::hard_stop_current(&app, state.inner()).await.is_none() {
                        tracing::info!("tray: stop agent task — no task running");
                    }
                });
            }
            "autostart" => {
                let want = autostart.is_checked().unwrap_or(false);
                let item = autostart.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = commands::set_autostart(want, app.clone(), app.state()).await
                    {
                        tracing::error!(%e, "tray autostart toggle failed");
                        let _ = item.set_checked(!want);
                    }
                });
            }
            "quit" => {
                // Quit ≠ capture off: consent persists, so the next login's
                // autostart restores capture exactly as the user left it.
                //
                // Reap the GPU sidecars BEFORE exiting: Tauri's exit ends in
                // process::exit, which runs no Drop impls — kill_on_drop never
                // fired and llama/whisper survived holding VRAM (2026-08-15
                // review). The spawn-side Job Object is the hard backstop
                // (handle close on process death kills the tree); this makes
                // the normal path orderly and logged. Bounded so a wedged
                // lifecycle can never hold Quit hostage.
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let cleanup = async {
                        let state = app.state::<AppState>();
                        let lifecycle = state.orchestration.lock().await.lifecycle();
                        let mut lifecycle = lifecycle.lock().await;
                        if let Err(e) = lifecycle.kill_all_sidecars().await {
                            tracing::warn!(%e, "sidecar reap on quit failed (Job Object will finish it)");
                        }
                    };
                    if tokio::time::timeout(std::time::Duration::from_secs(3), cleanup)
                        .await
                        .is_err()
                    {
                        tracing::warn!("sidecar reap on quit timed out (Job Object will finish it)");
                    }
                    app.exit(0);
                });
            }
            other => tracing::warn!(id = other, "unknown tray menu id"),
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click = open the dashboard (menu stays on right-click).
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = events::emit_dashboard_open(tray.app_handle());
            }
        })
        .build(app)?;

    // Mirror the stored `ui.hud_hidden` onto the "Show overlay controls"
    // checkmark on every `ui` write — the orb's hide button lands here, so the
    // tray can never disagree with what the overlay shows.
    let app_for_hud = app.clone();
    app.listen_any(events::SETTINGS_CHANGED, move |event| {
        let Ok(p) = serde_json::from_str::<events::SettingsChangedPayload>(event.payload()) else {
            return;
        };
        if p.sections.iter().any(|s| s == "ui") {
            let hidden = commands::ui_flag(&app_for_hud.state::<AppState>().db, "hud_hidden");
            let _ = show_hud.set_checked(!hidden);
        }
    });

    // Mirror the observed capture state onto the checkmark + tooltip.
    app.listen_any(events::CAPTURE_INDICATOR, move |event| {
        if let Ok(p) = serde_json::from_str::<events::CaptureIndicatorPayload>(event.payload()) {
            let _ = capture.set_checked(p.capturing);
            let _ = tray.set_tooltip(Some(if p.capturing {
                "Aperture — capturing"
            } else {
                "Aperture — capture off"
            }));
        }
    });

    // Windows 11 buries brand-new tray icons in the overflow chevron — for an
    // app whose ONLY chrome is this icon, that reads as "it didn't start"
    // (user report, 2026-08-14). Explorer creates the icon's
    // NotifyIconSettings entry asynchronously after registration, so retry
    // briefly until it appears.
    #[cfg(windows)]
    tauri::async_runtime::spawn(async {
        for _ in 0..10 {
            if promote_tray_icon() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        tracing::warn!("tray icon NotifyIconSettings entry never appeared; not promoted");
    });

    Ok(())
}

/// Make this exe's tray icon visible on the taskbar (Win11 22H2+ overflow
/// model). Writes `IsPromoted=1` ONLY when the value does not exist yet — the
/// icon's first registration — so a user who later demotes it (Explorer writes
/// 0) is never overridden. Returns true once our entry was found and handled
/// (or promotion is impossible), false to ask the caller to retry.
#[cfg(windows)]
fn promote_tray_icon() -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;
    let Ok(exe) = std::env::current_exe() else {
        return true; // no exe path: nothing to match against, ever
    };
    let exe = exe.to_string_lossy().to_string();
    let root = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Control Panel\\NotifyIconSettings")
    {
        Ok(root) => root,
        // Pre-22H2 Windows has no per-icon promotion registry; nothing to do.
        Err(_) => return true,
    };
    for name in root.enum_keys().flatten() {
        let Ok(icon) = root.open_subkey_with_flags(&name, KEY_READ | KEY_SET_VALUE) else {
            continue;
        };
        let path: String = icon.get_value("ExecutablePath").unwrap_or_default();
        if path.eq_ignore_ascii_case(&exe) {
            if icon.get_raw_value("IsPromoted").is_err() {
                if let Err(e) = icon.set_value("IsPromoted", &1u32) {
                    tracing::warn!(%e, "tray icon promotion write failed");
                }
            }
            return true;
        }
    }
    false // our entry isn't there yet — Explorer may still be writing it
}
