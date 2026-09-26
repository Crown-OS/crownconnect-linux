//! Makes a peer's clipboard this computer's selection through ext-data-control, since the
//! crownos-clipboard service can only re-select entries it captured itself.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self, ExtDataControlOfferV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self, ExtDataControlSourceV1,
};

use super::worker::{WorkerHandle, WorkerState};
use super::WaylandError;

const TEXT_ALIASES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Owns this computer's selection on behalf of paired devices.
#[derive(Debug)]
pub struct ClipboardSetter {
    worker: WorkerHandle<Selection>,
}

#[derive(Debug)]
struct Selection {
    mime: String,
    bytes: Arc<[u8]>,
}

impl ClipboardSetter {
    /// # Errors
    ///
    /// Fails without a compositor offering ext-data-control and a seat.
    pub async fn connect() -> Result<Self, WaylandError> {
        let worker = WorkerHandle::spawn("crownconnect-clipboard", SetterState::bind).await?;
        Ok(Self { worker })
    }

    /// Offers `bytes` as the selection until something else is copied.
    ///
    /// # Errors
    ///
    /// Fails once the wayland thread has stopped.
    pub fn set(&self, mime: &str, bytes: Arc<[u8]>) -> Result<(), WaylandError> {
        self.worker.send(Selection {
            mime: mime.to_owned(),
            bytes,
        })
    }
}

/// The MIME types to offer for a clipboard of type `mime`: text is also offered under the
/// legacy X11 names, which many applications still ask for first.
fn offered_mimes(mime: &str) -> Vec<&str> {
    let mut offered = vec![mime];
    if mime.starts_with("text/plain") {
        offered.extend(TEXT_ALIASES.into_iter().filter(|alias| *alias != mime));
    }
    offered
}

struct SetterState {
    manager: ExtDataControlManagerV1,
    device: ExtDataControlDeviceV1,
    source: Option<(ExtDataControlSourceV1, Arc<[u8]>)>,
    latest_offer: Option<ExtDataControlOfferV1>,
    finished: bool,
}

impl SetterState {
    fn bind(globals: &GlobalList, queue: &QueueHandle<Self>) -> Result<Self, WaylandError> {
        let manager: ExtDataControlManagerV1 = globals.bind(queue, 1..=1, ())?;
        let seat: WlSeat = globals.bind(queue, 1..=9, ())?;
        let device = manager.get_data_device(&seat, queue, ());
        Ok(Self {
            manager,
            device,
            source: None,
            latest_offer: None,
            finished: false,
        })
    }

    fn drop_source(&mut self) {
        if let Some((source, _)) = self.source.take() {
            source.destroy();
        }
    }
}

impl WorkerState for SetterState {
    type Command = Selection;

    fn apply(&mut self, selection: Selection, queue: &QueueHandle<Self>) {
        let source = self.manager.create_data_source(queue, ());
        for mime in offered_mimes(&selection.mime) {
            source.offer(mime.to_owned());
        }
        self.device.set_selection(Some(&source));
        self.drop_source();
        self.source = Some((source, selection.bytes));
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

fn send_bytes(fd: OwnedFd, bytes: Arc<[u8]>) {
    let spawned = std::thread::Builder::new()
        .name("crownconnect-clipboard-send".into())
        .spawn(move || {
            let _ = std::fs::File::from(fd).write_all(&bytes);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "cannot hand the clipboard to a reader");
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for SetterState {
    fn event(
        state: &mut Self,
        source: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let is_current = state
            .source
            .as_ref()
            .is_some_and(|(current, _)| current == source);
        match event {
            ext_data_control_source_v1::Event::Send { fd, .. } => {
                if let Some((_, bytes)) = state.source.as_ref().filter(|_| is_current) {
                    send_bytes(fd, Arc::clone(bytes));
                }
            }
            ext_data_control_source_v1::Event::Cancelled if is_current => state.drop_source(),
            ext_data_control_source_v1::Event::Cancelled => source.destroy(),
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for SetterState {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::DataOffer { id } => {
                if let Some(previous) = state.latest_offer.replace(id) {
                    previous.destroy();
                }
            }
            ext_data_control_device_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(SetterState, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for SetterState {
    fn event(
        _: &mut Self,
        _: &ExtDataControlOfferV1,
        _: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for SetterState {
    fn event(
        _: &mut Self,
        _: &ExtDataControlManagerV1,
        _: <ExtDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for SetterState {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for SetterState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_offered_under_its_legacy_names_too() {
        let offered = offered_mimes("text/plain;charset=utf-8");
        assert_eq!(offered.first(), Some(&"text/plain;charset=utf-8"));
        assert_eq!(offered.len(), TEXT_ALIASES.len());
        assert!(offered.contains(&"UTF8_STRING"));
    }

    #[test]
    fn other_types_are_offered_as_they_are() {
        assert_eq!(offered_mimes("image/png"), ["image/png"]);
    }
}
