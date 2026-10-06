// Global chat shortcut: Ctrl+Shift+Space opens the island on the chat from any
// app. It goes through the same "tray" event as the tray menu, so the island
// handles it like any other way of opening it. Can be switched off in Settings.

use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::island::WINDOW_LABEL;
use crate::log;

fn chat_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
}

pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() == ShortcutState::Pressed && *shortcut == chat_shortcut() {
                let _ = app.emit_to(WINDOW_LABEL, "tray", "chat".to_string());
            }
        })
        .build()
}

/// Registers or releases the shortcut. Another app may already own the combo:
/// that is logged, never fatal.
pub fn apply(app: &AppHandle, enabled: bool) {
    let manager = app.global_shortcut();
    let shortcut = chat_shortcut();
    let registered = manager.is_registered(shortcut);
    let result = match (enabled, registered) {
        (true, false) => manager.register(shortcut),
        (false, true) => manager.unregister(shortcut),
        _ => Ok(()),
    };
    if let Err(err) = result {
        log::line(&format!("chat shortcut: {err}"));
    }
}
