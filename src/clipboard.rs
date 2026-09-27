use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::linux::raw::stat;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use notify_rust::{Notification, Urgency};
use tracing::{debug, error, info, trace, warn};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{self, ExtDataControlDeviceV1};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{self, ExtDataControlOfferV1};
use url::Url;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{Event, ExtDataControlSourceV1};

static UTF8_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

struct State {
    pending_offers: HashMap<ObjectId, Vec<String>>,
    unhandled: Option<UnixStream>,
    owned_clipboard: Option<String>,
    ignore_next: bool,
    closing: Arc<AtomicBool>,
}

impl State {
    fn new(closing_token: Arc<AtomicBool>) -> Self {
        Self {
            pending_offers: HashMap::with_capacity(2),
            unhandled: None,
            owned_clipboard: None,
            ignore_next: false,
            closing: closing_token,
        }
    }

    fn closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }
}

macro_rules! dispatch {
    ($state:ident, $proxy:ident, $data:ty) => {
        impl Dispatch<$proxy, $data> for $state {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as Proxy>::Event,
                _: &$data,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {}
        }
    };
    ($state:ident, $proxy:ident) => {
        dispatch!($state, $proxy, ());
    }
}

dispatch!(State, WlRegistry, GlobalListContents);
dispatch!(State, ExtDataControlManagerV1);
dispatch!(State, WlSeat);

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: <ExtDataControlDeviceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_data_control_device_v1::Event;
        match event {
            Event::DataOffer { id: offer } => {
                if state.ignore_next || state.closing() { return; }
                state.pending_offers.entry(offer.id()).or_insert_with(|| Vec::with_capacity(8));
            }
            Event::PrimarySelection { id: Some(offer) } => {
                // We ignore primary selection
                state.pending_offers.remove(&offer.id());
            }
            Event::Selection { id: Some(offer) } => {
                if state.ignore_next || state.closing() { state.ignore_next = false; return; }
                let Some(offers) = state.pending_offers.remove(&offer.id()) else {
                    error!(id = ?offer.id(), "No offers found for selection!");
                    return;
                };

                debug!("Found new selection with offers: {offers:?}");

                let Some(mime) = ClipboardSanitizer::choose_mime(&offers) else {
                    debug!(?offers, "No supported MIME found");
                    return;
                };

                debug!("Found compatible MIME: {mime}");

                let (tx, rx) = match UnixStream::pair() {
                    Ok(pair) => pair,
                    Err(e) => {
                        error!("Couldn't open UNIX stream pair: {e:?}");
                        return;
                    }
                };

                offer.receive(mime.to_owned(), tx.as_fd());

                if state.unhandled.replace(rx).is_some() {
                    warn!("Overwritten unhandled clipboard data");
                }
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(
        State,
        ExtDataControlDeviceV1,
        [
            ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ())
        ]
    );
}

impl Dispatch<ExtDataControlOfferV1, ()> for State {
    fn event(state: &mut Self, proxy: &ExtDataControlOfferV1, event: <ExtDataControlOfferV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if state.ignore_next || state.closing() { return; }
        use ext_data_control_offer_v1::Event;
        match event {
            Event::Offer { mime_type } => {
                state.pending_offers.entry(proxy.id())
                    .or_insert_with(|| {
                        warn!(id = ?proxy.id(), "Offer not properly initialized!");
                        Vec::with_capacity(8)
                    })
                    .push(mime_type);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(state: &mut Self, proxy: &ExtDataControlSourceV1, event: <ExtDataControlSourceV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use ext_data_control_source_v1::Event::*;
        match event {
            Send { mime_type, fd } => {
                if !UTF8_MIME_TYPES.iter().any(|it| *it == &mime_type) {
                    error!(mime_type, ?fd, "Requested invalid MIME type");
                    return;
                }

                let Some(str) = &state.owned_clipboard else {
                    error!(?fd, "Owned clipboard data not found");
                    return;
                };

                match File::from(fd).write_all(str.as_bytes()) {
                    Ok(_) => {
                        trace!(mime_type, "Clipboard data shared");
                    }
                    Err(e) => {
                        error!("Error writing clipboard data: {e:?}");
                    }
                }
            }
            Cancelled => {
                trace!("Destroying owned clipboard data and source");
                state.owned_clipboard = None;
                proxy.destroy();
            }
            _ => {}
        }
    }
}

pub struct ClipboardSanitizer {
    #[expect(unused)]
    thread: JoinHandle<()>,
    closing: Arc<AtomicBool>,
}

impl ClipboardSanitizer {
    pub fn register() -> eyre::Result<Self> {
        let closing = Arc::new(AtomicBool::new(false));

        let conn = Connection::connect_to_env()?;

        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();

        let manager = globals.bind::<ExtDataControlManagerV1, _, _>(&qh, 1..=1, ())?;
        trace!(?manager);

        let seat = globals.bind::<WlSeat, _, _>(&qh, 1..=1, ())?;
        trace!(?seat);

        let device = manager.get_data_device(&seat, &qh, ());

        let token = closing.clone();
        let thread = std::thread::spawn(move || {
            info!("Wayland thread started");
            let mut state = State::new(token);
            while !state.closing() {
                queue.blocking_dispatch(&mut state).unwrap();
                if let Some(mut rx) = state.unhandled.take() {
                    queue.flush().unwrap();
                    let mut str = String::with_capacity(256);
                    if let Err(e) = rx.read_to_string(&mut str) {
                        warn!("Error reading clipboard data: {e:?}");
                        continue;
                    }
                    debug!("Read clipboard data: {str}");
                    let mut url = match Url::parse(&str) {
                        Ok(url) => url,
                        Err(e) => {
                            debug!("Couldn't parse clipboard data as URL: {e:?}");
                            continue;
                        }
                    };

                    let tracking_query_keys = &[
                        "utm_source", "utm_medium", "utm_term", "utm_content", "utm_campaign", "si",
                    ];

                    let query = url.query_pairs().map(|(a,b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
                    let mut new_query = url.query_pairs_mut();
                    new_query.clear();

                    let mut removed_keys = Vec::with_capacity(4);

                    for (key, value) in query {
                        if tracking_query_keys.iter().any(|it| it == &key) {
                            removed_keys.push(key);
                        } else {
                            new_query.append_pair(&key, &value);
                        }
                    }

                    drop(new_query);

                    // Remove trailing ?
                    if url.query() == Some("") { url.set_query(None); }

                    if removed_keys.is_empty() {
                        debug!("No tracking query params found.");
                        continue;
                    }

                    let url_str = url.to_string();
                    info!(url_str, "Found and removed tracking query params: {removed_keys:?}");

                    let source = manager.create_data_source(&qh, ());
                    for mime_type in UTF8_MIME_TYPES {
                        source.offer(mime_type.to_string());
                    }

                    state.ignore_next = true;
                    state.owned_clipboard = Some(url_str);
                    device.set_selection(Some(&source));

                    let domain_url = url.domain().map_or_else(|| "URL".to_string(), |d| format!("{d} URL"));

                    let _ = Notification::new()
                        .summary("Link sanitized!")
                        .body(&format!("Your {domain_url} has been sanitized! Removed parameters: {}", removed_keys.join(", ")))
                        .urgency(Urgency::Low)
                        .timeout(Duration::from_secs(3))
                        .show();
                }
            }
        });

        Ok(Self { thread, closing })
    }

    pub fn choose_mime(mime_types: &[String]) -> Option<&str> {
        UTF8_MIME_TYPES
            .iter()
            .copied()
            .find(|mime| mime_types.iter().any(|it| it == *mime))
    }

    pub fn close(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
    }
}


impl Drop for ClipboardSanitizer {
    fn drop(&mut self) {
        self.close();
    }
}
