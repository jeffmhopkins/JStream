//! The stream threads: one per open stream, as docs/ARCHITECTURE.md#threading says.
//!
//! Each thread initialises COM, registers with MMCSS "Pro Audio", opens its client (COM
//! objects never leave the thread), reports the format or the error back to the opener,
//! and then loops: wait for the WASAPI event, move every ready buffer between the device
//! and the engine's handler, repeat until the stream is dropped. In that loop it does not
//! allocate, lock or log (R-RT-001): the sample buffer is allocated once, before the
//! stream starts. A stream on a default device follows the system default when it
//! changes, as long as the new device has the same format.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

use jstream_platform::media::{AudioFormat, MediaTime, Timebase};
use jstream_platform::{
    AudioInputHandler, AudioInputTarget, AudioOutputHandler, AudioOutputTarget, AudioPacket,
    AudioRenderBuffer, AudioStream, PlatformError,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_INVALIDATED, EDataFlow, IAudioCaptureClient, IAudioRenderClient,
    IMMDeviceEnumerator, eCapture, eRender,
};

use super::device::{self, DefaultWatch, Mode, Session};
use super::format::{Positions, packet_meta, read_f32_le, write_f32_le};
use crate::com::{Com, Event, Mmcss, OsResult, Woke, os_error, wait_any};

/// How long a stream thread waits for its event before checking the device anyway, so a
/// device that disappears without a notification is still noticed.
const WAIT_MS: u32 = 500;

/// A running stream: its thread, and the event that stops it.
struct Running {
    format: AudioFormat,
    stop: Arc<Event>,
    thread: Option<JoinHandle<()>>,
}

impl AudioStream for Running {
    fn format(&self) -> AudioFormat {
        self.format
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(t) = self.thread.take() {
            // The thread catches panics itself, so joining only waits for it to end.
            let _ = t.join();
        }
    }
}

/// What the stream thread reports once it has opened its client, or failed to.
type Opened = SyncSender<Result<AudioFormat, PlatformError>>;

/// Starts a stream thread running `body` and waits for it to report its format.
fn spawn(
    name: &str,
    body: impl FnOnce(&Event, &Opened) + Send + 'static,
) -> Result<Box<dyn AudioStream>, PlatformError> {
    let stop = Arc::new(Event::new()?);
    let thread_stop = Arc::clone(&stop);
    let (tx, rx) = sync_channel(1);
    let thread = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || body(&thread_stop, &tx))
        .map_err(|e| PlatformError::Os {
            call: "std::thread::Builder::spawn",
            code: e.raw_os_error().map(i32::cast_unsigned),
            message: e.to_string(),
        })?;
    match rx.recv() {
        Ok(Ok(format)) => Ok(Box::new(Running {
            format,
            stop,
            thread: Some(thread),
        })),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(PlatformError::Os {
                call: "WASAPI stream thread",
                code: None,
                message: "the thread ended before opening its stream".into(),
            })
        }
    }
}

/// Which endpoint a stream uses, and how to find it again.
enum Endpoint {
    /// The system default for this flow, followed when it changes.
    Default(EDataFlow),
    /// One device.
    Device(jstream_platform::DeviceId, EDataFlow),
    /// Loopback of a process tree.
    Process(u32),
}

/// A resolved stream target.
struct Source {
    endpoint: Endpoint,
    mode: Mode,
    /// Names the target in a `DeviceLost` error.
    what: String,
}

impl Source {
    fn input(target: &AudioInputTarget) -> Result<Self, PlatformError> {
        let (endpoint, mode) = match target {
            AudioInputTarget::DefaultInput => (Endpoint::Default(eCapture), Mode::Capture),
            AudioInputTarget::Input(id) => (Endpoint::Device(id.clone(), eCapture), Mode::Capture),
            AudioInputTarget::DefaultOutputLoopback => (Endpoint::Default(eRender), Mode::Loopback),
            AudioInputTarget::OutputLoopback(id) => {
                (Endpoint::Device(id.clone(), eRender), Mode::Loopback)
            }
            AudioInputTarget::ProcessLoopback { pid } => (Endpoint::Process(*pid), Mode::Loopback),
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("audio input target {other:?}"),
                });
            }
        };
        Ok(Self {
            endpoint,
            mode,
            what: format!("audio input {target:?}"),
        })
    }

    fn output(target: &AudioOutputTarget) -> Result<Self, PlatformError> {
        let endpoint = match target {
            AudioOutputTarget::DefaultOutput => Endpoint::Default(eRender),
            AudioOutputTarget::Output(id) => Endpoint::Device(id.clone(), eRender),
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("audio output target {other:?}"),
                });
            }
        };
        Ok(Self {
            endpoint,
            mode: Mode::Render,
            what: format!("audio output {target:?}"),
        })
    }

    /// The flow whose default this stream follows, if it follows one.
    fn follows(&self) -> Option<EDataFlow> {
        match self.endpoint {
            Endpoint::Default(flow) => Some(flow),
            Endpoint::Device(..) | Endpoint::Process(_) => None,
        }
    }

    fn open(&self, e: &IMMDeviceEnumerator) -> Result<Session, PlatformError> {
        match &self.endpoint {
            Endpoint::Default(flow) => {
                device::open_endpoint(&device::default_endpoint(e, *flow)?, self.mode)
            }
            Endpoint::Device(id, flow) => {
                device::open_endpoint(&device::by_id(e, id, *flow)?, self.mode)
            }
            Endpoint::Process(pid) => device::open_process(*pid),
        }
    }

    fn lost(&self, reason: &str) -> PlatformError {
        PlatformError::DeviceLost {
            what: self.what.clone(),
            reason: reason.to_owned(),
        }
    }
}

/// Everything a stream thread sets up before its loop.
///
/// Fields drop in order, so the COM objects go before the MMCSS and COM guards.
struct Setup {
    source: Source,
    watch: Option<DefaultWatch>,
    enumerator: IMMDeviceEnumerator,
    _mmcss: Mmcss,
    _com: Com,
}

impl Setup {
    fn new(source: Source) -> Result<Self, PlatformError> {
        let com = Com::init()?;
        let mmcss = Mmcss::pro_audio();
        let enumerator = device::enumerator()?;
        // Best effort: without the watch, a default stream still moves when its device is
        // removed, just not when the user picks another default.
        let watch = source
            .follows()
            .and_then(|flow| DefaultWatch::new(&enumerator, flow).ok());
        Ok(Self {
            source,
            watch,
            enumerator,
            _mmcss: mmcss,
            _com: com,
        })
    }

    /// Waits for the stop event (`Ok(None)`), the client's event, a default-device change
    /// or the timeout. `Some(true)` means the default device changed.
    fn wait(&self, stop: &Event, session: &Session) -> Result<Option<bool>, PlatformError> {
        let woke = match &self.watch {
            Some(w) => wait_any(&[stop, &session.event, &w.event], WAIT_MS)?,
            None => wait_any(&[stop, &session.event], WAIT_MS)?,
        };
        Ok(match woke {
            Woke::Index(0) => None,
            Woke::Index(2) => Some(true),
            Woke::Index(_) | Woke::Timeout => Some(false),
        })
    }

    /// Reopens a default stream on the current default device, which must have the
    /// stream's format.
    fn reopen(&self, old: &Session) -> Result<Session, PlatformError> {
        // SAFETY: plain COM call; stopping a stream on a removed device is harmless.
        let _ = unsafe { old.client.Stop() };
        let new = self.source.open(&self.enumerator).map_err(|e| {
            self.source.lost(&format!(
                "the default device changed and the new one failed to open: {e}"
            ))
        })?;
        if new.format != old.format {
            return Err(self.source.lost(
                "the default device changed to one with a different format; reopen the stream",
            ));
        }
        Ok(new)
    }
}

/// A WASAPI call failed in the loop: either the device went away, or something else.
fn classify(source: &Source, call: &'static str, e: &windows::core::Error) -> Failure {
    if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        if source.follows().is_some() {
            Failure::Moved
        } else {
            Failure::Fatal(source.lost("the device was removed or disabled"))
        }
    } else {
        Failure::Fatal(os_error(call, e))
    }
}

/// Why one pass of a loop stopped early.
enum Failure {
    /// The default device changed or went away; follow it.
    Moved,
    /// The stream ends with this error.
    Fatal(PlatformError),
}

/// Runs a stream's loop, catching a panic in it (the handler's included) so the thread
/// ends cleanly: COM objects are released on this thread and the panic reaches no OS
/// frame. Returns the error that ended the stream, if one did; the caller reports it to
/// the handler's `on_error`, itself guarded.
fn guarded(run: impl FnOnce() -> Result<(), PlatformError>) -> Option<PlatformError> {
    match catch_unwind(AssertUnwindSafe(run)) {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some(PlatformError::InvalidState {
            what: "audio stream",
            state: "its thread panicked, so it was stopped",
        }),
    }
}

/// Starts recording `target` on a new thread.
pub(crate) fn open_input(
    target: &AudioInputTarget,
    handler: Box<dyn AudioInputHandler>,
) -> Result<Box<dyn AudioStream>, PlatformError> {
    let source = Source::input(target)?;
    spawn("jstream-wasapi-in", move |stop, opened| {
        input_thread(source, handler, stop, opened);
    })
}

fn input_thread(
    source: Source,
    mut handler: Box<dyn AudioInputHandler>,
    stop: &Event,
    opened: &Opened,
) {
    let start = Setup::new(source).and_then(|setup| {
        let session = setup.source.open(&setup.enumerator)?;
        Ok((setup, session))
    });
    let (setup, session) = match start {
        Ok(v) => v,
        Err(e) => {
            let _ = opened.send(Err(e));
            return;
        }
    };
    if opened.send(Ok(session.format)).is_err() {
        return;
    }
    if let Some(e) = guarded(|| run_input(&setup, session, &mut *handler, stop)) {
        let _ = catch_unwind(AssertUnwindSafe(|| handler.on_error(&e)));
    }
}

/// Starts a capture client and returns its capture service.
fn start_capture(session: &Session) -> Result<IAudioCaptureClient, PlatformError> {
    // SAFETY: plain COM calls on an initialised client.
    let capture: IAudioCaptureClient = unsafe { session.client.GetService() }
        .os("IAudioClient::GetService(IAudioCaptureClient)")?;
    // SAFETY: as above.
    unsafe { session.client.Start() }.os("IAudioClient::Start")?;
    Ok(capture)
}

fn run_input(
    setup: &Setup,
    mut session: Session,
    handler: &mut dyn AudioInputHandler,
    stop: &Event,
) -> Result<(), PlatformError> {
    let mut state = InputState {
        format: session.format,
        timebase: crate::clock::timebase(),
        samples: vec![0.0f32; session.buffer_frames as usize * session.format.channels()],
        positions: Positions::default(),
        gap: false,
    };
    let mut capture = start_capture(&session)?;
    while let Some(default_changed) = setup.wait(stop, &session)? {
        let pass = if default_changed {
            Err(Failure::Moved)
        } else {
            state.drain(&setup.source, &capture, handler)
        };
        match pass {
            Ok(()) => {}
            Err(Failure::Fatal(e)) => return Err(e),
            Err(Failure::Moved) => {
                session = setup.reopen(&session)?;
                let needed = session.buffer_frames as usize * state.format.channels();
                if state.samples.len() < needed {
                    // Not the steady state: the stream is moving to a new device.
                    state.samples.resize(needed, 0.0);
                }
                state.positions.new_device();
                state.gap = true;
                capture = start_capture(&session)?;
            }
        }
    }
    // SAFETY: plain COM call on a started client.
    let _ = unsafe { session.client.Stop() };
    Ok(())
}

/// An input stream's loop state, allocated before the stream starts.
struct InputState {
    format: AudioFormat,
    timebase: Timebase,
    /// The handler's samples, sized for the device buffer.
    samples: Vec<f32>,
    positions: Positions,
    /// The next packet follows a move to a new device.
    gap: bool,
}

impl InputState {
    /// Hands every ready capture packet to the handler.
    fn drain(
        &mut self,
        source: &Source,
        capture: &IAudioCaptureClient,
        handler: &mut dyn AudioInputHandler,
    ) -> Result<(), Failure> {
        let channels = self.format.channels();
        loop {
            // SAFETY: plain COM call.
            let next = unsafe { capture.GetNextPacketSize() }
                .map_err(|e| classify(source, "IAudioCaptureClient::GetNextPacketSize", &e))?;
            if next == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let (mut frames, mut flags, mut device_pos, mut qpc) = (0u32, 0u32, 0u64, 0u64);
            // SAFETY: every out-pointer is a valid local.
            unsafe {
                capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut device_pos),
                    Some(&mut qpc),
                )
            }
            .map_err(|e| classify(source, "IAudioCaptureClient::GetBuffer", &e))?;
            let n = frames as usize * channels;
            if self.samples.len() < n {
                // WASAPI never hands more than the buffer the vector was sized for; this only
                // keeps a misbehaving driver from truncating data.
                self.samples.resize(n, 0.0);
            }
            let meta = packet_meta(flags, qpc, self.gap);
            let out = &mut self.samples[..n];
            if meta.flags.silent || data.is_null() {
                out.fill(0.0);
            } else {
                // SAFETY: GetBuffer returned `frames` frames of `channels` f32 samples at
                // `data`, valid until the ReleaseBuffer below; the slice is not used after it.
                let bytes = unsafe { std::slice::from_raw_parts(data, n * 4) };
                read_f32_le(bytes, out);
            }
            // SAFETY: releases exactly the frames GetBuffer returned.
            unsafe { capture.ReleaseBuffer(frames) }
                .map_err(|e| classify(source, "IAudioCaptureClient::ReleaseBuffer", &e))?;
            let device_position = self.positions.packet(device_pos, frames);
            self.gap = false;
            handler.on_packet(&AudioPacket {
                samples: &self.samples[..n],
                device_position,
                time: meta.time_hns.map(|t| MediaTime::new(t, self.timebase)),
                flags: meta.flags,
            });
        }
    }
}

/// Starts playing on `target` on a new thread.
pub(crate) fn open_output(
    target: &AudioOutputTarget,
    handler: Box<dyn AudioOutputHandler>,
) -> Result<Box<dyn AudioStream>, PlatformError> {
    let source = Source::output(target)?;
    spawn("jstream-wasapi-out", move |stop, opened| {
        output_thread(source, handler, stop, opened);
    })
}

fn output_thread(
    source: Source,
    mut handler: Box<dyn AudioOutputHandler>,
    stop: &Event,
    opened: &Opened,
) {
    let start = Setup::new(source).and_then(|setup| {
        let session = setup.source.open(&setup.enumerator)?;
        Ok((setup, session))
    });
    let (setup, session) = match start {
        Ok(v) => v,
        Err(e) => {
            let _ = opened.send(Err(e));
            return;
        }
    };
    if opened.send(Ok(session.format)).is_err() {
        return;
    }
    if let Some(e) = guarded(|| run_output(&setup, session, &mut *handler, stop)) {
        let _ = catch_unwind(AssertUnwindSafe(|| handler.on_error(&e)));
    }
}

/// Fills the whole free buffer, then starts the client: WASAPI wants a render buffer
/// filled before `Start`.
fn start_render(
    setup: &Setup,
    session: &Session,
    samples: &mut Vec<f32>,
    written: &mut u64,
    handler: &mut dyn AudioOutputHandler,
) -> Result<IAudioRenderClient, Failure> {
    // SAFETY: plain COM call on an initialised client.
    let render: IAudioRenderClient = unsafe { session.client.GetService() }.map_err(|e| {
        classify(
            &setup.source,
            "IAudioClient::GetService(IAudioRenderClient)",
            &e,
        )
    })?;
    fill(&setup.source, session, &render, samples, written, handler)?;
    // SAFETY: plain COM call on an initialised client.
    unsafe { session.client.Start() }
        .map_err(|e| classify(&setup.source, "IAudioClient::Start", &e))?;
    Ok(render)
}

fn run_output(
    setup: &Setup,
    mut session: Session,
    handler: &mut dyn AudioOutputHandler,
    stop: &Event,
) -> Result<(), PlatformError> {
    let channels = session.format.channels();
    let mut samples = vec![0.0f32; session.buffer_frames as usize * channels];
    let mut written = 0u64;
    let mut render = None;
    let mut moved = false;
    loop {
        if moved {
            session = setup.reopen(&session)?;
            let needed = session.buffer_frames as usize * channels;
            if samples.len() < needed {
                // Not the steady state: the stream is moving to a new device.
                samples.resize(needed, 0.0);
            }
            render = None;
            moved = false;
        }
        let pass = match &render {
            None => start_render(setup, &session, &mut samples, &mut written, handler).map(|r| {
                render = Some(r);
            }),
            Some(r) => match setup.wait(stop, &session)? {
                None => break,
                Some(true) => Err(Failure::Moved),
                Some(false) => fill(
                    &setup.source,
                    &session,
                    r,
                    &mut samples,
                    &mut written,
                    handler,
                ),
            },
        };
        match pass {
            Ok(()) => {}
            Err(Failure::Moved) => moved = true,
            Err(Failure::Fatal(e)) => return Err(e),
        }
    }
    // SAFETY: plain COM call on a started client.
    let _ = unsafe { session.client.Stop() };
    Ok(())
}

/// Asks the handler for as many frames as the device buffer has room for.
fn fill(
    source: &Source,
    session: &Session,
    render: &IAudioRenderClient,
    samples: &mut Vec<f32>,
    written: &mut u64,
    handler: &mut dyn AudioOutputHandler,
) -> Result<(), Failure> {
    // SAFETY: plain COM call on an initialised client.
    let padding = unsafe { session.client.GetCurrentPadding() }
        .map_err(|e| classify(source, "IAudioClient::GetCurrentPadding", &e))?;
    let free = session.buffer_frames.saturating_sub(padding);
    if free == 0 {
        return Ok(());
    }
    // SAFETY: asks for `free` frames, which the padding check says are available.
    let data = unsafe { render.GetBuffer(free) }
        .map_err(|e| classify(source, "IAudioRenderClient::GetBuffer", &e))?;
    let n = free as usize * session.format.channels();
    if samples.len() < n {
        // `free` never exceeds the buffer the vector was sized for; see `drain`.
        samples.resize(n, 0.0);
    }
    let out = &mut samples[..n];
    out.fill(0.0);
    handler.render(&mut AudioRenderBuffer {
        samples: out,
        device_position: *written,
        time: None,
    });
    if !data.is_null() {
        // SAFETY: GetBuffer returned room for `free` frames of `channels` f32 samples at
        // `data`, writable until the ReleaseBuffer below; the slice is not used after it.
        let bytes = unsafe { std::slice::from_raw_parts_mut(data, n * 4) };
        write_f32_le(&samples[..n], bytes);
    }
    // SAFETY: releases exactly the frames GetBuffer returned, with no flags.
    unsafe { render.ReleaseBuffer(free, 0) }
        .map_err(|e| classify(source, "IAudioRenderClient::ReleaseBuffer", &e))?;
    *written += u64::from(free);
    Ok(())
}
