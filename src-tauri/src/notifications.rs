//! OS notifications for operations that stop on a prompt while their
//! session's window isn't focused, so nobody is watching it wait.

use tauri::{AppHandle, Manager, UserAttentionType, WebviewWindow};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast;

use crate::main_window::Prompt;
use crate::preferences::PreferencesHandle;

/// The window a notification is about, carried through a click.
const WINDOW: &str = "window";

/// A click on a notification brings its window forward. Registered once,
/// before any is shown: the plugin reports clicks only on notifications
/// shown after a handler exists.
pub fn init(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.notification().on_action(move |performed| {
        let window = performed
            .notification()
            .and_then(|n| n.extra().get(WINDOW))
            .and_then(|label| label.as_str())
            .and_then(|label| handle.get_webview_window(label));
        if let Some(window) = window {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    });
}

/// Notify of `window`'s session's prompts until the session ends.
pub async fn notify_prompts(
    window: WebviewWindow,
    mut prompts: broadcast::Receiver<Prompt>,
    preferences: PreferencesHandle,
) {
    loop {
        let prompt = match prompts.recv().await {
            Ok(prompt) => prompt,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if !preferences.load().behavior.notify_on_prompt || window.is_focused().unwrap_or(true) {
            continue;
        }
        let _ = window.request_user_attention(Some(UserAttentionType::Informational));
        let shown = window
            .app_handle()
            .notification()
            .builder()
            .title(prompt.description)
            .body(prompt.message)
            .extra(WINDOW, window.label())
            .show();
        if let Err(e) = shown {
            log::warn!("notifying of a prompt: {e}");
        }
    }
}
