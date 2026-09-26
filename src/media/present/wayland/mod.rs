mod buffers;
mod seat;
mod state;

use std::os::fd::{AsFd, BorrowedFd};
use std::time::Duration;

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, EventQueue, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::Flags;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase;

use buffers::{BufferCache, BufferKey};
use state::{State, SubmittedAt};

use super::input::InputMessage;
use super::PresentError;
use crate::media::video::{DecodedFrame, DmabufFrame};
use crate::util::latency::LatencySummary;
use crate::util::numeric::saturating_i32;

const APP_ID: &str = "crownconnect-viewer";
/// Height a new window opens at when the compositor leaves the size to us.
const DEFAULT_WINDOW_HEIGHT: u32 = 960;
const DMABUF_VERSION: u32 = 3;

/// How presentation is going, for latency reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationStats {
    /// From handing a frame to the compositor until it reached the screen.
    pub submit_to_present: Option<LatencySummary>,
    pub presented: u64,
    pub discarded: u64,
    /// Distinct dmabufs imported as `wl_buffer`s.
    pub imports: u64,
    /// Decoded frames the compositor still holds.
    pub held: usize,
}

/// A minimal xdg-toplevel that shows decoded frames by attaching their dmabufs, scaled by the
/// compositor through wp_viewporter, with wp_presentation feedback for latency.
#[derive(Debug)]
pub struct Presenter {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    surface: WlSurface,
    _xdg_surface: XdgSurface,
    _toplevel: XdgToplevel,
    viewport: Option<WpViewport>,
    dmabuf: ZwpLinuxDmabufV1,
    presentation: Option<WpPresentation>,
    buffers: BufferCache,
    video_size: (u32, u32),
}

impl Presenter {
    pub fn connect(title: &str) -> Result<Self, PresentError> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let handle = queue.handle();
        let compositor: WlCompositor = globals.bind(&handle, 4..=6, ())?;
        let wm_base: XdgWmBase = globals.bind(&handle, 1..=6, ())?;
        let dmabuf: ZwpLinuxDmabufV1 =
            globals.bind(&handle, DMABUF_VERSION..=DMABUF_VERSION, ())?;
        let presentation: Option<WpPresentation> = globals.bind(&handle, 1..=1, ()).ok();
        let viewporter: Option<WpViewporter> = globals.bind(&handle, 1..=1, ()).ok();
        let _seat: Option<WlSeat> = globals.bind(&handle, 1..=9, ()).ok();

        let surface = compositor.create_surface(&handle, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &handle, ());
        let toplevel = xdg_surface.get_toplevel(&handle, ());
        toplevel.set_title(title.to_owned());
        toplevel.set_app_id(APP_ID.to_owned());
        let viewport = viewporter.map(|viewporter| viewporter.get_viewport(&surface, &handle, ()));
        surface.commit();

        let mut state = State::new();
        while !state.configured {
            queue.blocking_dispatch(&mut state)?;
        }
        Ok(Self {
            connection,
            queue,
            state,
            surface,
            _xdg_surface: xdg_surface,
            _toplevel: toplevel,
            viewport,
            dmabuf,
            presentation,
            buffers: BufferCache::default(),
            video_size: (0, 0),
        })
    }

    pub const fn is_closed(&self) -> bool {
        self.state.closed
    }

    /// Flushes requests, then waits until the compositor or `other` has something to read, or
    /// `timeout` passes. Dispatches compositor events and reports whether `other` is readable.
    pub fn wait(
        &mut self,
        other: Option<BorrowedFd<'_>>,
        timeout: Option<Duration>,
    ) -> Result<bool, PresentError> {
        self.queue.flush()?;
        self.queue.dispatch_pending(&mut self.state)?;
        let mut other_ready = false;
        if let Some(guard) = self.queue.prepare_read() {
            let timeout = timeout.map(|timeout| Timespec {
                tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: i64::from(timeout.subsec_nanos()),
            });
            let (wayland_ready, ready) = {
                let wayland_fd = guard.connection_fd();
                let other_fd = other.unwrap_or(wayland_fd);
                let mut all = [
                    PollFd::new(&wayland_fd, PollFlags::IN),
                    PollFd::new(&other_fd, PollFlags::IN),
                ];
                let watched = if other.is_some() { all.len() } else { 1 };
                let fds = all.get_mut(..watched).unwrap_or_default();
                match poll(fds, timeout.as_ref()) {
                    Ok(_) | Err(rustix::io::Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                }
                let readable = |fd: &PollFd<'_>| {
                    fd.revents()
                        .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
                };
                let wayland_ready = fds.first().is_some_and(readable);
                (wayland_ready, fds.get(1).is_some_and(readable))
            };
            other_ready = ready;
            if wayland_ready {
                guard.read()?;
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        for key in self.state.released.drain(..) {
            self.buffers.release(key);
        }
        Ok(other_ready)
    }

    /// Shows `frame` without copying it. The frame is held until the compositor releases it.
    pub fn present(&mut self, frame: DecodedFrame) -> Result<(), PresentError> {
        if self.state.closed {
            return Ok(());
        }
        let export = frame.export_dmabuf()?;
        let dmabuf = export.frame()?;
        let identity = dmabuf.identity()?;
        let key = BufferKey(identity.inode);
        let handle = self.queue.handle();
        let buffer = match self.buffers.get(&identity) {
            Some(buffer) => buffer.clone(),
            None => {
                let buffer = self.import(&dmabuf, key, &handle)?;
                self.buffers.insert(identity, buffer.clone());
                buffer
            }
        };
        drop(export);
        self.fit_to_window(frame.width(), frame.height());
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        if let Some(presentation) = &self.presentation {
            presentation.feedback(&self.surface, &handle, SubmittedAt::now());
        }
        self.surface.commit();
        self.buffers.hold(key, frame);
        self.queue.flush()?;
        Ok(())
    }

    /// Input gathered since the last call, oldest first.
    pub fn drain_input(&mut self) -> impl Iterator<Item = InputMessage> + '_ {
        self.state.input.drain(..)
    }

    pub fn stats(&self) -> PresentationStats {
        PresentationStats {
            submit_to_present: self.state.submit_to_present.summary(),
            presented: self.state.presented,
            discarded: self.state.discarded,
            imports: self.buffers.imports,
            held: self.buffers.held(),
        }
    }

    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    fn import(
        &self,
        dmabuf: &DmabufFrame<'_>,
        key: BufferKey,
        handle: &QueueHandle<State>,
    ) -> Result<WlBuffer, PresentError> {
        if !self.state.nv12_modifiers.contains(&dmabuf.modifier) {
            return Err(PresentError::UnsupportedModifier(dmabuf.modifier));
        }
        let params = self.dmabuf.create_params(handle, ());
        let modifier_hi = u32::try_from(dmabuf.modifier >> 32).unwrap_or_default();
        let modifier_lo = u32::try_from(dmabuf.modifier & u64::from(u32::MAX)).unwrap_or_default();
        for (index, plane) in (0u32..).zip(dmabuf.planes) {
            params.add(
                dmabuf.fd.as_fd(),
                index,
                plane.offset,
                plane.pitch,
                modifier_hi,
                modifier_lo,
            );
        }
        let buffer = params.create_immed(
            i32::try_from(dmabuf.width).unwrap_or(i32::MAX),
            i32::try_from(dmabuf.height).unwrap_or(i32::MAX),
            dmabuf.fourcc.code(),
            Flags::empty(),
            handle,
            key,
        );
        params.destroy();
        Ok(buffer)
    }

    fn fit_to_window(&mut self, video_width: u32, video_height: u32) {
        let window = self.state.window_size;
        if self.video_size == (video_width, video_height)
            && self.state.content_size != (0, 0)
            && window == (0, 0)
        {
            return;
        }
        self.video_size = (video_width, video_height);
        let content = fitted_size((video_width, video_height), window);
        if content == self.state.content_size {
            return;
        }
        self.state.content_size = content;
        if let Some(viewport) = &self.viewport {
            viewport.set_destination(content.0, content.1);
        }
    }
}

/// The largest size with the video's aspect ratio inside `window`, or a default-height window
/// when the compositor lets the client choose.
fn fitted_size(video: (u32, u32), window: (i32, i32)) -> (i32, i32) {
    let (video_width, video_height) = (f64::from(video.0.max(1)), f64::from(video.1.max(1)));
    let scale = match window {
        (width, height) if width > 0 && height > 0 => {
            (f64::from(width) / video_width).min(f64::from(height) / video_height)
        }
        _ => (f64::from(DEFAULT_WINDOW_HEIGHT) / video_height).min(1.0),
    };
    let scaled = |extent: f64| saturating_i32((extent * scale).round().max(1.0));
    (scaled(video_width), scaled(video_height))
}

#[cfg(test)]
mod tests {
    use super::fitted_size;

    #[test]
    fn fits_portrait_video_inside_the_window() {
        assert_eq!(fitted_size((1080, 2400), (800, 800)), (360, 800));
        assert_eq!(fitted_size((1080, 2400), (0, 0)), (432, 960));
        assert_eq!(fitted_size((640, 360), (0, 0)), (640, 360));
    }
}
