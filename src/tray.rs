use std::sync::mpsc::Sender;
use image::ImageFormat;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use crate::MainThreadMessage;

pub static ICON_ON: &[u8] = include_bytes!("../resources/icon_on.png");
pub static ICON_OFF: &[u8] = include_bytes!("../resources/icon_off.png");

pub struct TrayState {
    icon_on: Icon,
    icon_off: Icon,
    tray_icon: TrayIcon,
}

impl TrayState {
    pub fn new() -> eyre::Result<Self> {
        let icon_on = Self::load_icon(ICON_ON)?;
        let icon_off = Self::load_icon(ICON_OFF)?;

        let context_menu = Menu::with_id_and_items(
            "linksan",
            &[
                // &MenuItem::with_id("open_config", "Open config", true, None),
                &MenuItem::with_id("exit", "Exit", true, None),
            ]
        )?;

        let tray_icon = TrayIconBuilder::new()
            .with_tooltip("LinkSan")
            .with_icon(icon_on.clone())
            .with_menu(Box::new(context_menu))
            .build()?;

        Ok(Self {
            icon_on,
            icon_off,
            tray_icon,
        })
    }

    pub fn load_icon(png_data: &[u8]) -> eyre::Result<Icon> {
        let image = image::load_from_memory_with_format(png_data, ImageFormat::Png)?;
        let rgba = image.into_rgba8();
        let (w, h) = rgba.dimensions();
        Ok(Icon::from_rgba(rgba.into_raw(), w, h)?)
    }

    pub fn refresh_icon(&mut self, enabled: bool) -> eyre::Result<()> {
        self.tray_icon.set_icon(Some(if enabled { self.icon_on.clone() } else { self.icon_off.clone() }))?;
        Ok(())
    }

    pub fn set_event_handler(&mut self, tx: Sender<MainThreadMessage>) -> eyre::Result<()> {
        let tx2 = tx.clone();

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id();
            if id == "open_config" {
                let _ = tx.send(MainThreadMessage::OpenConfigClicked);
            } else if id == "exit" {
                let _ = tx.send(MainThreadMessage::ExitClicked);
            }
        }));

        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            match event {
                TrayIconEvent::Click { .. } => { let _ = tx2.send(MainThreadMessage::TrayIconClicked); }
                _ => {}
            }
        }));

        Ok(())
    }
}
