use wayland_client::protocol::wl_buffer::WlBuffer;

use crate::media::video::{BufferIdentity, DecodedFrame};

/// The decoder recycles a small pool of surfaces, so each distinct dmabuf is imported into the
/// compositor once and its `wl_buffer` reused; more than this many means the pool changed.
const MAX_CACHED_BUFFERS: usize = 32;

/// Identifies a `wl_buffer` in release events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BufferKey(pub(super) u64);

#[derive(Debug)]
struct CachedBuffer {
    identity: BufferIdentity,
    buffer: WlBuffer,
}

/// `wl_buffer`s by dmabuf, plus the decoded frames the compositor still reads from.
#[derive(Debug, Default)]
pub(super) struct BufferCache {
    buffers: Vec<CachedBuffer>,
    held: Vec<(BufferKey, DecodedFrame)>,
    pub(super) imports: u64,
}

impl BufferCache {
    pub(super) fn get(&self, identity: &BufferIdentity) -> Option<&WlBuffer> {
        self.buffers
            .iter()
            .find(|cached| cached.identity == *identity)
            .map(|cached| &cached.buffer)
    }

    pub(super) fn insert(&mut self, identity: BufferIdentity, buffer: WlBuffer) {
        if self.buffers.len() == MAX_CACHED_BUFFERS {
            let evicted = self.buffers.remove(0);
            evicted.buffer.destroy();
        }
        self.imports += 1;
        self.buffers.push(CachedBuffer { identity, buffer });
    }

    pub(super) fn hold(&mut self, key: BufferKey, frame: DecodedFrame) {
        self.release(key);
        self.held.push((key, frame));
    }

    pub(super) fn release(&mut self, key: BufferKey) {
        self.held.retain(|(held, _)| *held != key);
    }

    pub(super) const fn held(&self) -> usize {
        self.held.len()
    }
}
