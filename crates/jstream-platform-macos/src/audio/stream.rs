//! Recording and playing a Core Audio device (ADR-0063 §4): an IOProc on the HAL device,
//! and a watcher thread per stream.
//!
//! The IOProc runs on the device's real-time IO thread, which Core Audio owns. On an
//! input, each cycle it interleaves the device's input buffers into a buffer allocated
//! before the stream started ([`ChannelMap::gather`]) and hands it to the engine's input
//! handler. On an output, it asks the engine's output handler to fill that buffer and
//! spreads it over the device's output buffers ([`ChannelMap::scatter`]), silencing the
//! channels it doesn't use. Either way it works in chunks of that buffer's size, each with
//! its device position and its host time on the media clock (when the first frame was
//! recorded, or is expected to play). It doesn't allocate, lock, block or log (R-RT-001),
//! and it catches any panic, which stops the stream (R-UNSAFE-005).
//!
//! The watcher thread is woken by HAL property listeners (the system default for the
//! stream's direction, and the device's liveness, nominal rate and stream configuration)
//! and every [`WAIT`] anyway. It then checks the stream against the device: a stream on
//! a default device moves to the new default when that changes, as long as it has the
//! same format; a device that is gone, or that changed format, ends the stream with
//! [`PlatformError::DeviceLost`]. Anything it does to the stream it does with the IOProc
//! stopped.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jstream_platform::media::{AudioFormat, MediaTime};
use jstream_platform::{
    AudioDirection, AudioInputHandler, AudioOutputHandler, AudioPacket, AudioPacketFlags,
    AudioRenderBuffer, AudioStream, DeviceId, PlatformError,
};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyScopeGlobal,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};

use super::hal::{self, SYSTEM, address, scope};
use super::layout::{ChannelMap, SamplePositions, StreamPlan, frames_hns};
use crate::MachClock;
use crate::fourcc::os_status;

/// How long the watcher waits for a notification before checking the device anyway, so
/// a device that disappears without one, or a panicked IOProc, is still noticed.
const WAIT: Duration = Duration::from_millis(500);

/// How often stopping checks whether an IOProc cycle is still running.
const STOP_POLL: Duration = Duration::from_micros(200);

/// The smallest chunk the IOProc hands the handler, in frames, when the device reports
/// a smaller IO buffer or none.
const MIN_CHUNK_FRAMES: u32 = 512;

/// `noErr`.
const NO_ERR: i32 = 0;

/// Which device a stream records or plays.
#[derive(Clone, Debug)]
pub(super) struct Which {
    /// Recording or playing.
    pub(super) direction: AudioDirection,
    /// One device by UID, or `None` for the system default, followed when it changes.
    pub(super) device: Option<DeviceId>,
}

impl Which {
    /// The device to use now.
    pub(super) fn resolve(&self) -> Result<AudioObjectID, PlatformError> {
        match &self.device {
            None => hal::default_device(self.direction)?.ok_or_else(|| PlatformError::NotFound {
                kind: "audio device",
                id: format!("the default {}", self.noun()),
            }),
            Some(id) => hal::find(id, self.direction),
        }
    }

    fn noun(&self) -> &'static str {
        match self.direction {
            AudioDirection::Input => "input",
            AudioDirection::Output => "output",
        }
    }

    fn what(&self) -> String {
        match &self.device {
            None => format!("the default audio {}", self.noun()),
            Some(id) => format!("audio {} {:?}", self.noun(), id.as_str()),
        }
    }

    /// The system property naming this direction's default device.
    fn default_selector(&self) -> u32 {
        match self.direction {
            AudioDirection::Input => kAudioHardwarePropertyDefaultInputDevice,
            AudioDirection::Output => kAudioHardwarePropertyDefaultOutputDevice,
        }
    }
}

/// The engine's handler, for the stream's direction.
enum Handler {
    Input(Box<dyn AudioInputHandler>),
    Output(Box<dyn AudioOutputHandler>),
}

impl Handler {
    fn on_error(&mut self, error: &PlatformError) {
        match self {
            Self::Input(h) => h.on_error(error),
            Self::Output(h) => h.on_error(error),
        }
    }
}

/// The buffers of one of the HAL's buffer lists.
///
/// # Safety
/// `list` must be a buffer list the HAL handed this IOProc cycle: `mNumberBuffers` buffers
/// laid out contiguously from `mBuffers`, valid for the duration of the call.
unsafe fn buffers<'a>(list: NonNull<AudioBufferList>) -> &'a [AudioBuffer] {
    let list = list.as_ptr();
    // SAFETY: `list` is valid for reads (the caller's contract).
    let count = unsafe { (*list).mNumberBuffers } as usize;
    // From the raw pointer, not a reference, so the buffers past the declared one are in
    // bounds of what it points to.
    let first = list
        .cast::<u8>()
        .wrapping_add(std::mem::offset_of!(AudioBufferList, mBuffers))
        .cast::<AudioBuffer>();
    // SAFETY: the caller guarantees `count` contiguous, initialised buffers from there.
    unsafe { std::slice::from_raw_parts(first, count) }
}

/// The frames a buffer holds, in `f32` samples of its channels.
fn buffer_frames(b: &AudioBuffer) -> usize {
    b.mDataByteSize as usize / 4 / (b.mNumberChannels as usize).max(1)
}

/// What the IOProc owns while it runs: only it touches this then, and only the watcher,
/// with the IOProc stopped, otherwise.
struct Rt {
    handler: Handler,
    map: ChannelMap,
    rate: u32,
    /// The handler's interleaved samples, allocated before the stream starts.
    scratch: Vec<f32>,
    positions: SamplePositions,
    clock: MachClock,
}

impl Rt {
    /// One IOProc cycle, for the stream's direction. Returns `false` when the device's
    /// buffer list doesn't have the planned layout, so the watcher re-plans.
    ///
    /// # Safety
    /// `input` and `output` must be the HAL's buffer lists for this cycle (see
    /// [`buffers`]); the output buffers are this IOProc's to write during the call.
    unsafe fn cycle(
        &mut self,
        input: NonNull<AudioBufferList>,
        input_time: &AudioTimeStamp,
        output: NonNull<AudioBufferList>,
        output_time: &AudioTimeStamp,
    ) -> bool {
        match self.handler {
            // SAFETY: the caller's contract.
            Handler::Input(_) => unsafe { self.record(buffers(input), input_time) },
            // SAFETY: as above.
            Handler::Output(_) => unsafe { self.play(buffers(output), output_time) },
        }
    }

    /// The sample time and media-clock time of a cycle's first frame.
    fn times(&self, time: &AudioTimeStamp) -> (Option<f64>, Option<MediaTime>) {
        let sample = time
            .mFlags
            .contains(AudioTimeStampFlags::SampleTimeValid)
            .then_some(time.mSampleTime);
        let host = time
            .mFlags
            .contains(AudioTimeStampFlags::HostTimeValid)
            .then(|| self.clock.from_host_time(time.mHostTime));
        (sample, host)
    }

    /// Hands every input frame to the handler, in chunks of `scratch`.
    ///
    /// # Safety
    /// `buffers` are the HAL's input buffers for this cycle.
    unsafe fn record(&mut self, buffers: &[AudioBuffer], time: &AudioTimeStamp) -> bool {
        if !self
            .map
            .matches(buffers.iter().map(|b| b.mNumberChannels as usize))
        {
            return false;
        }
        let samples = |i: usize| -> Option<&[f32]> {
            let b = buffers.get(i)?;
            let data = NonNull::new(b.mData.cast::<f32>())?;
            // SAFETY: the HAL's input buffers hold `mDataByteSize` bytes of native `f32`
            // samples (the virtual format was checked when the stream was planned), valid
            // for the duration of the call and not written by anyone during it.
            Some(unsafe { std::slice::from_raw_parts(data.as_ptr(), b.mDataByteSize as usize / 4) })
        };
        let frames = self.map.frames(|i| buffers.get(i).map_or(0, buffer_frames));
        let (sample_time, host) = self.times(time);
        let channels = self.map.channels();
        let chunk = (self.scratch.len() / channels.max(1)).max(1);
        let Handler::Input(handler) = &mut self.handler else {
            return true;
        };
        let mut start = 0;
        while start < frames {
            let n = chunk.min(frames - start);
            let out = &mut self.scratch[..n * channels];
            self.map.gather(samples, start, n, out);
            let offset = start as u64;
            // An offset within one IO cycle, far below 2^52, so exact as an f64.
            let at = sample_time.map(|t| t + offset as f64);
            let (device_position, discontinuity) = self.positions.packet(at, n as u64);
            handler.on_packet(&AudioPacket {
                samples: out,
                device_position,
                time: offset_time(host, offset, self.rate),
                flags: AudioPacketFlags {
                    discontinuity,
                    silent: false,
                },
            });
            start += n;
        }
        true
    }

    /// Clears the device's output buffers and has the handler fill the channels the map
    /// uses, in chunks of `scratch`.
    ///
    /// # Safety
    /// `buffers` are the HAL's output buffers for this cycle, this IOProc's to write.
    unsafe fn play(&mut self, buffers: &[AudioBuffer], time: &AudioTimeStamp) -> bool {
        for b in buffers {
            if let Some(data) = NonNull::new(b.mData.cast::<u8>()) {
                // SAFETY: the output buffer holds `mDataByteSize` writable bytes for the
                // call; zero bytes are silence in `f32`.
                unsafe { std::ptr::write_bytes(data.as_ptr(), 0, b.mDataByteSize as usize) };
            }
        }
        if !self
            .map
            .matches(buffers.iter().map(|b| b.mNumberChannels as usize))
        {
            // Silence until the watcher re-plans.
            return false;
        }
        let write = |buffer: usize, index: usize, v: f32| {
            let Some(b) = buffers.get(buffer) else {
                return;
            };
            let Some(data) = NonNull::new(b.mData.cast::<f32>()) else {
                return;
            };
            if index < b.mDataByteSize as usize / 4 {
                // SAFETY: `index` is inside the buffer's `mDataByteSize` bytes of native
                // `f32`s, which this IOProc may write during the call.
                unsafe { data.as_ptr().add(index).write(v) };
            }
        };
        let frames = self.map.frames(|i| buffers.get(i).map_or(0, buffer_frames));
        let (sample_time, host) = self.times(time);
        let channels = self.map.channels();
        let chunk = (self.scratch.len() / channels.max(1)).max(1);
        let Handler::Output(handler) = &mut self.handler else {
            return true;
        };
        let mut start = 0;
        while start < frames {
            let n = chunk.min(frames - start);
            let out = &mut self.scratch[..n * channels];
            let offset = start as u64;
            // As in `record`.
            let at = sample_time.map(|t| t + offset as f64);
            let (device_position, _) = self.positions.packet(at, n as u64);
            let mut buffer = AudioRenderBuffer {
                samples: out,
                device_position,
                time: offset_time(host, offset, self.rate),
            };
            handler.render(&mut buffer);
            self.map.scatter(buffer.samples, start, n, write);
            start += n;
        }
        true
    }
}

/// `host` moved on by `frames` frames at `rate`: a later chunk's time in the same cycle.
fn offset_time(host: Option<MediaTime>, frames: u64, rate: u32) -> Option<MediaTime> {
    host.map(|h| {
        MediaTime::new(
            h.ticks().saturating_add(frames_hns(frames, rate)),
            h.timebase(),
        )
    })
}

/// What the IOProc and the watcher share.
struct Shared {
    rt: UnsafeCell<Rt>,
    /// The IOProc may touch `rt`. Cleared, and `busy` seen clear, before anyone else does.
    enabled: AtomicBool,
    /// An IOProc cycle is running.
    busy: AtomicBool,
    /// The IOProc panicked; it touches `rt` no more.
    panicked: AtomicBool,
    /// The IOProc saw a buffer list it wasn't planned for.
    mismatch: AtomicBool,
}

// SAFETY: `rt` is only touched by one thread at a time: by the IOProc while `enabled` is
// set (it sets `busy` first and re-checks `enabled`, both sequentially consistent), and
// by the watcher only after clearing `enabled` and seeing `busy` clear, with the IOProc
// stopped. `Rt` itself is `Send` (its handler is).
unsafe impl Sync for Shared {}

impl Shared {
    /// The IOProc's state, to be borrowed as `&mut` only by the one thread the protocol
    /// above allows: the IOProc while `enabled`, or the watcher once [`Capture::stop`] has
    /// returned.
    fn rt(&self) -> *mut Rt {
        self.rt.get()
    }
}

/// The IOProc.
///
/// # Safety
/// Called by the HAL with `client` the `Shared` [`Capture::start`] registered, which
/// outlives the IOProc's registration.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    // SAFETY: `client` is the `Shared` registered with this IOProc, alive until the IOProc
    // is destroyed and seen not to be running (`Capture::stop`).
    let Some(shared) = (unsafe { client.cast::<Shared>().as_ref() }) else {
        return NO_ERR;
    };
    shared.busy.store(true, Ordering::SeqCst);
    if shared.enabled.load(Ordering::SeqCst) {
        let ok = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `enabled` is set and `busy` was set before checking it, so the watcher
            // won't touch `rt` until this cycle ends (see `Shared`'s `Sync`). The lists and
            // time stamps are the HAL's, valid for the call, so `cycle`'s contract holds.
            unsafe {
                let rt = &mut *shared.rt();
                rt.cycle(input, input_time.as_ref(), output, output_time.as_ref())
            }
        }));
        match ok {
            Ok(true) => {}
            Ok(false) => shared.mismatch.store(true, Ordering::SeqCst),
            Err(_) => {
                shared.enabled.store(false, Ordering::SeqCst);
                shared.panicked.store(true, Ordering::SeqCst);
            }
        }
    }
    shared.busy.store(false, Ordering::SeqCst);
    NO_ERR
}

/// An IOProc registered on a device, started.
struct Capture {
    device: AudioObjectID,
    id: AudioDeviceIOProcID,
}

impl Capture {
    /// Registers [`io_proc`] on `device` with `shared` and starts it.
    fn start(device: AudioObjectID, shared: &Shared) -> Result<Self, PlatformError> {
        let mut id: AudioDeviceIOProcID = None;
        let client = std::ptr::from_ref(shared).cast_mut().cast::<c_void>();
        // SAFETY: `io_proc` matches `AudioDeviceIOProc`; `client` is the `Shared`, which the
        // caller keeps alive until this `Capture` is stopped; `id` is a valid out-slot.
        let status = unsafe {
            AudioDeviceCreateIOProcID(device, Some(io_proc), client, NonNull::from(&mut id))
        };
        if status != NO_ERR {
            return Err(os_status(
                "AudioDeviceCreateIOProcID",
                status,
                "registering the audio callback",
            ));
        }
        let capture = Self { device, id };
        shared.enabled.store(true, Ordering::SeqCst);
        // SAFETY: starts the IOProc just registered on this device.
        let status = unsafe { AudioDeviceStart(device, capture.id) };
        if status != NO_ERR {
            shared.enabled.store(false, Ordering::SeqCst);
            // `capture` drops here, destroying the IOProc.
            return Err(os_status(
                "AudioDeviceStart",
                status,
                "starting the audio device",
            ));
        }
        Ok(capture)
    }

    /// Stops and unregisters the IOProc, and waits until no cycle of it is running, after
    /// which `shared.rt` is the caller's alone.
    fn stop(self, shared: &Shared) {
        shared.enabled.store(false, Ordering::SeqCst);
        drop(self);
        // `AudioDeviceStop` has already waited for the IOProc outside its own thread; this
        // only covers a cycle that was entering as it returned, which ends quickly. Never
        // give up: touching `rt` during a cycle would be unsound.
        while shared.busy.load(Ordering::SeqCst) {
            thread::sleep(STOP_POLL);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: stops and destroys the IOProc this value registered; outside the IOProc,
        // `AudioDeviceStop` returns once the IOProc won't be called again. Failures (a
        // device already gone) leave nothing to undo.
        unsafe {
            let _ = AudioDeviceStop(self.device, self.id);
            let _ = AudioDeviceDestroyIOProcID(self.device, self.id);
        }
    }
}

/// What wakes the watcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// A property it listens to changed.
    Changed,
    /// The stream was dropped.
    Stop,
}

/// The listeners' context: where they send their [`Event::Changed`].
struct Notify {
    tx: Sender<Event>,
}

/// A property listener.
///
/// # Safety
/// Called by the HAL with `client` the `Notify` a [`Listener`] registered.
unsafe extern "C-unwind" fn on_property(
    _object: AudioObjectID,
    _count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client: *mut c_void,
) -> i32 {
    // SAFETY: `client` is the `Notify` the listener was registered with, which outlives
    // the registration (`Watch` drops its listeners first).
    if let Some(notify) = unsafe { client.cast::<Notify>().as_ref() } {
        let _ = catch_unwind(AssertUnwindSafe(|| notify.tx.send(Event::Changed)));
    }
    NO_ERR
}

/// A registered property listener, removed when dropped.
struct Listener {
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    client: *mut c_void,
}

impl Listener {
    /// Listens to `address` on `object`, best effort: without it, the watcher's timeout
    /// still notices the change.
    fn add(
        object: AudioObjectID,
        address: AudioObjectPropertyAddress,
        notify: &Notify,
    ) -> Option<Self> {
        let client = std::ptr::from_ref(notify).cast_mut().cast::<c_void>();
        // SAFETY: `on_property` matches `AudioObjectPropertyListenerProc`; `notify` outlives
        // the registration (see `Watch`); `address` is valid for the call.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&address),
                Some(on_property),
                client,
            )
        };
        (status == NO_ERR).then_some(Self {
            object,
            address,
            client,
        })
    }
}

// SAFETY: `client` is a pointer to a `Notify`, which is `Sync`; the listener is only
// added and removed through it, from whichever thread owns the `Listener`.
unsafe impl Send for Listener {}

impl Drop for Listener {
    fn drop(&mut self) {
        // SAFETY: removes exactly the registration `add` made.
        let _ = unsafe {
            AudioObjectRemovePropertyListener(
                self.object,
                NonNull::from(&self.address),
                Some(on_property),
                self.client,
            )
        };
    }
}

/// The listeners on one device: its liveness, nominal rate and stream configuration in
/// `direction`.
fn device_listeners(
    device: AudioObjectID,
    direction: AudioDirection,
    notify: &Notify,
) -> Vec<Listener> {
    [
        address(
            kAudioDevicePropertyDeviceIsAlive,
            kAudioObjectPropertyScopeGlobal,
        ),
        address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        address(kAudioDevicePropertyStreamConfiguration, scope(direction)),
    ]
    .into_iter()
    .filter_map(|a| Listener::add(device, a, notify))
    .collect()
}

/// The watcher's state. Fields drop in order: the listeners and the IOProc before what
/// they point to.
struct Watch {
    device_listeners: Vec<Listener>,
    _default_listener: Option<Listener>,
    capture: Option<Capture>,
    device: AudioObjectID,
    which: Which,
    format: AudioFormat,
    /// The map the IOProc runs with, so a change can be checked without stopping it.
    map: ChannelMap,
    notify: Box<Notify>,
    shared: Box<Shared>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        // Before the fields drop: the IOProc must be stopped before `shared` goes.
        self.stop();
    }
}

impl Watch {
    fn lost(&self, reason: &str) -> PlatformError {
        PlatformError::DeviceLost {
            what: self.which.what(),
            reason: reason.to_owned(),
        }
    }

    /// Stops the IOProc, if it runs; `shared.rt` is then the watcher's.
    fn stop(&mut self) {
        if let Some(c) = self.capture.take() {
            c.stop(&self.shared);
        }
    }

    /// Checks the stream against the device, moving or re-planning it as needed. With
    /// `changed` (a listener fired), the device is re-planned even if it is the same one.
    fn reconcile(&mut self, changed: bool) -> Result<(), PlatformError> {
        let mismatch = self.shared.mismatch.swap(false, Ordering::SeqCst);
        let direction = self.which.direction;
        let target = match &self.which.device {
            None => match hal::default_device(direction)? {
                Some(d) if hal::is_alive(d) => d,
                _ => {
                    return Err(self.lost(&format!(
                        "there is no default {} device any more",
                        self.which.noun()
                    )));
                }
            },
            Some(_) if hal::is_alive(self.device) => self.device,
            Some(_) => return Err(self.lost("the device was removed or stopped working")),
        };
        if target == self.device && !mismatch && !changed {
            return Ok(());
        }
        let plan = hal::plan(target, direction)?;
        if plan.format != self.format {
            return Err(self.lost(&format!(
                "its format changed from {:?} to {:?}; reopen the stream",
                self.format, plan.format
            )));
        }
        if target == self.device && !mismatch && plan.map == self.map {
            return Ok(());
        }
        self.map = plan.map.clone();
        self.stop();
        // SAFETY: the IOProc is stopped (`stop` above).
        let rt = unsafe { &mut *self.shared.rt() };
        prepare(rt, plan, target);
        if target != self.device {
            rt.positions.new_device();
            self.device_listeners = device_listeners(target, direction, &self.notify);
            self.device = target;
        }
        self.capture = Some(Capture::start(target, &self.shared)?);
        Ok(())
    }

    /// Waits for events until the stream is dropped or fails.
    fn run(&mut self, rx: &Receiver<Event>) -> Result<(), PlatformError> {
        loop {
            let changed = match rx.recv_timeout(WAIT) {
                Ok(Event::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                Ok(Event::Changed) => true,
                Err(RecvTimeoutError::Timeout) => false,
            };
            if self.shared.panicked.load(Ordering::SeqCst) {
                return Err(PlatformError::InvalidState {
                    what: "audio stream",
                    state: "its audio callback panicked, so it was stopped",
                });
            }
            self.reconcile(changed)?;
        }
    }
}

/// Sizes `rt` for `plan` on `device`, before its IOProc starts.
fn prepare(rt: &mut Rt, plan: StreamPlan, device: AudioObjectID) {
    let frames = hal::buffer_frames(device)
        .unwrap_or(0)
        .max(MIN_CHUNK_FRAMES) as usize;
    let needed = frames * plan.map.channels();
    if rt.scratch.len() < needed {
        // Not on the audio thread: the IOProc is stopped.
        rt.scratch.resize(needed, 0.0);
    }
    rt.map = plan.map;
    rt.rate = plan.format.sample_rate();
}

/// A running stream: dropping it stops the watcher, which stops the IOProc.
struct Running {
    format: AudioFormat,
    tx: Sender<Event>,
    thread: Option<JoinHandle<()>>,
}

impl AudioStream for Running {
    fn format(&self) -> AudioFormat {
        self.format
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.tx.send(Event::Stop);
        if let Some(t) = self.thread.take() {
            // The watcher catches panics itself, so joining only waits for it to end.
            let _ = t.join();
        }
    }
}

/// The format `which` would be recorded or played in now.
pub(super) fn format(which: &Which) -> Result<AudioFormat, PlatformError> {
    Ok(hal::plan(which.resolve()?, which.direction)?.format)
}

/// Starts recording `which`.
pub(super) fn open_input(
    which: Which,
    handler: Box<dyn AudioInputHandler>,
) -> Result<Box<dyn AudioStream>, PlatformError> {
    open(which, Handler::Input(handler))
}

/// Starts playing on `which`.
pub(super) fn open_output(
    which: Which,
    handler: Box<dyn AudioOutputHandler>,
) -> Result<Box<dyn AudioStream>, PlatformError> {
    open(which, Handler::Output(handler))
}

/// Plans the device, starts its IOProc on this thread, and hands the stream to a watcher
/// thread.
fn open(which: Which, handler: Handler) -> Result<Box<dyn AudioStream>, PlatformError> {
    let direction = which.direction;
    let device = which.resolve()?;
    let plan = hal::plan(device, direction)?;
    let format = plan.format;
    let plan_map = plan.map.clone();
    let mut rt = Rt {
        handler,
        map: plan.map.clone(),
        rate: format.sample_rate(),
        scratch: Vec::new(),
        positions: SamplePositions::default(),
        clock: MachClock::new()?,
    };
    prepare(&mut rt, plan, device);
    let shared = Box::new(Shared {
        rt: UnsafeCell::new(rt),
        enabled: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        panicked: AtomicBool::new(false),
        mismatch: AtomicBool::new(false),
    });
    let (tx, rx) = mpsc::channel();
    let notify = Box::new(Notify { tx: tx.clone() });
    let default_listener = which.device.is_none().then(|| {
        Listener::add(
            SYSTEM,
            address(which.default_selector(), kAudioObjectPropertyScopeGlobal),
            &notify,
        )
    });
    let device_listeners = device_listeners(device, direction, &notify);
    let capture = Capture::start(device, &shared)?;
    let mut watch = Watch {
        device_listeners,
        _default_listener: default_listener.flatten(),
        capture: Some(capture),
        device,
        which,
        format,
        map: plan_map,
        notify,
        shared,
    };
    let name = match direction {
        AudioDirection::Input => "jstream-coreaudio-in",
        AudioDirection::Output => "jstream-coreaudio-out",
    };
    let thread = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| watch.run(&rx)));
            watch.stop();
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some(PlatformError::InvalidState {
                    what: "audio stream",
                    state: "its watcher thread panicked, so it was stopped",
                }),
            };
            if let Some(e) = error {
                // SAFETY: the IOProc is stopped (`watch.stop()` above).
                let rt = unsafe { &mut *watch.shared.rt() };
                let _ = catch_unwind(AssertUnwindSafe(|| rt.handler.on_error(&e)));
            }
        })
        .map_err(|e| PlatformError::Os {
            call: "std::thread::Builder::spawn",
            code: e.raw_os_error().map(i32::cast_unsigned),
            message: e.to_string(),
        })?;
    Ok(Box::new(Running {
        format,
        tx,
        thread: Some(thread),
    }))
}
