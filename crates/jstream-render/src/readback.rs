//! Reading composited frames back without waiting for the GPU (ADR-0117 §1, §4).
//!
//! A [`ReadbackRing`] copies an output into one of a few map-read buffers in a submit of
//! its own and asks wgpu to map it; a later call collects the buffers whose map has
//! finished, oldest first, polling the device without blocking. The thread that draws
//! never waits on the GPU: a frame read back reaches its consumers a frame or two after
//! it was drawn, under the tag (its time) it was recorded with. When every buffer is
//! still on its way, the frame is not read back and [`ReadbackRing::record`] says so,
//! for the caller to count (R-RT-004).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::compositor::Compositor;
use crate::error::RenderError;
use crate::scene::Output;

/// How many readbacks can be on their way at once: the frame drawn, the one the GPU is
/// running and one being mapped.
pub const READBACK_DEPTH: usize = 3;

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

struct Slot<T> {
    buffer: wgpu::Buffer,
    /// [`IDLE`], [`PENDING`] while the copy and the map are on their way, then
    /// [`MAPPED`] or [`FAILED`], set by the map's callback.
    state: Arc<AtomicU8>,
    /// What the frame was recorded with, and in which order.
    tag: Option<(u64, T)>,
}

/// A few map-read buffers for one output size, reused frame after frame, so a steady
/// size allocates nothing. Buffers belong to the device of the compositor that recorded
/// into them: a ring is dropped with its compositor.
pub struct ReadbackRing<T> {
    slots: Vec<Slot<T>>,
    width: u32,
    height: u32,
    /// The next recording's order.
    next: u64,
}

impl<T> Default for ReadbackRing<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            width: 0,
            height: 0,
            next: 0,
        }
    }
}

impl<T> std::fmt::Debug for ReadbackRing<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadbackRing")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("slots", &self.slots.len())
            .finish_non_exhaustive()
    }
}

impl<T> ReadbackRing<T> {
    /// Copies `output` into a free buffer in a submit of its own, after the composite
    /// that drew it, and starts mapping it, tagged with `tag`. `Ok(false)` when every
    /// buffer is still on its way: the frame is not read back. A new output size drops
    /// the buffers once none is on its way, and makes new ones.
    ///
    /// # Errors
    /// [`RenderError::TooLarge`] when a frame of this size does not fit in a buffer.
    pub fn record(
        &mut self,
        compositor: &Compositor,
        output: &Output,
        tag: T,
    ) -> Result<bool, RenderError> {
        let (width, height) = (output.width(), output.height());
        if (width, height) != (self.width, self.height) {
            if self.slots.iter().any(|slot| slot.tag.is_some()) {
                return Ok(false);
            }
            self.slots.clear();
            self.width = width;
            self.height = height;
        }
        if self.slots.len() < READBACK_DEPTH
            && !self
                .slots
                .iter()
                .any(|slot| slot.state.load(Ordering::Acquire) == IDLE)
        {
            self.slots.push(Slot {
                buffer: compositor.readback_buffer(width, height)?,
                state: Arc::new(AtomicU8::new(IDLE)),
                tag: None,
            });
        }
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.state.load(Ordering::Acquire) == IDLE)
        else {
            return Ok(false);
        };
        let mut encoder =
            compositor
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("jstream-render readback"),
                });
        compositor.record_copy_out(&mut encoder, output, &slot.buffer);
        {
            // As every submit on a platform context's queue: no other thread's staged
            // fences ride on this one.
            let _submitting = compositor.submit_lock().lock();
            compositor.queue().submit([encoder.finish()]);
        }
        slot.state.store(PENDING, Ordering::Release);
        let state = Arc::clone(&slot.state);
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let done = if result.is_ok() { MAPPED } else { FAILED };
                state.store(done, Ordering::Release);
            });
        slot.tag = Some((self.next, tag));
        self.next += 1;
        Ok(true)
    }

    /// Polls the device without waiting, then hands every frame whose map has finished,
    /// oldest first by the order they were recorded in, to `ready`: its tag and its
    /// tightly packed RGBA8 (`out`, reused). Maps finish in the order their submits
    /// complete, which is the order they were recorded in on one queue; should a later
    /// one finish first, it is handed first and the earlier one after it, so a caller
    /// that orders by tag drops that one. A map that failed (a lost device) frees its
    /// buffer and hands nothing. Returns how many were handed over.
    pub fn collect(
        &mut self,
        compositor: &Compositor,
        out: &mut Vec<u8>,
        mut ready: impl FnMut(T, &[u8]),
    ) -> usize {
        if !self.slots.iter().any(|slot| slot.tag.is_some()) {
            return 0;
        }
        let _ = compositor.device().poll(wgpu::PollType::Poll);
        let mut handed = 0;
        loop {
            let oldest = self
                .slots
                .iter_mut()
                .filter(|slot| {
                    matches!(slot.state.load(Ordering::Acquire), MAPPED | FAILED)
                        && slot.tag.is_some()
                })
                .min_by_key(|slot| slot.tag.as_ref().map(|(order, _)| *order));
            let Some(slot) = oldest else { break };
            let Some((_, tag)) = slot.tag.take() else {
                break;
            };
            if slot.state.load(Ordering::Acquire) == MAPPED {
                let unpacked = unpack(&slot.buffer, self.width, self.height, out);
                slot.buffer.unmap();
                if unpacked {
                    ready(tag, out);
                    handed += 1;
                }
            }
            slot.state.store(IDLE, Ordering::Release);
        }
        handed
    }

    /// Whether every buffer is on its way, so [`record`](Self::record) would refuse
    /// a frame of the ring's size.
    pub fn is_full(&self) -> bool {
        self.slots.len() >= READBACK_DEPTH && self.slots.iter().all(|slot| slot.tag.is_some())
    }

    /// How many readbacks are on their way.
    pub fn pending(&self) -> usize {
        self.slots.iter().filter(|slot| slot.tag.is_some()).count()
    }
}

/// Copies the mapped `buffer`'s padded rows into `out` as tightly packed RGBA8.
fn unpack(buffer: &wgpu::Buffer, width: u32, height: u32, out: &mut Vec<u8>) -> bool {
    let Ok(view) = buffer.slice(..).get_mapped_range() else {
        return false;
    };
    let row = width as usize * 4;
    let stride = crate::compositor::padded_row_bytes(width) as usize;
    out.clear();
    for y in 0..height as usize {
        match view.get(y * stride..y * stride + row) {
            Some(bytes) => out.extend_from_slice(bytes),
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use jstream_media::FrameSize;

    use super::{READBACK_DEPTH, ReadbackRing};
    use crate::tests::open;
    use crate::{Color, Compositor, Output, Scene};

    fn render(compositor: &mut Compositor, width: u32, height: u32, red: f32) -> Output {
        let scene = Scene {
            canvas: FrameSize::new(width, height).expect("a test size"),
            background: Color::rgb(red, 0.0, 0.0),
            items: &[],
        };
        compositor.render(&scene, false).expect("render")
    }

    /// Collects until `want` frames have been handed over, polling without a wait.
    fn collect_all(
        ring: &mut ReadbackRing<u32>,
        compositor: &Compositor,
        want: usize,
    ) -> Vec<(u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut got = Vec::new();
        for _ in 0..10_000 {
            ring.collect(compositor, &mut out, |tag, rgba| {
                got.push((tag, rgba.to_vec()))
            });
            if got.len() >= want {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        got
    }

    /// Each frame recorded comes back once mapped, oldest first, with its tag and the
    /// pixels it was drawn with, tightly packed; and recording never waits.
    #[test]
    fn frames_come_back_in_order_with_their_pixels() {
        let Some(mut opened) = open() else { return };
        let compositor = &mut opened.compositor;
        let mut ring = ReadbackRing::default();
        // An odd width, so the rows are padded in the buffer.
        let (width, height) = (5, 3);
        for (tag, red) in [(0u32, 0.0f32), (1, 1.0)] {
            let output = render(compositor, width, height, red);
            assert!(ring.record(compositor, &output, tag).expect("record"));
        }
        assert_eq!(ring.pending(), 2);
        let got = collect_all(&mut ring, compositor, 2);
        assert_eq!(got.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(), [0, 1]);
        for (tag, rgba) in &got {
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            let red = if *tag == 0 { 0 } else { 255 };
            assert!(
                rgba.chunks_exact(4).all(|px| px == [red, 0, 0, 255]),
                "frame {tag}: {rgba:?}"
            );
        }
        assert_eq!(ring.pending(), 0);
    }

    /// With every buffer on its way, a frame is refused rather than waited for; so is a
    /// new size while the old one's frames are out. Collected, the ring takes both.
    #[test]
    fn a_full_ring_refuses_and_a_resize_waits_for_the_ring_to_drain() {
        let Some(mut opened) = open() else { return };
        let compositor = &mut opened.compositor;
        let mut ring = ReadbackRing::default();
        let output = render(compositor, 4, 4, 1.0);
        for tag in 0..READBACK_DEPTH as u32 {
            assert!(ring.record(compositor, &output, tag).expect("record"));
        }
        // No poll between: every map is still pending.
        assert!(ring.is_full());
        assert!(!ring.record(compositor, &output, 99).expect("record"));
        let larger = render(compositor, 8, 2, 0.0);
        assert!(!ring.record(compositor, &larger, 100).expect("record"));
        assert_eq!(
            collect_all(&mut ring, compositor, READBACK_DEPTH).len(),
            READBACK_DEPTH
        );
        assert!(!ring.is_full());
        assert!(ring.record(compositor, &larger, 7).expect("record"));
        let got = collect_all(&mut ring, compositor, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 7);
        assert_eq!(got[0].1.len(), 8 * 2 * 4);
    }
}
