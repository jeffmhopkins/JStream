//! Reading composited frames back without waiting for the GPU (ADR-0117 §1, §4).
//!
//! A [`ReadbackRing`] copies an output into one of a few map-read buffers in a submit of
//! its own and asks wgpu to map it; a later call collects the buffers whose map has
//! finished, oldest first, polling the device without blocking. The thread that draws
//! never waits on the GPU: a frame read back reaches its consumers a frame or two after
//! it was drawn, under the tag (its time) it was recorded with. When every buffer is
//! still on its way, the frame is not read back and [`ReadbackRing::record`] says so,
//! for the caller to count (R-RT-004).
//!
//! A ring made [`with_copier`](ReadbackRing::with_copier) leaves the copy out of the
//! mapped buffer to a [`Copier`]'s `jstream-readback` thread (ADR-0117 §4): the drawing
//! thread hands it each buffer once mapped and later takes back the finished frame, a
//! buffer of its own that it swaps in, so it never touches a frame's pixels. A lost
//! device's buffers are freed by the next poll of it on any thread, so the copier reads
//! a buffer only while its device can't be marked lost, and reads nothing once it is
//! ([`Compositor::destroy_device`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::compositor::Compositor;
use crate::error::RenderError;
use crate::gpu::Faults;
use crate::scene::Output;

/// How many readbacks can be on their way at once: the frame drawn, the one the GPU is
/// running and one being mapped.
pub const READBACK_DEPTH: usize = 3;

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;
/// Handed to the [`Copier`], until its copy comes back.
const COPYING: u8 = 4;

/// The `jstream-readback` thread that copies mapped readback buffers out for the rings
/// made [`with_copier`](ReadbackRing::with_copier), so the thread that draws doesn't.
/// Cloning shares the thread, which ends once every clone and every ring using it is
/// gone.
#[derive(Clone, Debug)]
pub struct Copier {
    jobs: Sender<Job>,
}

/// One mapped buffer to copy out and unmap.
struct Job {
    buffer: wgpu::Buffer,
    /// How the buffer's device has failed: the copy holds off its loss.
    faults: Arc<Faults>,
    width: u32,
    height: u32,
    order: u64,
    /// Where the frame goes: a buffer the ring had spare, reused.
    into: Vec<u8>,
    done: Sender<Copied>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

/// A finished copy: the frame recorded `order`th, or `None` when its buffer could not be
/// read (a lost device).
struct Copied {
    order: u64,
    frame: Option<Vec<u8>>,
}

impl Copier {
    /// Starts the thread.
    ///
    /// # Errors
    /// When the OS refuses the thread.
    pub fn spawn() -> std::io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("jstream-readback".into())
            .spawn(move || {
                for mut job in queue {
                    let copied = unpack(
                        &job.faults,
                        &job.buffer,
                        job.width,
                        job.height,
                        &mut job.into,
                    );
                    // A lost device's buffers are gone: unmapping one only raises an
                    // error for the uncaptured-error handler to keep (as on the ring's
                    // own copies below).
                    if job.faults.lost.get().is_none() {
                        job.buffer.unmap();
                    }
                    let frame = copied.then_some(job.into);
                    // A ring that is gone wants nothing back.
                    let _ = job.done.send(Copied {
                        order: job.order,
                        frame,
                    });
                }
            })?;
        Ok(Self { jobs })
    }
}

/// A ring's link to its [`Copier`].
struct Handoff {
    jobs: Sender<Job>,
    done: Sender<Copied>,
    back: Receiver<Copied>,
    /// Buffers to copy into, from the frames the caller let go of.
    spares: Vec<Vec<u8>>,
}

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
    /// Where mapped buffers are copied out, when not on the caller's thread.
    copier: Option<Handoff>,
}

impl<T> Default for ReadbackRing<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            width: 0,
            height: 0,
            next: 0,
            copier: None,
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
    /// A ring whose mapped buffers `copier`'s thread copies out ([`Self::collect`]).
    pub fn with_copier(copier: &Copier) -> Self {
        let (done, back) = mpsc::channel();
        Self {
            copier: Some(Handoff {
                jobs: copier.jobs.clone(),
                done,
                back,
                spares: Vec::new(),
            }),
            ..Self::default()
        }
    }

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
                .any(|slot| slot.tag.is_none() && slot.state.load(Ordering::Acquire) == IDLE)
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
            .find(|slot| slot.tag.is_none() && slot.state.load(Ordering::Acquire) == IDLE)
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
    /// tightly packed RGBA8, which is also left in `out`. Maps finish in the order their
    /// submits complete, which is the order they were recorded in on one queue; should a
    /// later one finish first, it is handed first and the earlier one after it, so a
    /// caller that orders by tag drops that one. A map that failed (a lost device) frees
    /// its buffer and hands nothing. Returns how many were handed over.
    ///
    /// On a ring [`with_copier`](Self::with_copier), a finished map goes to the copier's
    /// thread instead, and what it has copied since the last call is handed over: each
    /// frame is swapped into `out`, whose earlier buffer is kept to copy a later frame
    /// into, so this thread copies no pixels and a steady size allocates nothing once
    /// the buffers have gone round. A frame comes back a call or two after its map
    /// finishes.
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
        if let Some(handoff) = &mut self.copier {
            // Each finished map to the copier, in the order recorded.
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
                let Some(order) = slot.tag.as_ref().map(|(order, _)| *order) else {
                    break;
                };
                if slot.state.load(Ordering::Acquire) == FAILED {
                    slot.tag = None;
                    slot.state.store(IDLE, Ordering::Release);
                    continue;
                }
                slot.state.store(COPYING, Ordering::Release);
                let job = Job {
                    buffer: slot.buffer.clone(),
                    faults: Arc::clone(compositor.faults()),
                    width: self.width,
                    height: self.height,
                    order,
                    into: handoff.spares.pop().unwrap_or_default(),
                    done: handoff.done.clone(),
                };
                if let Err(mpsc::SendError(job)) = handoff.jobs.send(job) {
                    // The copier is gone: copied here instead, as without one.
                    let copied = unpack(
                        compositor.faults(),
                        &slot.buffer,
                        self.width,
                        self.height,
                        out,
                    );
                    if compositor.faults().lost.get().is_none() {
                        slot.buffer.unmap();
                    }
                    handoff.spares.push(job.into);
                    if let Some((_, tag)) = slot.tag.take()
                        && copied
                    {
                        ready(tag, out);
                        handed += 1;
                    }
                    slot.state.store(IDLE, Ordering::Release);
                }
            }
            // What the copier has finished, in the order it was handed.
            while let Ok(copied) = handoff.back.try_recv() {
                let Some(slot) = self.slots.iter_mut().find(|slot| {
                    slot.tag
                        .as_ref()
                        .is_some_and(|(order, _)| *order == copied.order)
                }) else {
                    // A slot dropped by a resize can't be, while it is out: nothing.
                    continue;
                };
                let tag = slot.tag.take();
                slot.state.store(IDLE, Ordering::Release);
                if let (Some((_, tag)), Some(mut frame)) = (tag, copied.frame) {
                    std::mem::swap(out, &mut frame);
                    handoff.spares.push(frame);
                    ready(tag, out);
                    handed += 1;
                }
            }
            return handed;
        }
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
                let unpacked = unpack(
                    compositor.faults(),
                    &slot.buffer,
                    self.width,
                    self.height,
                    out,
                );
                if compositor.faults().lost.get().is_none() {
                    slot.buffer.unmap();
                }
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

/// Copies the mapped `buffer`'s padded rows into `out` as tightly packed RGBA8. `false`
/// when it can't be read, as on a lost device, whose loss waits for the copy
/// ([`Faults::read_mapped`]).
fn unpack(
    faults: &Faults,
    buffer: &wgpu::Buffer,
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) -> bool {
    let row = width as usize * 4;
    let stride = crate::compositor::padded_row_bytes(width) as usize;
    out.clear();
    faults
        .read_mapped(buffer, |view| {
            for y in 0..height as usize {
                match view.get(y * stride..y * stride + row) {
                    Some(bytes) => out.extend_from_slice(bytes),
                    None => return false,
                }
            }
            true
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use jstream_media::FrameSize;

    use super::{Copier, READBACK_DEPTH, ReadbackRing};
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

    /// A ring with a copier hands back the same frames, in order, with the same pixels:
    /// copied on the `jstream-readback` thread and swapped into `out`, whose earlier
    /// buffer is reused for a later frame.
    #[test]
    fn a_copier_hands_back_the_same_frames_in_order() {
        let Some(mut opened) = open() else { return };
        let compositor = &mut opened.compositor;
        let copier = Copier::spawn().expect("the copier thread");
        let mut ring = ReadbackRing::with_copier(&copier);
        let (width, height) = (5, 3);
        let mut got = Vec::new();
        let mut out = Vec::new();
        for round in 0..3u32 {
            for (tag, red) in [(round * 2, 0.0f32), (round * 2 + 1, 1.0)] {
                let output = render(compositor, width, height, red);
                assert!(ring.record(compositor, &output, tag).expect("record"));
            }
            let want = got.len() + 2;
            for _ in 0..10_000 {
                ring.collect(compositor, &mut out, |tag, rgba| {
                    got.push((tag, rgba.to_vec()))
                });
                if got.len() >= want {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(ring.pending(), 0);
        }
        assert_eq!(
            got.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4, 5]
        );
        for (tag, rgba) in &got {
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            let red = if tag % 2 == 0 { 0 } else { 255 };
            assert!(
                rgba.chunks_exact(4).all(|px| px == [red, 0, 0, 255]),
                "frame {tag}: {rgba:?}"
            );
        }
        // Out once, a buffer stays busy until its copy is back.
        let output = render(compositor, width, height, 1.0);
        for tag in 0..READBACK_DEPTH as u32 {
            assert!(ring.record(compositor, &output, tag).expect("record"));
        }
        assert!(ring.is_full());
    }

    /// A device lost while the copier reads one of its mapped buffers frees nothing
    /// under it: the next poll of a destroyed device frees every buffer made on it, so
    /// before the loss waited for the copy this read freed memory on the
    /// `jstream-readback` thread (#1454). Frames large enough that each copy takes a
    /// while, handed over and then lost at once, a few times over.
    #[test]
    fn a_device_lost_while_the_copier_reads_frees_nothing_under_it() {
        for _ in 0..8 {
            let Some(mut opened) = open() else { return };
            let compositor = &mut opened.compositor;
            let copier = Copier::spawn().expect("the copier thread");
            let mut ring = ReadbackRing::with_copier(&copier);
            let (width, height) = (3840, 2160);
            for tag in 0..READBACK_DEPTH as u32 {
                let output = render(compositor, width, height, 1.0);
                assert!(ring.record(compositor, &output, tag).expect("record"));
            }
            let _ = compositor
                .device()
                .poll(wgpu::PollType::wait_indefinitely());
            let mut out = Vec::new();
            let mut tags = Vec::new();
            ring.collect(compositor, &mut out, |tag, _| tags.push(tag));
            compositor.destroy_device();
            let _ = compositor
                .device()
                .poll(wgpu::PollType::wait_indefinitely());
            assert_eq!(compositor.lost().as_deref(), Some("destroyed"));
            // A copy that finished before the loss still comes back, whole and red; one
            // the loss caught comes back as nothing. Each frame comes back at most once,
            // and every buffer is free again once the copier has answered for it.
            for _ in 0..50 {
                ring.collect(compositor, &mut out, |tag, rgba| {
                    tags.push(tag);
                    assert_eq!(rgba.len(), (width * height * 4) as usize, "frame {tag}");
                    assert!(
                        rgba.chunks_exact(4).all(|px| px == [255, 0, 0, 255]),
                        "frame {tag} came back partly copied"
                    );
                });
                if ring.pending() == 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(ring.pending(), 0, "a buffer is still out after the loss");
            let handed = tags.len();
            tags.sort_unstable();
            tags.dedup();
            assert_eq!(tags.len(), handed, "a frame came back twice: {tags:?}");
            assert!(
                tags.iter().all(|tag| (*tag as usize) < READBACK_DEPTH),
                "a frame that was never recorded came back: {tags:?}"
            );
        }
    }
}
