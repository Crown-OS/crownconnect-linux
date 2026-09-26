use rustix::time::{clock_gettime, ClockId};
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::{
    self, ZwpLinuxDmabufV1,
};
use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

use super::buffers::BufferKey;
use crate::media::present::input::InputMessage;
use crate::media::video::DrmFourcc;
use crate::util::latency::LatencyWindow;

const CLOCK_MONOTONIC: u32 = 1;
const LATENCY_SAMPLES: usize = 600;

/// When a frame was handed to the compositor, on CLOCK_MONOTONIC.
#[derive(Debug, Clone, Copy)]
pub(super) struct SubmittedAt(pub(super) u64);

impl SubmittedAt {
    pub(super) fn now() -> Self {
        Self(monotonic_ns())
    }
}

fn monotonic_ns() -> u64 {
    let now = clock_gettime(ClockId::Monotonic);
    u64::try_from(now.tv_sec).unwrap_or_default() * 1_000_000_000
        + u64::try_from(now.tv_nsec).unwrap_or_default()
}

#[derive(Debug)]
pub(super) struct State {
    pub(super) configured: bool,
    pub(super) closed: bool,
    pub(super) window_size: (i32, i32),
    pub(super) content_size: (i32, i32),
    pub(super) nv12_modifiers: Vec<u64>,
    pub(super) released: Vec<BufferKey>,
    pub(super) input: Vec<InputMessage>,
    pub(super) scroll_v120: (i32, i32),
    pub(super) presentation_clock: Option<u32>,
    pub(super) submit_to_present: LatencyWindow,
    pub(super) presented: u64,
    pub(super) discarded: u64,
    pub(super) import_failed: bool,
}

impl State {
    pub(super) fn new() -> Self {
        Self {
            configured: false,
            closed: false,
            window_size: (0, 0),
            content_size: (0, 0),
            nv12_modifiers: Vec::new(),
            released: Vec::new(),
            input: Vec::new(),
            scroll_v120: (0, 0),
            presentation_clock: None,
            submit_to_present: LatencyWindow::with_capacity(LATENCY_SAMPLES),
            presented: 0,
            discarded: 0,
            import_failed: false,
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
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

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore WpViewport);

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.window_size = (width, height)
            }
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_dmabuf_v1::Event::Modifier {
            format,
            modifier_hi,
            modifier_lo,
        } = event
            && format == DrmFourcc::Nv12.code()
        {
            state
                .nv12_modifiers
                .push(u64::from(modifier_hi) << 32 | u64::from(modifier_lo));
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_buffer_params_v1::Event::Failed = event {
            state.import_failed = true;
        }
    }
}

impl Dispatch<WlBuffer, BufferKey> for State {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        key: &BufferKey,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.released.push(*key);
        }
    }
}

impl Dispatch<WpPresentation, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.presentation_clock = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, SubmittedAt> for State {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        submitted: &SubmittedAt,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                ..
            } => {
                state.presented += 1;
                if state.presentation_clock == Some(CLOCK_MONOTONIC) {
                    let seconds = u64::from(tv_sec_hi) << 32 | u64::from(tv_sec_lo);
                    let presented_ns = seconds * 1_000_000_000 + u64::from(tv_nsec);
                    let latency = presented_ns.saturating_sub(submitted.0);
                    state
                        .submit_to_present
                        .record(std::time::Duration::from_nanos(latency));
                }
            }
            wp_presentation_feedback::Event::Discarded => state.discarded += 1,
            _ => {}
        }
    }
}

pub(super) const fn button_pressed(
    state: WEnum<wayland_client::protocol::wl_pointer::ButtonState>,
) -> bool {
    matches!(
        state,
        WEnum::Value(wayland_client::protocol::wl_pointer::ButtonState::Pressed)
    )
}
