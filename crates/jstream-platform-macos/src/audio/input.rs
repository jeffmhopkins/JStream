//! Recording a Core Audio input device (ADR-0063 §4): an IOProc on the HAL device, and a
//! watcher thread per stream.
//!
//! The IOProc runs on the device's real-time IO thread, which Core Audio owns. Each cycle
//! it interleaves the device's input buffers into a buffer allocated before the stream
//! started ([`ChannelMap::gather`]), in chunks of that buffer's size, and hands each chunk
//! to the engine's handler with its device position and its host time on the media
//! clock. It doesn't allocate, lock, block or log (R-RT-001), and it catches any panic,
//! which stops the stream (R-UNSAFE-005).
//!
//! The watcher thread is woken by HAL property listeners (the system default input, and
//! the device's liveness, nominal rate and stream configuration) and every
//! [`WAIT`] anyway. It then checks the stream against the device: a stream on the
//! default input moves to the new default when that changes, as long as it has the same
//! format; a device that is gone, or that changed format, ends the stream with
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
    AudioDirection, AudioInputHandler, AudioPacket, AudioPacketFlags, AudioStream, DeviceId,
    PlatformError,
};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};

use super::hal::{self, SYSTEM, address};
use super::layout::{ChannelMap, InputPlan, SamplePositions, frames_hns};
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

/// Which device a stream records.
#[derive(Clone, Debug)]
pub(super) enum Which {
    /// The system default input, followed when it changes.
    Default,
    /// One device, by UID.
    Device(DeviceId),
}

impl Which {
    /// The device to record now.
    pub(super) fn resolve(&self) -> Result<AudioObjectID, PlatformError> {
        match self {
            Self::Default => {
                hal::default_device(AudioDirection::Input)?.ok_or_else(|| PlatformError::NotFound {
                    kind: "audio device",
                    id: "the default input".into(),
                })
            }
            Self::Device(id) => hal::find(id, AudioDirection::Input),
        }
    }

    fn what(&self) -> String {
        match self {
            Self::Default => "the default audio input".into(),
            Self::Device(id) => format!("audio input {:?}", id.as_str()),
        }
    }
}

/// What the IOProc owns while it runs: only it touches this then, and only the watcher,
/// with the IOProc stopped, otherwise.
struct Rt {
    handler: Box<dyn AudioInputHandler>,
    map: ChannelMap,
    rate: u32,
    /// The handler's interleaved samples, allocated before the stream starts.
    scratch: Vec<f32>,
    positions: SamplePositions,
    clock: MachClock,
}

impl Rt {
    /// One IOProc cycle: hands every input frame to the handler, in chunks of
    /// `scratch`. Returns `false` when the buffer list doesn't have the planned layout,
    /// so the watcher re-plans.
    ///
    /// # Safety
    /// `list` must be the HAL's input buffer list for this cycle: `mNumberBuffers`
    /// buffers laid out contiguously from `mBuffers`, valid for the duration of the call.
    unsafe fn cycle(&mut self, list: NonNull<AudioBufferList>, time: &AudioTimeStamp) -> bool {
        let list = list.as_ptr();
        // SAFETY: `list` is valid for reads (the caller's contract).
        let count = unsafe { (*list).mNumberBuffers } as usize;
        // From the raw pointer, not a reference, so the buffers past the declared one are
        // in bounds of what it points to.
        let first = list
            .cast::<u8>()
            .wrapping_add(std::mem::offset_of!(AudioBufferList, mBuffers))
            .cast::<AudioBuffer>();
        // SAFETY: the caller guarantees `count` contiguous, initialised buffers from there.
        let buffers = unsafe { std::slice::from_raw_parts(first, count) };
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
        let frames = buffers
            .iter()
            .map(|b| b.mDataByteSize as usize / 4 / (b.mNumberChannels as usize).max(1))
            .min()
            .unwrap_or(0);
        let channels = self.map.channels();
        let chunk = (self.scratch.len() / channels.max(1)).max(1);
        let sample_time = time
            .mFlags
            .contains(AudioTimeStampFlags::SampleTimeValid)
            .then_some(time.mSampleTime);
        let host = time
            .mFlags
            .contains(AudioTimeStampFlags::HostTimeValid)
            .then(|| self.clock.from_host_time(time.mHostTime));
        let mut start = 0;
        while start < frames {
            let n = chunk.min(frames - start);
            let out = &mut self.scratch[..n * channels];
            self.map.gather(samples, start, n, out);
            let offset = start as u64;
            // An offset within one IO cycle, far below 2^52, so exact as an f64.
            let at = sample_time.map(|t| t + offset as f64);
            let (device_position, discontinuity) = self.positions.packet(at, n as u64);
            let time = host.map(|h| {
                MediaTime::new(
                    h.ticks().saturating_add(frames_hns(offset, self.rate)),
                    h.timebase(),
                )
            });
            self.handler.on_packet(&AudioPacket {
                samples: out,
                device_position,
                time,
                flags: AudioPacketFlags {
                    discontinuity,
                    silent: false,
                },
            });
            start += n;
        }
        true
    }
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
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
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
            // won't touch `rt` until this cycle ends (see `Shared`'s `Sync`). The list and
            // time stamp are the HAL's, valid for the call, so `cycle`'s contract holds.
            unsafe {
                let (rt, time) = (&mut *shared.rt(), input_time.as_ref());
                rt.cycle(input, time)
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
                "registering the audio input callback",
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
                "starting the audio input",
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

/// The listeners on one device: its liveness, nominal rate and input configuration.
fn device_listeners(device: AudioObjectID, notify: &Notify) -> Vec<Listener> {
    [
        address(
            kAudioDevicePropertyDeviceIsAlive,
            kAudioObjectPropertyScopeGlobal,
        ),
        address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        address(
            kAudioDevicePropertyStreamConfiguration,
            kAudioObjectPropertyScopeInput,
        ),
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
        let target = match &self.which {
            Which::Default => match hal::default_device(AudioDirection::Input)? {
                Some(d) if hal::is_alive(d) => d,
                _ => return Err(self.lost("there is no default input device any more")),
            },
            Which::Device(_) if hal::is_alive(self.device) => self.device,
            Which::Device(_) => return Err(self.lost("the device was removed or stopped working")),
        };
        if target == self.device && !mismatch && !changed {
            return Ok(());
        }
        let plan = hal::input_plan(target)?;
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
            self.device_listeners = device_listeners(target, &self.notify);
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
                    what: "audio input stream",
                    state: "its audio callback panicked, so it was stopped",
                });
            }
            self.reconcile(changed)?;
        }
    }
}

/// Sizes `rt` for `plan` on `device`, before its IOProc starts.
fn prepare(rt: &mut Rt, plan: InputPlan, device: AudioObjectID) {
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

/// A running input stream: dropping it stops the watcher, which stops the IOProc.
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

/// The format `which` would be recorded in now.
pub(super) fn format(which: &Which) -> Result<AudioFormat, PlatformError> {
    Ok(hal::input_plan(which.resolve()?)?.format)
}

/// Starts recording `which`: plans the device, starts its IOProc on this thread, and
/// hands the stream to a watcher thread.
pub(super) fn open(
    which: Which,
    handler: Box<dyn AudioInputHandler>,
) -> Result<Box<dyn AudioStream>, PlatformError> {
    let device = which.resolve()?;
    let plan = hal::input_plan(device)?;
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
    let default_listener = matches!(which, Which::Default).then(|| {
        Listener::add(
            SYSTEM,
            address(
                kAudioHardwarePropertyDefaultInputDevice,
                kAudioObjectPropertyScopeGlobal,
            ),
            &notify,
        )
    });
    let device_listeners = device_listeners(device, &notify);
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
    let thread = thread::Builder::new()
        .name("jstream-coreaudio-in".to_owned())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| watch.run(&rx)));
            watch.stop();
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some(PlatformError::InvalidState {
                    what: "audio input stream",
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
