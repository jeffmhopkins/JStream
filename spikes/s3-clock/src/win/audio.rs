//! WASAPI: opening the capture streams and the silent keep-alive render stream, and
//! the capture threads.
//!
//! Every stream is opened, run and closed on its own thread (COM objects never cross
//! threads), registered with MMCSS "Pro Audio". The thread sends its [`StreamInfo`] (or
//! the error that stopped it opening) back through a channel, then loops until `stop`:
//! wait for the WASAPI event, drain every packet with `GetBuffer`, copy and convert it,
//! `ReleaseBuffer`, then hand it to [`StreamState::on_packet`]. A failure inside the
//! loop ends that stream and is reported; it never panics.

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, EDataFlow,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{BLOB, CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, IUnknown, Interface, PWSTR, Ref, implement};

use super::sys::{Com, Mmcss};
use crate::err::{Context, Error, Result, bail};
use crate::stream::{Packet, Role, SampleFormat, Snapshot, StreamInfo, StreamState};

/// Requested shared-mode buffer: 200 ms, so a late thread doesn't lose data.
const BUFFER_HNS: i64 = 2_000_000;
/// How long a capture thread waits for the WASAPI event before counting a timeout.
const WAIT_MS: u32 = 200;
/// How often a capture thread publishes its snapshot.
const PUBLISH_EVERY: Duration = Duration::from_millis(250);
/// Process-loopback capture format (it has no mix format of its own).
const PROCESS_RATE: u32 = 48_000;
const PROCESS_CHANNELS: u16 = 2;

/// What to capture.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// The default capture endpoint (console role).
    Mic,
    /// Loopback of the default render endpoint: everything the desktop plays.
    Loopback,
    /// Loopback of one process tree (Windows 10 2004+).
    Process(u32),
}

/// An auto-reset Win32 event, closed on drop.
struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self> {
        // SAFETY: plain call creating an unnamed auto-reset event.
        let h = unsafe { CreateEventW(None, false, false, None) }.context("CreateEventW")?;
        Ok(Self(h))
    }

    /// Waits up to `ms`; true when signalled.
    fn wait(&self, ms: u32) -> Result<bool> {
        // SAFETY: the handle is a live event owned by `self`.
        let r = unsafe { WaitForSingleObject(self.0, ms) };
        if r == WAIT_OBJECT_0 {
            Ok(true)
        } else if r == WAIT_TIMEOUT {
            Ok(false)
        } else {
            bail!("WaitForSingleObject returned {:#x}", r.0)
        }
    }
}

// SAFETY: an event handle is a kernel object; signalling, waiting and closing it are
// thread-safe. The process-loopback completion handler signals it from a COM thread.
unsafe impl Send for Event {}
// SAFETY: as above; `&Event` only allows signalling and waiting.
unsafe impl Sync for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateEventW and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A `*mut WAVEFORMATEX` owned by COM; freed on drop.
struct CoFormat(*mut WAVEFORMATEX);

impl Drop for CoFormat {
    fn drop(&mut self) {
        // SAFETY: the pointer came from GetMixFormat, which allocates with CoTaskMemAlloc.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

/// A wave format copied out of COM memory, plus what the spike needs from it.
struct Format {
    /// The `WAVEFORMATEX` (and its `cbSize` extension) as bytes, for `Initialize`.
    bytes: Vec<u8>,
    rate: u32,
    channels: u16,
    sample: SampleFormat,
    bits: u16,
    valid_bits: u16,
}

impl Format {
    /// Reads a format allocated by WASAPI.
    ///
    /// # Safety
    /// `p` must point to a valid `WAVEFORMATEX` followed by `cbSize` extension bytes.
    unsafe fn read(p: *const WAVEFORMATEX) -> Result<Self> {
        if p.is_null() {
            bail!("null wave format");
        }
        // SAFETY: the caller guarantees `p` is valid; WAVEFORMATEX is packed, so read
        // it unaligned.
        let wf = unsafe { p.read_unaligned() };
        let total = std::mem::size_of::<WAVEFORMATEX>() + usize::from(wf.cbSize);
        // SAFETY: the caller guarantees `total` bytes are readable at `p`.
        let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), total) }.to_vec();
        let tag = u32::from(wf.wFormatTag);
        let bits = wf.wBitsPerSample;
        let (is_float, is_pcm, valid_bits) = if tag == WAVE_FORMAT_EXTENSIBLE
            && usize::from(wf.cbSize)
                >= std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()
        {
            // SAFETY: cbSize says the extensible fields are present.
            let ext = unsafe { p.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
            let sub = ext.SubFormat;
            // SAFETY: every member of the `Samples` union is a u16.
            let valid = unsafe { ext.Samples.wValidBitsPerSample };
            (
                sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
                sub == KSDATAFORMAT_SUBTYPE_PCM,
                if valid == 0 { bits } else { valid },
            )
        } else {
            (tag == WAVE_FORMAT_IEEE_FLOAT, tag == 1, bits)
        };
        let sample = match (is_float, is_pcm, bits) {
            (true, _, 32) => SampleFormat::F32,
            (_, true, 16) => SampleFormat::I16,
            (_, true, 24) => SampleFormat::I24,
            (_, true, 32) => SampleFormat::I32,
            _ => bail!("unsupported mix format: tag {tag:#x}, {bits} bits"),
        };
        if wf.nChannels == 0 || wf.nSamplesPerSec == 0 {
            bail!("mix format has no channels or no rate");
        }
        let (block, channels) = (wf.nBlockAlign, wf.nChannels);
        if usize::from(block) != usize::from(channels) * sample.bytes() {
            bail!(
                "unexpected block align {block} for {channels} channels of {}",
                sample.name()
            );
        }
        Ok(Self {
            bytes,
            rate: wf.nSamplesPerSec,
            channels: wf.nChannels,
            sample,
            bits,
            valid_bits,
        })
    }

    /// 32-bit float, interleaved: the process-loopback format.
    fn float(rate: u32, channels: u16) -> Self {
        let block = channels * 4;
        let wf = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        // SAFETY: WAVEFORMATEX is plain data with no padding (packed(1)); viewing a
        // local as bytes is sound.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&wf as *const WAVEFORMATEX).cast::<u8>(),
                std::mem::size_of::<WAVEFORMATEX>(),
            )
        }
        .to_vec();
        Self {
            bytes,
            rate,
            channels,
            sample: SampleFormat::F32,
            bits: 32,
            valid_bits: 32,
        }
    }

    fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.bytes.as_ptr().cast()
    }
}

/// Takes a COM-allocated wide string.
fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is a valid, null-terminated string from COM.
    let s = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: the string was allocated with CoTaskMemAlloc and is freed once.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

fn default_device(flow: EDataFlow) -> Result<IMMDevice> {
    // SAFETY: plain COM activation on a COM-initialised thread.
    let e: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("create MMDeviceEnumerator")?;
    // SAFETY: plain COM call.
    unsafe { e.GetDefaultAudioEndpoint(flow, eConsole) }.context("GetDefaultAudioEndpoint")
}

/// The endpoint's friendly name and ID (best effort).
fn describe(device: &IMMDevice) -> (String, String) {
    // SAFETY: plain COM call; the string is taken and freed by take_pwstr.
    let id = unsafe { device.GetId() }
        .map(take_pwstr)
        .unwrap_or_default();
    let name = (|| -> Result<String> {
        // SAFETY: plain COM calls; the PROPVARIANT is cleared by its Drop.
        let store = unsafe { device.OpenPropertyStore(STGM_READ) }?;
        // SAFETY: as above.
        let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }?;
        // SAFETY: `value` is a valid PROPVARIANT; the result is freed by take_pwstr.
        let s = unsafe { PropVariantToStringAlloc(&value) }?;
        Ok(take_pwstr(s))
    })()
    .unwrap_or_else(|e| format!("(name unavailable: {e})"));
    (name, id)
}

/// An initialised, not yet started, event-driven shared-mode stream.
struct Opened {
    client: IAudioClient,
    event: Event,
    format: Format,
    info: StreamInfo,
}

fn initialise(
    client: IAudioClient,
    format: Format,
    flags: u32,
    role: Role,
    name: String,
    id: String,
) -> Result<Opened> {
    // SAFETY: `format` outlives the call and points to a valid WAVEFORMATEX.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            BUFFER_HNS,
            0,
            format.as_ptr(),
            None,
        )
    }
    .context("IAudioClient::Initialize")?;
    let event = Event::new()?;
    // SAFETY: the event lives as long as the client (both in `Opened`).
    unsafe { client.SetEventHandle(event.0) }.context("SetEventHandle")?;
    // SAFETY: plain COM calls on an initialised client.
    let buffer_frames = unsafe { client.GetBufferSize() }.unwrap_or(0);
    let mut period = 0i64;
    // SAFETY: `period` is a valid out-pointer.
    let _ = unsafe { client.GetDevicePeriod(Some(&mut period), None) };
    let info = StreamInfo {
        role,
        device_name: name,
        device_id: id,
        rate: format.rate,
        channels: format.channels,
        format: format.sample,
        bits: format.bits,
        valid_bits: format.valid_bits,
        buffer_frames,
        period_hns: period,
    };
    Ok(Opened {
        client,
        event,
        format,
        info,
    })
}

/// Opens the default endpoint of `flow` with its mix format.
fn open_endpoint(flow: EDataFlow, loopback: bool, role: Role) -> Result<Opened> {
    let device = default_device(flow)?;
    let (name, id) = describe(&device);
    // SAFETY: plain COM activation.
    let client: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.context("activate IAudioClient")?;
    // SAFETY: plain COM call; the returned memory is owned by CoFormat.
    let mix = CoFormat(unsafe { client.GetMixFormat() }.context("GetMixFormat")?);
    // SAFETY: GetMixFormat returned a valid format.
    let format = unsafe { Format::read(mix.0) }?;
    let flags = if loopback {
        AUDCLNT_STREAMFLAGS_LOOPBACK
    } else {
        0
    };
    initialise(client, format, flags, role, name, id)
}

/// Signals an event when `ActivateAudioInterfaceAsync` completes.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated {
    event: Arc<Event>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _op: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // SAFETY: the event is kept alive by the Arc this handler holds.
        unsafe { SetEvent(self.event.0) }
    }
}

/// Opens process loopback for `pid` (and its children) through the virtual
/// process-loopback device.
fn open_process(pid: u32) -> Result<Opened> {
    let event = Arc::new(Event::new()?);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated {
        event: Arc::clone(&event),
    }
    .into();
    // Boxed so it can be leaked if activation times out while still holding the pointer.
    let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    });
    // A VT_BLOB pointing at `params`. ManuallyDrop: PropVariantClear would try to free
    // the blob, which Rust owns.
    let prop = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&mut *params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
                    },
                },
            }),
        },
    });
    // SAFETY: `prop` (a plain struct) and the boxed `params` it points to stay alive
    // until the activation has completed; on a timeout `params` is leaked below.
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*prop as *const PROPVARIANT),
            &handler,
        )
    }
    .context("ActivateAudioInterfaceAsync")?;
    if !event.wait(5_000)? {
        std::mem::forget(params);
        bail!("process loopback activation did not complete within 5 s");
    }
    drop(params);
    let mut hr = HRESULT(0);
    let mut unk: Option<IUnknown> = None;
    // SAFETY: both out-pointers are valid locals.
    unsafe { op.GetActivateResult(&mut hr, &mut unk) }.context("GetActivateResult")?;
    hr.ok()
        .context(format!("activate process loopback for pid {pid}"))?;
    let client: IAudioClient = unk
        .context("activation returned no interface")?
        .cast()
        .context("cast to IAudioClient")?;
    let format = Format::float(PROCESS_RATE, PROCESS_CHANNELS);
    initialise(
        client,
        format,
        AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        Role::ProcessLoopback,
        format!("process loopback, pid {pid} and children"),
        String::new(),
    )
}

/// What a capture thread reports when it ends.
pub struct Outcome {
    pub snap: Snapshot,
    pub mmcss: bool,
    pub wait_timeouts: u64,
    pub error: Option<String>,
}

/// A running capture stream.
pub struct Capture {
    pub info: StreamInfo,
    pub latest: Arc<Mutex<Snapshot>>,
    pub join: JoinHandle<Outcome>,
}

/// Opens `source` on a new thread and starts capturing.
pub fn spawn_capture(
    source: Source,
    engine_rate: u32,
    origin_hns: i64,
    settle_s: f64,
    stop: Arc<AtomicBool>,
) -> Result<Capture> {
    let latest = Arc::new(Mutex::new(Snapshot::default()));
    let shared = Arc::clone(&latest);
    let (tx, rx) = mpsc::channel::<std::result::Result<StreamInfo, String>>();
    let name = format!("s3-{source:?}");
    let join = thread::Builder::new()
        .name(name)
        .spawn(move || {
            capture_thread(
                source,
                engine_rate,
                origin_hns,
                settle_s,
                &stop,
                &shared,
                &tx,
            )
        })
        .context("spawn capture thread")?;
    match rx.recv() {
        Ok(Ok(info)) => Ok(Capture { info, latest, join }),
        Ok(Err(e)) => {
            let _ = join.join();
            bail!("{source:?}: {e}")
        }
        Err(_) => {
            let _ = join.join();
            bail!("{source:?}: capture thread ended before opening")
        }
    }
}

fn capture_thread(
    source: Source,
    engine_rate: u32,
    origin_hns: i64,
    settle_s: f64,
    stop: &AtomicBool,
    shared: &Mutex<Snapshot>,
    tx: &mpsc::Sender<std::result::Result<StreamInfo, String>>,
) -> Outcome {
    let mut outcome = Outcome {
        snap: Snapshot::default(),
        mmcss: false,
        wait_timeouts: 0,
        error: None,
    };
    let _com = match Com::init() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Err(e.to_string()));
            outcome.error = Some(e.to_string());
            return outcome;
        }
    };
    let mmcss = Mmcss::pro_audio();
    outcome.mmcss = mmcss.active();
    let opened = match source {
        Source::Mic => open_endpoint(eCapture, false, Role::Mic),
        Source::Loopback => open_endpoint(eRender, true, Role::Loopback),
        Source::Process(pid) => open_process(pid),
    };
    let setup = opened.and_then(|o| {
        let state =
            StreamState::new(&o.info, engine_rate, origin_hns, settle_s).map_err(Error::msg)?;
        // SAFETY: plain COM call on an initialised client.
        let cap: IAudioCaptureClient =
            unsafe { o.client.GetService() }.context("GetService(IAudioCaptureClient)")?;
        Ok((o, state, cap))
    });
    let (opened, mut state, cap) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(Err(e.to_string()));
            outcome.error = Some(e.to_string());
            return outcome;
        }
    };
    let _ = tx.send(Ok(opened.info.clone()));

    let result = pump(
        &opened,
        &cap,
        &mut state,
        stop,
        shared,
        &mut outcome.wait_timeouts,
    );
    // SAFETY: plain COM call; stopping an unstarted or failed stream is harmless.
    let _ = unsafe { opened.client.Stop() };
    outcome.snap = state.snapshot();
    if let Ok(mut g) = shared.lock() {
        *g = outcome.snap.clone();
    }
    if let Err(e) = result {
        outcome.error = Some(e.to_string());
    }
    outcome
}

/// The capture loop.
fn pump(
    o: &Opened,
    cap: &IAudioCaptureClient,
    state: &mut StreamState,
    stop: &AtomicBool,
    shared: &Mutex<Snapshot>,
    timeouts: &mut u64,
) -> Result<()> {
    let channels = usize::from(o.format.channels);
    let width = o.format.sample.bytes();
    let mut samples: Vec<f32> =
        Vec::with_capacity(o.info.buffer_frames.max(4800) as usize * channels);
    let mut last_publish = Instant::now();
    // SAFETY: plain COM call on an initialised client.
    unsafe { o.client.Start() }.context("IAudioClient::Start")?;
    while !stop.load(Ordering::Relaxed) {
        if !o.event.wait(WAIT_MS)? {
            *timeouts += 1;
        }
        loop {
            // SAFETY: plain COM call.
            let next = unsafe { cap.GetNextPacketSize() }.context("GetNextPacketSize")?;
            if next == 0 {
                break;
            }
            let (frames, flags, pos, qpc) =
                read_packet(cap, o.format.sample, channels, width, &mut samples)?;
            if frames == 0 {
                break;
            }
            state
                .on_packet(&Packet {
                    device_position: pos,
                    frames,
                    qpc_hns: qpc,
                    flags,
                    samples: &samples,
                })
                .map_err(Error::msg)?;
        }
        if last_publish.elapsed() >= PUBLISH_EVERY {
            // Never block the audio thread on the reader: skip if it holds the lock.
            if let Ok(mut g) = shared.try_lock() {
                *g = state.snapshot();
                last_publish = Instant::now();
            }
        }
    }
    Ok(())
}

/// Takes one packet: copies it into `out` as f32 and releases it. Returns
/// (frames, flags, device position, QPC position in hns).
fn read_packet(
    cap: &IAudioCaptureClient,
    sample: SampleFormat,
    channels: usize,
    width: usize,
    out: &mut Vec<f32>,
) -> Result<(u32, u32, u64, u64)> {
    let mut data: *mut u8 = std::ptr::null_mut();
    let mut frames = 0u32;
    let mut flags = 0u32;
    let mut pos = 0u64;
    let mut qpc = 0u64;
    // SAFETY: all out-pointers are valid locals.
    unsafe {
        cap.GetBuffer(
            &mut data,
            &mut frames,
            &mut flags,
            Some(&mut pos),
            Some(&mut qpc),
        )
    }
    .context("GetBuffer")?;
    let n = frames as usize * channels;
    if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
        out.clear();
        out.resize(n, 0.0);
    } else {
        // SAFETY: GetBuffer returned `frames` frames of `channels * width` bytes at
        // `data`, valid until ReleaseBuffer below.
        let bytes = unsafe { std::slice::from_raw_parts(data, n * width) };
        crate::stream::to_f32(bytes, sample, n, out);
    }
    // SAFETY: releases exactly the frames GetBuffer returned; `bytes` is no longer used.
    unsafe { cap.ReleaseBuffer(frames) }.context("ReleaseBuffer")?;
    Ok((frames, flags, pos, qpc))
}

/// A thread that plays silence on the default render endpoint, so loopback keeps
/// delivering packets (with timestamps) when nothing else plays.
pub struct Keepalive {
    pub device: String,
    pub join: JoinHandle<Option<String>>,
}

pub fn spawn_keepalive(stop: Arc<AtomicBool>) -> Result<Keepalive> {
    let (tx, rx) = mpsc::channel::<std::result::Result<String, String>>();
    let join = thread::Builder::new()
        .name("s3-keepalive".into())
        .spawn(move || keepalive_thread(&stop, &tx))
        .context("spawn keep-alive thread")?;
    match rx.recv() {
        Ok(Ok(device)) => Ok(Keepalive { device, join }),
        Ok(Err(e)) => {
            let _ = join.join();
            bail!("keep-alive: {e}")
        }
        Err(_) => {
            let _ = join.join();
            bail!("keep-alive thread ended before opening")
        }
    }
}

fn keepalive_thread(
    stop: &AtomicBool,
    tx: &mpsc::Sender<std::result::Result<String, String>>,
) -> Option<String> {
    let run = || -> Result<()> {
        let _com = Com::init()?;
        let _mmcss = Mmcss::pro_audio();
        let o = open_endpoint(eRender, false, Role::Loopback)?;
        // SAFETY: plain COM call on an initialised client.
        let render: IAudioRenderClient =
            unsafe { o.client.GetService() }.context("GetService(IAudioRenderClient)")?;
        let size = o.info.buffer_frames;
        let _ = tx.send(Ok(o.info.device_name.clone()));
        let silent = AUDCLNT_BUFFERFLAGS_SILENT.0 as u32;
        // Pre-fill the whole buffer, as required before Start.
        // SAFETY: requesting `size` frames on an empty buffer; released right away with
        // the SILENT flag, so the memory is never written.
        unsafe {
            render.GetBuffer(size).context("render GetBuffer")?;
            render
                .ReleaseBuffer(size, silent)
                .context("render ReleaseBuffer")?;
            o.client.Start().context("render Start")?;
        }
        while !stop.load(Ordering::Relaxed) {
            o.event.wait(WAIT_MS)?;
            // SAFETY: plain COM call.
            let padding = unsafe { o.client.GetCurrentPadding() }.context("GetCurrentPadding")?;
            let free = size.saturating_sub(padding);
            if free > 0 {
                // SAFETY: `free` frames are available; released at once as silence.
                unsafe {
                    render.GetBuffer(free).context("render GetBuffer")?;
                    render
                        .ReleaseBuffer(free, silent)
                        .context("render ReleaseBuffer")?;
                }
            }
        }
        // SAFETY: plain COM call.
        let _ = unsafe { o.client.Stop() };
        Ok(())
    };
    match run() {
        Ok(()) => None,
        Err(e) => {
            // If opening failed, the spawner is still waiting for this.
            let _ = tx.send(Err(e.to_string()));
            Some(e.to_string())
        }
    }
}
