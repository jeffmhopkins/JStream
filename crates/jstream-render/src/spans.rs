//! GPU time of a frame's units of work (ADR-0103, `profile.get`'s `gpu_ms`): what the
//! GPU spent running each, which a CPU clock on the drawing thread can't see.
//!
//! The drawer opens a frame ([`Compositor::begin_gpu_frame`](crate::Compositor::begin_gpu_frame))
//! and names the unit each stretch of its work belongs to
//! ([`Compositor::gpu_unit`](crate::Compositor::gpu_unit)). While a frame is open and a
//! unit named, every submit of the compositor's own work goes with two empty compute
//! passes, one in a command buffer before the work and one after, each writing a
//! timestamp ([`Gpu::submit_work`](crate::gpu::Gpu::submit_work)): the GPU runs a submit's
//! command buffers in order, so the span between them is the GPU running that work,
//! without the time it sat idle waiting for the drawing thread. Closing the frame
//! ([`Compositor::end_gpu_frame`](crate::Compositor::end_gpu_frame)) resolves its
//! timestamps into a buffer, copies them to a mappable one in the same submit and maps
//! it; they are read a frame or two on, without waiting
//! ([`Compositor::gpu_units`](crate::Compositor::gpu_units)), each unit's spans in the
//! frame added together.
//!
//! One unit is named at a time and every span is one submit's, so a frame's spans never
//! nest or overlap: its units' times add up to the GPU time of the frame's timed work.
//! The program's render thread relies on that sum to choose the size 3D models are drawn
//! at (ADR-0197), so a unit must never be timed inside another. A span longer than
//! [`LONGEST`] is dropped as garbage, which makes its frame read shorter than it was.
//!
//! [`FRAMES`] frames may be on their way at once, each of at most [`SPANS`] spans; a
//! frame opened while all are on their way isn't timed, and spans past the last aren't
//! either. Copies and uploads outside a submit of the compositor's own (a texture write,
//! a readback, the encoders' conversion) carry no timestamps, so a unit that only
//! uploads shows little GPU time. A device without `TIMESTAMP_QUERY` times nothing.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Frames that may be on their way at once.
pub(crate) const FRAMES: usize = 3;

/// The most spans one frame times.
pub(crate) const SPANS: u32 = 256;

/// The most one span may take before it is taken for garbage (a query whose submit
/// never happened reads whatever the buffer held).
const LONGEST: Duration = Duration::from_secs(10);

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

/// One compositor's frame spans: inert on a device without `TIMESTAMP_QUERY`.
pub(crate) struct Spans {
    inner: Mutex<Option<Inner>>,
}

struct Inner {
    frames: Vec<FrameSet>,
    /// The frame being recorded into, while one is open.
    open: Option<usize>,
    /// The unit the work submitted now belongs to.
    unit: Option<u32>,
    /// Nanoseconds per timestamp tick.
    period: f64,
    /// Each unit's nanoseconds in the frame being read, reused.
    sums: Vec<(u32, u64)>,
}

struct FrameSet {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// The unit of each span recorded, in order.
    units: Vec<u32>,
    /// The drawer's tag for the frame.
    tag: u64,
    state: Arc<AtomicU8>,
    /// Whether it is being recorded or is on its way.
    busy: bool,
}

/// The two timestamp writes of one span, for the submit that carries it.
pub(crate) struct Span {
    queries: wgpu::QuerySet,
    first: u32,
}

impl Span {
    /// Records the write of its `which` (0, before the work, or 1, after it) timestamp
    /// into a command buffer of its own.
    pub(crate) fn stamp(&self, device: &wgpu::Device, which: u32) -> wgpu::CommandBuffer {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render span"),
        });
        drop(encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("jstream-render span"),
            timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                query_set: &self.queries,
                beginning_of_pass_write_index: Some(self.first + which),
                end_of_pass_write_index: None,
            }),
        }));
        encoder.finish()
    }
}

impl Spans {
    /// Spans on `device`, inert when it lacks `TIMESTAMP_QUERY`.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let inner = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY)
            .then(|| Inner {
                frames: (0..FRAMES).map(|_| FrameSet::new(device)).collect(),
                open: None,
                unit: None,
                period: f64::from(queue.get_timestamp_period()),
                sums: Vec::with_capacity(SPANS as usize),
            });
        Self {
            inner: Mutex::new(inner),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Inner>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the device can time spans at all.
    pub(crate) fn supported(&self) -> bool {
        self.lock().is_some()
    }

    /// Opens a frame tagged `tag`: whether it is timed (the device times spans and a
    /// frame set is free). A frame left open is closed untimed first.
    pub(crate) fn begin(&self, tag: u64) -> bool {
        let mut guard = self.lock();
        let Some(inner) = guard.as_mut() else {
            return false;
        };
        if let Some(open) = inner.open.take() {
            inner.frames[open].busy = false;
        }
        inner.unit = None;
        let free = inner
            .frames
            .iter()
            .position(|set| !set.busy && set.state.load(Ordering::Acquire) == IDLE);
        if let Some(at) = free {
            let set = &mut inner.frames[at];
            set.busy = true;
            set.tag = tag;
            set.units.clear();
            inner.open = Some(at);
        }
        inner.open.is_some()
    }

    /// Names the unit the work submitted from now on belongs to; `None` times none.
    pub(crate) fn unit(&self, unit: Option<u32>) {
        if let Some(inner) = self.lock().as_mut() {
            inner.unit = unit;
        }
    }

    /// The span for one submit of the open frame's current unit, if any.
    pub(crate) fn span(&self) -> Option<Span> {
        let mut guard = self.lock();
        let inner = guard.as_mut()?;
        let unit = inner.unit?;
        let set = &mut inner.frames[inner.open?];
        let index = u32::try_from(set.units.len()).ok()?;
        if index >= SPANS {
            return None;
        }
        set.units.push(unit);
        Some(Span {
            queries: set.queries.clone(),
            first: index * 2,
        })
    }

    /// Closes the open frame: resolves its timestamps and maps them. The caller holds
    /// the lock the compositor's submits take. A frame with no span is let go.
    pub(crate) fn end(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut guard = self.lock();
        let Some(inner) = guard.as_mut() else {
            return;
        };
        inner.unit = None;
        let Some(open) = inner.open.take() else {
            return;
        };
        let set = &mut inner.frames[open];
        let count = u32::try_from(set.units.len()).unwrap_or(SPANS).min(SPANS) * 2;
        if count == 0 {
            set.busy = false;
            return;
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("jstream-render span resolve"),
        });
        encoder.resolve_query_set(&set.queries, 0..count, &set.resolve, 0);
        encoder.copy_buffer_to_buffer(&set.resolve, 0, &set.readback, 0, u64::from(count) * 8);
        crate::gpu::count_submit();
        queue.submit([encoder.finish()]);
        set.state.store(PENDING, Ordering::Release);
        let state = Arc::clone(&set.state);
        set.readback
            .slice(..u64::from(count) * 8)
            .map_async(wgpu::MapMode::Read, move |result| {
                state.store(
                    if result.is_ok() { MAPPED } else { FAILED },
                    Ordering::Release,
                );
            });
    }

    /// Polls `device` without waiting and hands every frame read since the last call
    /// to `measured`: its tag, then each unit it timed with that unit's GPU time in it.
    /// Allocates nothing.
    pub(crate) fn collect(
        &self,
        device: &wgpu::Device,
        mut measured: impl FnMut(u64, u32, Duration),
    ) {
        let mut guard = self.lock();
        let Some(inner) = guard.as_mut() else {
            return;
        };
        if inner
            .frames
            .iter()
            .any(|set| set.state.load(Ordering::Acquire) == PENDING)
        {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        let Inner {
            frames,
            period,
            sums,
            ..
        } = inner;
        for set in frames.iter_mut() {
            match set.state.load(Ordering::Acquire) {
                MAPPED => {
                    sums.clear();
                    let count = set.units.len().min(SPANS as usize);
                    if let Ok(view) = set.readback.slice(..count as u64 * 16).get_mapped_range() {
                        for (span, unit) in view.chunks_exact(16).zip(&set.units) {
                            let tick = |at: usize| {
                                span.get(at..at + 8)
                                    .and_then(|b| b.try_into().ok())
                                    .map(u64::from_le_bytes)
                            };
                            let (Some(start), Some(end)) = (tick(0), tick(8)) else {
                                continue;
                            };
                            let nanos = (end.saturating_sub(start) as f64 * *period) as u64;
                            if Duration::from_nanos(nanos) > LONGEST {
                                continue;
                            }
                            if let Some((_, sum)) = sums.iter_mut().find(|(u, _)| u == unit) {
                                *sum = sum.saturating_add(nanos);
                            } else if sums.len() < sums.capacity() {
                                sums.push((*unit, nanos));
                            }
                        }
                    }
                    set.readback.unmap();
                    for (unit, nanos) in sums.iter() {
                        measured(set.tag, *unit, Duration::from_nanos(*nanos));
                    }
                    set.busy = false;
                    set.state.store(IDLE, Ordering::Release);
                }
                FAILED => {
                    set.busy = false;
                    set.state.store(IDLE, Ordering::Release);
                }
                _ => {}
            }
        }
    }
}

impl FrameSet {
    fn new(device: &wgpu::Device) -> Self {
        let bytes = u64::from(SPANS) * 16;
        Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("jstream-render spans"),
                ty: wgpu::QueryType::Timestamp,
                count: SPANS * 2,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("jstream-render spans resolve"),
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("jstream-render spans readback"),
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            units: Vec::with_capacity(SPANS as usize),
            tag: 0,
            state: Arc::new(AtomicU8::new(IDLE)),
            busy: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::filter::FilterClock;
    use crate::filter::testing::{filter, image};
    use crate::tests::open;

    /// The work submitted while a unit is named comes back as that unit's GPU time, a
    /// frame or so late, once per unit per frame; work outside any unit, and work
    /// outside a timed frame, doesn't.
    #[test]
    fn a_units_gpu_time_comes_back_under_its_id() {
        let Some(mut opened) = open() else {
            return;
        };
        // lavapipe, which CI's Linux jobs draw on, has timestamp queries.
        let lavapipe = opened.compositor.adapter_description().contains("llvmpipe");
        assert!(!lavapipe || opened.compositor.times_gpu());
        if !opened.compositor.times_gpu() {
            return;
        }
        let input = image(&opened, 64, &[[255, 0, 0, 255]; 64 * 64]);
        let mut sharpen = filter("sharpen", serde_json::json!({}));
        let clock = FilterClock::default();
        let mut run = |opened: &mut crate::tests::Opened| {
            opened
                .compositor
                .apply_filter(&mut sharpen, &input, clock)
                .expect("drawn");
        };
        // Untimed: no frame open.
        opened.compositor.gpu_unit(Some(3));
        run(&mut opened);
        assert!(opened.compositor.begin_gpu_frame(41));
        // Timed: unit 7 twice, then work outside any unit.
        opened.compositor.gpu_unit(Some(7));
        run(&mut opened);
        run(&mut opened);
        opened.compositor.gpu_unit(None);
        run(&mut opened);
        opened.compositor.end_gpu_frame();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut times = Vec::new();
        while times.is_empty() && Instant::now() < deadline {
            let _ = opened
                .compositor
                .device()
                .poll(wgpu::PollType::wait_indefinitely());
            opened
                .compositor
                .gpu_units(|tag, unit, took| times.push((tag, unit, took)));
        }
        assert_eq!(times.len(), 1, "{times:?}");
        assert_eq!((times[0].0, times[0].1), (41, 7));
        assert!(times[0].2 > Duration::ZERO, "{times:?}");
        // Each frame comes back once.
        opened
            .compositor
            .gpu_units(|tag, unit, _| panic!("frame {tag} unit {unit} came back twice"));
    }

    /// A frame's way out is timed with the unit named around it: a readback's copy, and
    /// its conversion to NV12 first, are submits of their own after the composite, and
    /// come back under that unit. Before, they were submitted past the spans, so no
    /// unit carried their GPU time.
    #[test]
    fn a_readback_and_its_conversion_are_timed_in_the_unit_named() {
        use crate::convert::Nv12Converter;
        use crate::readback::ReadbackRing;
        use crate::{Color, Scene};
        use jstream_media::{ColorInfo, FrameSize};

        let Some(mut opened) = open() else {
            return;
        };
        if !opened.compositor.times_gpu() {
            return;
        }
        let size = FrameSize::new(64, 36).expect("invariant: non-zero");
        let scene = Scene {
            canvas: size,
            background: Color::BLACK,
            items: &[],
        };
        let converter = Nv12Converter::new(opened.compositor.device());
        let planes = converter.planes(size).expect("planes");
        for (tag, planar) in [(51, false), (52, true)] {
            // The composite itself outside any unit: only the way out is timed.
            let output = opened.compositor.render(&scene, false).expect("render");
            let mut ring = ReadbackRing::<u64>::default();
            assert!(opened.compositor.begin_gpu_frame(tag));
            opened.compositor.gpu_unit(Some(9));
            let recorded = if planar {
                let pass = converter
                    .bind(output.texture(), &planes.target(), ColorInfo::default())
                    .expect("bound");
                ring.record_nv12(&opened.compositor, &pass, &planes, tag)
            } else {
                ring.record(&opened.compositor, &output, tag)
            };
            assert!(recorded.expect("recorded"), "a free buffer");
            opened.compositor.gpu_unit(None);
            opened.compositor.end_gpu_frame();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut times = Vec::new();
            while times.is_empty() && Instant::now() < deadline {
                let _ = opened
                    .compositor
                    .device()
                    .poll(wgpu::PollType::wait_indefinitely());
                opened
                    .compositor
                    .gpu_units(|tag, unit, took| times.push((tag, unit, took)));
            }
            assert_eq!(times.len(), 1, "{times:?}");
            assert_eq!((times[0].0, times[0].1), (tag, 9));
        }
    }
}
