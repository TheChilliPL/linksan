pub mod tray;
pub mod clipboard;

use crate::tray::TrayState;
use notify_rust::Urgency;
use std::time::Duration;
use tracing::debug;
use tracing_subscriber::EnvFilter;
use crate::clipboard::ClipboardSanitizer;

#[derive(Debug)]
pub enum MainThreadMessage {
    TrayIconClicked,
    OpenConfigClicked,
    ExitClicked,
}

fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();
    color_eyre::install().ok();

    let mut is_on = true;

    let (tx, rx) = std::sync::mpsc::channel::<MainThreadMessage>();

    let mut tray = TrayState::new()?;
    tray.set_event_handler(tx.clone())?;

    let mut clipboard_sanitizer = Some(ClipboardSanitizer::register()?);

    while let Ok(event) = rx.recv() {
        debug!("Event received: {:?}", event);

        match event {
            MainThreadMessage::TrayIconClicked => {
                is_on = !is_on;
                tray.refresh_icon(is_on)?;
                clipboard_sanitizer = if is_on { Some(ClipboardSanitizer::register()?) } else { None };
                let _ = notify_rust::Notification::new()
                    .summary(&format!("LinkSan has been turned {}!", if is_on { "on" } else { "off" }))
                    .urgency(Urgency::Low)
                    .timeout(Duration::from_secs(2))
                    .show();
            }
            MainThreadMessage::OpenConfigClicked => {
                showfile::show_path_in_file_manager("/home/chilli/Downloads/2026-09/Gay Brother Ringtone.mp3");
            }
            MainThreadMessage::ExitClicked => {
                break;
            }
        }
    }

    Ok(())
}
