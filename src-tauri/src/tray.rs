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
    let quit = MenuItem::with_id(app, "quit", "Quit Aperture", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &dashboard,
            &capture,
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
    let tray = builder
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "dashboard" => {
                let _ = events::emit_dashboard_open(app);
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
                    // never bypassed from the tray.
                    if !state.consent.lock().await.state().first_run_completed {
                        let _ = item.set_checked(false);
                        return;
                    }
                    if let Err(e) =
                        commands::toggle_capture(want, app.clone(), app.state()).await
                    {
                        tracing::error!(%e, "tray capture toggle failed");
                        // The indicator event reports the real state; also
                        // revert eagerly so the menu never lies while closed.
                        let _ = item.set_checked(!want);
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
                app.exit(0);
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
