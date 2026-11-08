//! GPU time of effect-pack passes (#349): the guard that turns a pack effect off before
//! the OS resets a GPU it hangs (Windows' TDR, about 2 s) needs to know how long each
//! of its passes runs on the GPU, which a CPU clock can't see.
//!
//! Once a compositor is asked to (`Compositor::time_passes`), on a device with wgpu's
//! `TIMESTAMP_QUERY` feature, every filter application whose
//! pass is a pack's ([`Pass::timed`](crate::filter::Pass)) is bracketed by two empty
//! compute passes that write a timestamp each, so the span covers every pass the filter
//! records in between. The two values are resolved into a buffer and copied to a
//! mappable one in the same submit, and read once the map finishes, a frame or two on,
//! without waiting. A device without the feature (or an application when every slot is
//! still on its way) is not timed. The times go to
//! [`Compositor::gpu_times`](crate::Compositor::gpu_times).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// How many timed applications may be on their way at once.
const SLOTS: u32 = 32;

/// Bytes between two slots in the resolve buffer: `resolve_query_set` writes at an
/// offset that is a multiple of 256.
const STRIDE: u64 = 256;

/// The two timestamps of one slot.
const PAIR: u64 = 16;

const IDLE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

/// Times pack passes on one device.
pub(crate) struct PassTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    /// Nanoseconds per timestamp tick.
    period: f64,
}

struct Slot {
    readback: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// The kind timed, while `busy`; its allocation is reused.
    label: String,
    busy: bool,
}

/// A slot [`PassTimer::begin`] wrote the first timestamp of.
pub(crate) struct Timing(usize);

impl PassTimer {
    /// A timer on `device`, or `None` when it lacks `TIMESTAMP_QUERY`.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("jstream-render pass times"),
            ty: wgpu::QueryType::Timestamp,
            count: SLOTS * 2,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("jstream-render pass times resolve"),
            size: STRIDE * u64::from(SLOTS),
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..SLOTS)
            .map(|_| Slot {
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("jstream-render pass time"),
                    size: PAIR,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(IDLE)),
                label: String::new(),
                busy: false,
            })
            .collect();
        Some(Self {
            queries,
            resolve,
            slots,
            period: f64::from(queue.get_timestamp_period()),
        })
    }

    /// Starts timing `label` in `encoder`: writes the first timestamp, or nothing when
    /// every slot is on its way.
    pub(crate) fn begin(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
    ) -> Option<Timing> {
        let index = self
            .slots
            .iter()
            .position(|slot| !slot.busy && slot.state.load(Ordering::Acquire) == IDLE)?;
        let slot = &mut self.slots[index];
        slot.busy = true;
        slot.label.clear();
        slot.label.push_str(label);
        self.stamp(encoder, index, 0);
        Some(Timing(index))
    }

    /// Ends `timing` in `encoder`: writes the second timestamp and records the copy out.
    pub(crate) fn end(&self, encoder: &mut wgpu::CommandEncoder, timing: &Timing) {
        let index = timing.0;
        self.stamp(encoder, index, 1);
        let first = u32::try_from(index).unwrap_or(0) * 2;
        let at = STRIDE * index as u64;
        encoder.resolve_query_set(&self.queries, first..first + 2, &self.resolve, at);
        encoder.copy_buffer_to_buffer(&self.resolve, at, &self.slots[index].readback, 0, PAIR);
    }

    /// After the submit that carries `timing`: maps its slot.
    pub(crate) fn submitted(&self, timing: Timing) {
        let slot = &self.slots[timing.0];
        slot.state.store(PENDING, Ordering::Release);
        let state = Arc::clone(&slot.state);
        slot.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                state.store(
                    if result.is_ok() { MAPPED } else { FAILED },
                    Ordering::Release,
                );
            });
    }

    /// A timing whose submit never happened (recording failed): frees its slot.
    pub(crate) fn abandon(&mut self, timing: Timing) {
        self.slots[timing.0].busy = false;
    }

    /// Polls `device` without waiting and hands every time measured since the last call
    /// to `measured`, with its kind. Allocates nothing.
    pub(crate) fn collect(
        &mut self,
        device: &wgpu::Device,
        mut measured: impl FnMut(&str, Duration),
    ) {
        if self
            .slots
            .iter()
            .any(|slot| slot.state.load(Ordering::Acquire) == PENDING)
        {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        for slot in &mut self.slots {
            match slot.state.load(Ordering::Acquire) {
                MAPPED => {
                    let time = slot
                        .readback
                        .slice(..)
                        .get_mapped_range()
                        .ok()
                        .and_then(|view| {
                            let start = u64::from_le_bytes(view.get(..8)?.try_into().ok()?);
                            let end = u64::from_le_bytes(view.get(8..16)?.try_into().ok()?);
                            Some(end.saturating_sub(start))
                        });
                    slot.readback.unmap();
                    if let Some(ticks) = time.filter(|_| slot.busy) {
                        // Ticks times nanoseconds per tick, well inside f64 and u64.
                        let nanos = (ticks as f64 * self.period) as u64;
                        measured(&slot.label, Duration::from_nanos(nanos));
                    }
                    slot.busy = false;
                    slot.state.store(IDLE, Ordering::Release);
                }
                FAILED => {
                    slot.busy = false;
                    slot.state.store(IDLE, Ordering::Release);
                }
                _ => {}
            }
        }
    }

    /// Writes timestamp `which` (0 or 1) of slot `index` with an empty compute pass.
    fn stamp(&self, encoder: &mut wgpu::CommandEncoder, index: usize, which: u32) {
        let at = u32::try_from(index).unwrap_or(0) * 2 + which;
        drop(encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("jstream-render pass time"),
            timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                query_set: &self.queries,
                beginning_of_pass_write_index: Some(at),
                end_of_pass_write_index: None,
            }),
        }));
    }
}
