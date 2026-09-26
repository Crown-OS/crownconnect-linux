//! Outputs with no display behind them (crownos-virtual-output-v1), paced by the consumer: the
//! compositor renders one only when asked with [`VirtualOutput::request_frame`].

use crownos_protocols::virtual_output::v1::client::crownos_virtual_output_manager_v1::{
    self, CrownosVirtualOutputManagerV1,
};
use crownos_protocols::virtual_output::v1::client::crownos_virtual_output_v1::{
    self, CloseReason, CrownosVirtualOutputV1,
};
use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

use super::worker::{WorkerHandle, WorkerState};
use super::{EventOutlet, WaylandError};

const MAX_NAME_LEN: usize = 32;
const MAX_DIMENSION: u32 = 16_384;
const SCALE_RANGE: std::ops::RangeInclusive<u32> = 60..=480;

/// A virtual output's mode, in the protocol's units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualOutputMode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    /// Scale in 120ths: 120 is 1x, 180 is 1.5x.
    pub scale_120: u32,
}

impl VirtualOutputMode {
    pub fn is_valid(&self) -> bool {
        (1..=MAX_DIMENSION).contains(&self.width)
            && (1..=MAX_DIMENSION).contains(&self.height)
            && self.refresh_mhz > 0
            && SCALE_RANGE.contains(&self.scale_120)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCloseCause {
    NameTaken,
    Revoked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtualOutputEvent {
    /// The output exists under this full name, which screencast and injection look it up by.
    Created {
        name: String,
    },
    Closed(OutputCloseCause),
}

/// Whether the compositor accepts `name`: 1 to 32 characters from `[A-Za-z0-9_-]`.
pub fn is_valid_output_name(name: &str) -> bool {
    (1..=MAX_NAME_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[derive(Debug)]
enum OutputCommand {
    SetMode(VirtualOutputMode),
    RequestFrame,
}

#[derive(Debug)]
pub struct VirtualOutput {
    worker: WorkerHandle<OutputCommand>,
}

impl VirtualOutput {
    /// Creates the output and reports what becomes of it to `events`. Blocks for one roundtrip,
    /// so call it from a pipeline thread. Dropping the handle destroys the output.
    ///
    /// # Errors
    ///
    /// Fails for a name or mode the compositor would reject, or without the protocol.
    pub fn create(
        name: &str,
        mode: VirtualOutputMode,
        events: EventOutlet<VirtualOutputEvent>,
    ) -> Result<Self, WaylandError> {
        if !is_valid_output_name(name) {
            return Err(WaylandError::InvalidArgument("virtual output name"));
        }
        if !mode.is_valid() {
            return Err(WaylandError::InvalidArgument("virtual output mode"));
        }
        let name = name.to_owned();
        let worker =
            WorkerHandle::spawn_blocking("crownconnect-virtual-output", move |globals, queue| {
                OutputState::create(globals, queue, name, mode, events)
            })?;
        Ok(Self { worker })
    }

    /// # Errors
    ///
    /// Fails for an out-of-range mode or once the output's thread has stopped.
    pub fn set_mode(&self, mode: VirtualOutputMode) -> Result<(), WaylandError> {
        if !mode.is_valid() {
            return Err(WaylandError::InvalidArgument("virtual output mode"));
        }
        self.worker.send(OutputCommand::SetMode(mode))
    }

    /// Advances the output by one frame.
    ///
    /// # Errors
    ///
    /// Fails once the output's thread has stopped.
    pub fn request_frame(&self) -> Result<(), WaylandError> {
        self.worker.send(OutputCommand::RequestFrame)
    }
}

struct OutputState {
    output: CrownosVirtualOutputV1,
    events: EventOutlet<VirtualOutputEvent>,
    closed: bool,
}

impl OutputState {
    fn create(
        globals: &GlobalList,
        queue: &QueueHandle<Self>,
        name: String,
        mode: VirtualOutputMode,
        events: EventOutlet<VirtualOutputEvent>,
    ) -> Result<Self, WaylandError> {
        let manager: CrownosVirtualOutputManagerV1 = globals.bind(queue, 1..=1, ())?;
        let output = manager.create_output(
            name,
            dimension(mode.width),
            dimension(mode.height),
            mode.refresh_mhz,
            mode.scale_120,
            queue,
            (),
        );
        Ok(Self {
            output,
            events,
            closed: false,
        })
    }
}

fn dimension(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

impl WorkerState for OutputState {
    type Command = OutputCommand;

    fn apply(&mut self, command: OutputCommand, _: &QueueHandle<Self>) {
        match command {
            OutputCommand::SetMode(mode) => self.output.set_mode(
                dimension(mode.width),
                dimension(mode.height),
                mode.refresh_mhz,
                mode.scale_120,
            ),
            OutputCommand::RequestFrame => self.output.request_frame(),
        }
    }

    fn is_finished(&self) -> bool {
        self.closed
    }
}

impl Drop for OutputState {
    fn drop(&mut self) {
        self.output.destroy();
    }
}

const fn close_cause(reason: WEnum<CloseReason>) -> OutputCloseCause {
    match reason {
        WEnum::Value(CloseReason::NameTaken) => OutputCloseCause::NameTaken,
        WEnum::Value(CloseReason::Revoked) => OutputCloseCause::Revoked,
        _ => OutputCloseCause::Failed,
    }
}

impl Dispatch<CrownosVirtualOutputV1, ()> for OutputState {
    fn event(
        state: &mut Self,
        _: &CrownosVirtualOutputV1,
        event: crownos_virtual_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let event = match event {
            crownos_virtual_output_v1::Event::Created { name } => {
                VirtualOutputEvent::Created { name }
            }
            crownos_virtual_output_v1::Event::Closed { reason } => {
                state.closed = true;
                VirtualOutputEvent::Closed(close_cause(reason))
            }
            _ => return,
        };
        let _ = (state.events)(event);
    }
}

impl Dispatch<CrownosVirtualOutputManagerV1, ()> for OutputState {
    fn event(
        _: &mut Self,
        _: &CrownosVirtualOutputManagerV1,
        _: crownos_virtual_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for OutputState {
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

    const MODE: VirtualOutputMode = VirtualOutputMode {
        width: 2560,
        height: 1600,
        refresh_mhz: 60_000,
        scale_120: 180,
    };

    #[test]
    fn names_follow_the_protocol_alphabet() {
        assert!(is_valid_output_name("crownconnect-tab_1"));
        assert!(!is_valid_output_name(""));
        assert!(!is_valid_output_name("has space"));
        assert!(!is_valid_output_name(&"x".repeat(33)));
    }

    #[test]
    fn modes_are_checked_against_the_protocol_ranges() {
        assert!(MODE.is_valid());
        assert!(!VirtualOutputMode { width: 0, ..MODE }.is_valid());
        assert!(!VirtualOutputMode {
            refresh_mhz: 0,
            ..MODE
        }
        .is_valid());
        assert!(!VirtualOutputMode {
            scale_120: 500,
            ..MODE
        }
        .is_valid());
        assert!(!VirtualOutputMode {
            height: 16_385,
            ..MODE
        }
        .is_valid());
    }

    #[test]
    fn unknown_close_reasons_count_as_failures() {
        assert_eq!(close_cause(WEnum::Unknown(9)), OutputCloseCause::Failed);
        assert_eq!(
            close_cause(WEnum::Value(CloseReason::NameTaken)),
            OutputCloseCause::NameTaken
        );
    }
}
