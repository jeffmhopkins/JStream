//! The active plugin on the host's audio thread.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE, clap_event_header, clap_event_param_value,
    clap_input_events, clap_output_events,
};
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};

use crate::plugin::Inner;
use crate::{ClapError, MAX_BLOCK_EVENTS, ParamEvent};

/// An active plugin's audio-thread side: runs it over interleaved stereo blocks.
///
/// Everything it needs is allocated by [`Plugin::activate`](crate::Plugin::activate),
/// so [`process`](Self::process) never allocates, locks or blocks on JStream's side;
/// what the plugin does inside is the plugin's. Dropping it stops processing, on the
/// thread that drops it, which should be the audio thread. It never deactivates or
/// destroys the instance: its [`Plugin`](crate::Plugin) does, on the main thread.
pub struct Processor {
    inner: Arc<Inner>,
    max_frames: usize,
    /// Every input port: the main one carries the block, the rest (sidechains) silence.
    input: Ports,
    /// Every output port: the main one's output is returned, the rest dropped.
    output: Ports,
    /// The parameter events for the block being processed. Reserved for
    /// [`MAX_BLOCK_EVENTS`].
    events_in: Vec<clap_event_param_value>,
    /// The parameter changes the plugin reported in the last block. Reserved for
    /// [`MAX_BLOCK_EVENTS`].
    events_out: Vec<ParamEvent>,
    /// Whether `start_processing` succeeded.
    started: bool,
    /// Frames processed since activation: CLAP's steady time.
    steady_time: i64,
}

impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Processor")
            .field("max_frames", &self.max_frames)
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

// SAFETY: a `Processor` makes only CLAP's audio-thread calls (`start_processing`,
// `process`, `reset`, `stop_processing`), from whichever single thread owns it, which
// CLAP allows to be any thread the host chooses as its audio thread. Its buffers, and
// the raw pointers into them in its port tables, are its own.
unsafe impl Send for Processor {}

/// One direction's audio ports: a buffer for each channel, and the tables CLAP reads.
struct Ports {
    /// `max_frames` samples for each channel of every port in order: the main port's
    /// two first.
    channels: Vec<Vec<f32>>,
    /// Each port's channel count.
    counts: Vec<u32>,
    /// A pointer to each channel's buffer, set by [`point`](Self::point).
    ptrs: Vec<*mut f32>,
    /// Each port's `clap_audio_buffer`, set by [`point`](Self::point).
    buffers: Vec<clap_audio_buffer>,
}

impl Ports {
    fn new(counts: &[u32], max_frames: usize) -> Self {
        let total = counts.iter().map(|&n| n as usize).sum();
        Self {
            channels: (0..total).map(|_| vec![0.0; max_frames]).collect(),
            counts: counts.to_vec(),
            ptrs: vec![std::ptr::null_mut(); total],
            buffers: counts
                .iter()
                .map(|&n| clap_audio_buffer {
                    data32: std::ptr::null_mut(),
                    data64: std::ptr::null_mut(),
                    channel_count: n,
                    latency: 0,
                    constant_mask: 0,
                })
                .collect(),
        }
    }

    /// Points the tables at the channel buffers. Called after the buffers were last
    /// written through `channels` and right before the plugin runs, so the pointers
    /// are fresh. With `silent_extras`, every port after the main one is marked
    /// constant (each of its channels is silence). Never allocates.
    fn point(&mut self, silent_extras: bool) {
        for (ptr, channel) in self.ptrs.iter_mut().zip(&mut self.channels) {
            *ptr = channel.as_mut_ptr();
        }
        let base = self.ptrs.as_mut_ptr();
        let mut at = 0;
        for (i, (buffer, &n)) in self.buffers.iter_mut().zip(&self.counts).enumerate() {
            // In bounds: the counts sum to `ptrs.len()`.
            buffer.data32 = base.wrapping_add(at);
            buffer.constant_mask = if silent_extras && i > 0 { u64::MAX } else { 0 };
            at += n as usize;
        }
    }
}

impl Processor {
    /// A processor for an instance activated for `max_frames`, with these channel
    /// counts for its input and output ports. Port 0 each way is the stereo main port
    /// (checked by `Plugin::activate`).
    pub(crate) fn new(
        inner: Arc<Inner>,
        max_frames: usize,
        inputs: &[u32],
        outputs: &[u32],
    ) -> Self {
        Self {
            inner,
            max_frames,
            input: Ports::new(inputs, max_frames),
            output: Ports::new(outputs, max_frames),
            events_in: Vec::with_capacity(MAX_BLOCK_EVENTS),
            events_out: Vec::with_capacity(MAX_BLOCK_EVENTS),
            started: false,
            steady_time: 0,
        }
    }

    /// The largest block, frames, this processor takes.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Runs the plugin over `input` (interleaved stereo) into `output` (the same
    /// length), with `params` applied at the block's start (at most
    /// [`MAX_BLOCK_EVENTS`]; the rest are ignored). Returns the parameter changes the
    /// plugin made itself in this block (its own editor), at most
    /// [`MAX_BLOCK_EVENTS`]. Never allocates.
    ///
    /// # Errors
    /// [`ClapError::BlockSize`] for an empty, odd, unequal or oversized block, and
    /// [`ClapError::Process`] when the plugin won't start processing or reports an
    /// error. `output` is then unspecified.
    pub fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        params: &[ParamEvent],
    ) -> Result<&[ParamEvent], ClapError> {
        let frames = input.len() / 2;
        if input.is_empty()
            || !input.len().is_multiple_of(2)
            || output.len() != input.len()
            || frames > self.max_frames
        {
            return Err(ClapError::BlockSize {
                frames,
                max: self.max_frames,
            });
        }
        if !self.started {
            let start = self
                .inner
                .raw()
                .start_processing
                .ok_or(ClapError::Process)?;
            // SAFETY: audio thread, the plugin active, not yet processing.
            if !unsafe { start(self.inner.plugin) } {
                return Err(ClapError::Process);
            }
            self.started = true;
        }
        let process = self.inner.raw().process.ok_or(ClapError::Process)?;

        let [in_l, in_r, sidechains @ ..] = self.input.channels.as_mut_slice() else {
            return Err(ClapError::Process);
        };
        for ((frame, l), r) in input
            .chunks_exact(2)
            .zip(in_l.iter_mut())
            .zip(in_r.iter_mut())
        {
            *l = frame[0];
            *r = frame[1];
        }
        // The plugin may have written into its inputs: silence them every block.
        for channel in sidechains {
            channel[..frames].fill(0.0);
        }
        self.events_in.clear();
        self.events_in.extend(
            params
                .iter()
                .take(MAX_BLOCK_EVENTS)
                .map(|p| param_value_event(p.id, p.value)),
        );
        self.events_out.clear();

        self.input.point(true);
        self.output.point(false);
        let in_events = clap_input_events {
            ctx: (&raw const self.events_in).cast_mut().cast::<c_void>(),
            size: Some(events_size),
            get: Some(events_get),
        };
        let out_events = clap_output_events {
            ctx: (&raw mut self.events_out).cast::<c_void>(),
            try_push: Some(events_push),
        };
        let block = clap_process {
            steady_time: self.steady_time,
            frames_count: frames as u32,
            transport: std::ptr::null(),
            audio_inputs: self.input.buffers.as_ptr(),
            audio_outputs: self.output.buffers.as_mut_ptr(),
            // As many as `clap_plugin_audio_ports.count` (at most MAX_PORTS).
            audio_inputs_count: self.input.buffers.len() as u32,
            audio_outputs_count: self.output.buffers.len() as u32,
            in_events: &in_events,
            out_events: &out_events,
        };
        // SAFETY: audio thread, processing started; every pointer in `block` points at
        // buffers and lists that outlive the call: one `clap_audio_buffer` for each of
        // the plugin's ports each way, with one buffer of `max_frames >= frames`
        // samples for each channel the port reported.
        let status = unsafe { process(self.inner.plugin, &block) };
        self.steady_time += frames as i64;
        if status == CLAP_PROCESS_ERROR {
            return Err(ClapError::Process);
        }

        let [out_l, out_r, ..] = self.output.channels.as_slice() else {
            return Err(ClapError::Process);
        };
        for ((frame, l), r) in output.chunks_exact_mut(2).zip(out_l).zip(out_r) {
            frame[0] = *l;
            frame[1] = *r;
        }
        Ok(&self.events_out)
    }

    /// Clears the plugin's audio state (delay lines, envelopes), as for a new stream.
    pub fn reset(&mut self) {
        if let Some(reset) = self.inner.raw().reset {
            // SAFETY: audio thread, the plugin active.
            unsafe { reset(self.inner.plugin) };
        }
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        if self.started
            && let Some(stop) = self.inner.raw().stop_processing
        {
            // SAFETY: audio thread (the owner's), processing started and not stopped.
            unsafe { stop(self.inner.plugin) };
        }
    }
}

/// A CLAP `PARAM_VALUE` event for parameter `id`, at the block's first frame, for any
/// note, port, channel and key.
fn param_value_event(id: u32, value: f64) -> clap_event_param_value {
    clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE,
            flags: 0,
        },
        param_id: id,
        cookie: std::ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    }
}

/// The input list `list` points to.
///
/// # Safety
/// `list` is null or the `clap_input_events` [`Processor::process`] built, whose `ctx`
/// is its `events_in`.
unsafe fn input_list<'a>(list: *const clap_input_events) -> Option<&'a [clap_event_param_value]> {
    // SAFETY: per the caller, `ctx` is a live `Vec` for the duration of `process`.
    unsafe {
        let list = list.as_ref()?;
        Some(
            list.ctx
                .cast::<Vec<clap_event_param_value>>()
                .as_ref()?
                .as_slice(),
        )
    }
}

unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    catch_unwind(|| {
        // SAFETY: the plugin calls back with the list it was given.
        unsafe { input_list(list) }.map_or(0, |e| e.len() as u32)
    })
    .unwrap_or(0)
}

unsafe extern "C" fn events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    catch_unwind(|| {
        // SAFETY: the plugin calls back with the list it was given.
        unsafe { input_list(list) }
            .and_then(|e| e.get(index as usize))
            .map_or(std::ptr::null(), |e| (&raw const e.header))
    })
    .unwrap_or(std::ptr::null())
}

/// Takes a parameter change the plugin reports; ignores every other event type. Never
/// allocates: past the list's reserved room it refuses.
unsafe extern "C" fn events_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the plugin calls back with the list it was given, whose `ctx` is the
        // processor's `events_out`, alive and not otherwise borrowed during `process`.
        let Some(out) = (unsafe { list.as_ref() })
            .and_then(|l| unsafe { l.ctx.cast::<Vec<ParamEvent>>().as_mut() })
        else {
            return false;
        };
        // SAFETY: a non-null event starts with a header (CLAP contract).
        let Some(header) = (unsafe { event.as_ref() }) else {
            return false;
        };
        if header.space_id != CLAP_CORE_EVENT_SPACE_ID
            || header.type_ != CLAP_EVENT_PARAM_VALUE
            || (header.size as usize) < size_of::<clap_event_param_value>()
        {
            // Not a parameter change: accepted and dropped.
            return true;
        }
        if out.len() >= out.capacity() {
            return false;
        }
        // SAFETY: the header says it is a whole `clap_event_param_value`.
        let value = unsafe { &*event.cast::<clap_event_param_value>() };
        out.push(ParamEvent {
            id: value.param_id,
            value: value.value,
        });
        true
    }))
    .unwrap_or(false)
}
