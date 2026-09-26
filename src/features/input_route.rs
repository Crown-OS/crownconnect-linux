//! Where a peer's remote input goes: to a shared cursor on this screen, the virtual monitor it
//! drives, or the mirrored screen it controls. Injection only hands events to the injector's
//! own thread, so it runs on the runtime thread without delaying it.

use std::sync::{Arc, OnceLock};

use llts_signaling::device::DeviceId;
use llts_signaling::message::{UnicursorEnter, UnicursorLeave};

use super::input_wire::{input_frames, PressedKeys};
use super::platform::MediaPlatform;
use super::unicursor::RemoteCursor;
use crate::wayland::injector::{InjectedEvent, Injector, InputFrame};

/// An injector that becomes available once its compositor connection is up.
pub(crate) type SharedInjector = Arc<OnceLock<Injector>>;

/// Who a peer's input is for, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InputOwner {
    RemoteCursor,
    Monitor,
    Mirror,
}

/// Connects an injector on a short-lived thread, so nobody waits for the roundtrip.
pub(crate) fn connect_injector(
    platform: Arc<dyn MediaPlatform>,
    output: Option<String>,
) -> SharedInjector {
    let shared = SharedInjector::default();
    let slot = Arc::clone(&shared);
    let spawned = std::thread::Builder::new()
        .name("crownconnect-injector-connect".to_owned())
        .spawn(move || match platform.injector(output) {
            Ok(injector) => {
                let _ = slot.set(injector);
            }
            Err(error) => tracing::warn!(%error, "remote input stays off"),
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "remote input stays off");
    }
    shared
}

#[derive(Debug)]
struct InputRoute {
    peer: DeviceId,
    owner: InputOwner,
    injector: SharedInjector,
    keys: PressedKeys,
    cursor: Option<RemoteCursor>,
    /// Where a shared cursor that entered before the injector was ready still has to appear.
    pending_entry: Option<(f64, f64)>,
}

impl InputRoute {
    /// A shared-cursor route only takes input while the peer's cursor is on this screen.
    const fn accepts(&self) -> bool {
        !matches!(self.owner, InputOwner::RemoteCursor) || self.cursor.is_some()
    }

    fn inject(&self, frame: InputFrame) {
        if let Some(injector) = self.injector.get()
            && let Err(error) = injector.inject(frame)
        {
            tracing::debug!(%error, "cannot inject remote input");
        }
    }

    fn place_entered_cursor(&mut self) {
        if self.injector.get().is_some()
            && let Some((x, y)) = self.pending_entry.take()
        {
            self.inject(InputFrame {
                events: vec![InjectedEvent::PointerTo { x, y }],
                time_us: 0,
            });
        }
    }

    /// Injects a batch; the handover when a shared cursor crossed back to the peer on the way.
    fn deliver(&mut self, batch: &[u8]) -> Option<UnicursorLeave> {
        self.place_entered_cursor();
        let size = self.injector.get().and_then(Injector::target_size);
        for frame in input_frames(batch) {
            let mut kept = Vec::with_capacity(frame.events.len());
            for event in frame.events {
                if let InjectedEvent::Key { code, pressed } = event {
                    self.keys.note(code, pressed);
                }
                if let (Some(cursor), InjectedEvent::PointerMotion { dx, dy }) =
                    (&mut self.cursor, event)
                    && let Some(leave) = cursor.moved((dx, dy), size)
                {
                    self.inject(InputFrame {
                        events: kept,
                        time_us: frame.time_us,
                    });
                    self.leave_screen();
                    return Some(leave);
                }
                kept.push(event);
            }
            self.inject(InputFrame {
                events: kept,
                time_us: frame.time_us,
            });
        }
        None
    }

    /// The peer's cursor went back: nothing it held stays pressed here.
    fn leave_screen(&mut self) {
        self.cursor = None;
        self.release_all();
    }

    fn release_all(&mut self) {
        self.adopt_keys(&[]);
    }

    /// Adopts the peer's held keys and releases the ones it let go of.
    fn adopt_keys(&mut self, snapshot: &[u8]) {
        let released = self.keys.adopt(snapshot);
        if !released.is_empty() {
            self.inject(InputFrame {
                events: released
                    .into_iter()
                    .map(|code| InjectedEvent::Key {
                        code,
                        pressed: false,
                    })
                    .collect(),
                time_us: 0,
            });
        }
    }
}

/// Every peer's input destination.
#[derive(Debug, Default)]
pub(crate) struct InputRoutes {
    routes: Vec<InputRoute>,
}

impl InputRoutes {
    pub(crate) fn add(&mut self, peer: DeviceId, owner: InputOwner, injector: SharedInjector) {
        self.remove(peer, owner);
        self.routes.push(InputRoute {
            peer,
            owner,
            injector,
            keys: PressedKeys::default(),
            cursor: None,
            pending_entry: None,
        });
    }

    pub(crate) fn contains(&self, peer: DeviceId, owner: InputOwner) -> bool {
        self.routes
            .iter()
            .any(|route| route.peer == peer && route.owner == owner)
    }

    pub(crate) fn remove(&mut self, peer: DeviceId, owner: InputOwner) {
        for route in self
            .routes
            .iter_mut()
            .filter(|route| route.peer == peer && route.owner == owner)
        {
            route.release_all();
        }
        self.routes
            .retain(|route| !(route.peer == peer && route.owner == owner));
    }

    pub(crate) fn remove_peer(&mut self, peer: DeviceId) {
        for route in self.routes.iter_mut().filter(|route| route.peer == peer) {
            route.release_all();
        }
        self.routes.retain(|route| route.peer != peer);
    }

    fn target(&mut self, peer: DeviceId) -> Option<&mut InputRoute> {
        self.routes
            .iter_mut()
            .filter(|route| route.peer == peer && route.accepts())
            .min_by_key(|route| route.owner)
    }

    /// Injects a batch from `peer`; the handover to send when its cursor went back.
    pub(crate) fn deliver(&mut self, peer: DeviceId, batch: &[u8]) -> Option<UnicursorLeave> {
        self.target(peer)?.deliver(batch)
    }

    /// Releases the keys `peer` no longer holds, as its key-state snapshot says.
    pub(crate) fn key_state(&mut self, peer: DeviceId, snapshot: &[u8]) {
        if let Some(route) = self.target(peer) {
            route.adopt_keys(snapshot);
        }
    }

    /// The peer's cursor crossed onto this screen; it appears at the facing edge.
    pub(crate) fn cursor_entered(&mut self, peer: DeviceId, enter: UnicursorEnter) {
        let Some(route) = self
            .routes
            .iter_mut()
            .find(|route| route.peer == peer && route.owner == InputOwner::RemoteCursor)
        else {
            return;
        };
        let cursor = RemoteCursor::entering(enter);
        route.pending_entry = Some(cursor.entry_point());
        route.cursor = Some(cursor);
        route.place_entered_cursor();
    }
}
