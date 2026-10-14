//! WASAPI endpoints: listing them, resolving a target to one, opening an event-driven
//! shared-mode client on it, process loopback, and watching for default-device changes.
//!
//! Everything here runs on the calling thread, which has COM initialised.

use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use jstream_platform::media::AudioFormat;
use jstream_platform::{AudioDeviceInfo, AudioDirection, DeviceId, PlatformError};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync, DEVICE_STATE,
    DEVICE_STATE_ACTIVE, EDataFlow, ERole, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioClient, IMMDevice, IMMDeviceEnumerator, IMMEndpoint, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eAll, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::{BLOB, CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, HSTRING, IUnknown, Interface, PCWSTR, Ref, implement};

use super::format::{MixFormat, channel_mask, plan};
use crate::com::{Event, OsResult, os_error, take_pwstr};

/// Capture and loopback buffer: 200 ms, so a late audio thread loses nothing.
const CAPTURE_BUFFER_HNS: i64 = 2_000_000;
/// Render buffer: 40 ms, a few device periods. It bounds how far ahead the output
/// handler is asked for samples, which is the monitoring latency.
const RENDER_BUFFER_HNS: i64 = 400_000;
/// Process loopback has no mix format of its own; this is the format it is asked for.
const PROCESS_RATE: u32 = 48_000;
const PROCESS_CHANNELS: u16 = 2;
/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: there is no default endpoint for a flow.
const E_NOTFOUND: HRESULT = HRESULT(0x8007_0490_u32.cast_signed());
/// How long process-loopback activation may take.
const ACTIVATE_TIMEOUT_MS: u32 = 5_000;

/// How a client on an endpoint is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Record a capture endpoint.
    Capture,
    /// Record what a render endpoint plays.
    Loopback,
    /// Play to a render endpoint.
    Render,
}

/// An initialised, not yet started, event-driven shared-mode client.
pub(crate) struct Session {
    pub(crate) client: IAudioClient,
    /// Signalled by WASAPI when a buffer is ready (capture) or wanted (render).
    pub(crate) event: Event,
    /// Interleaved `f32` at the device's rate.
    pub(crate) format: AudioFormat,
    /// The buffer size in frames.
    pub(crate) buffer_frames: u32,
}

/// Creates the device enumerator.
pub(crate) fn enumerator() -> Result<IMMDeviceEnumerator, PlatformError> {
    // SAFETY: plain COM activation on a thread with COM initialised.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .os("CoCreateInstance(MMDeviceEnumerator)")
}

/// The active capture and render endpoints.
pub(crate) fn list(e: &IMMDeviceEnumerator) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
    let default_id = |flow: EDataFlow| {
        // SAFETY: plain COM call.
        unsafe { e.GetDefaultAudioEndpoint(flow, eConsole) }
            .ok()
            .and_then(|d| device_id(&d).ok())
    };
    let (default_in, default_out) = (default_id(eCapture), default_id(eRender));
    // SAFETY: plain COM call.
    let all = unsafe { e.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE) }
        .os("IMMDeviceEnumerator::EnumAudioEndpoints")?;
    // SAFETY: plain COM call.
    let count = unsafe { all.GetCount() }.os("IMMDeviceCollection::GetCount")?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: `i` is below the collection's count.
        let device = unsafe { all.Item(i) }.os("IMMDeviceCollection::Item")?;
        let id = device_id(&device)?;
        let direction = if flow(&device)? == eCapture {
            AudioDirection::Input
        } else {
            AudioDirection::Output
        };
        let default = match direction {
            AudioDirection::Input => &default_in,
            AudioDirection::Output => &default_out,
        };
        out.push(AudioDeviceInfo {
            is_default: default.as_deref() == Some(id.as_str()),
            name: friendly_name(&device),
            id: DeviceId::new(id),
            direction,
        });
    }
    Ok(out)
}

/// The endpoint ID string, which [`by_id`] takes back.
fn device_id(device: &IMMDevice) -> Result<String, PlatformError> {
    // SAFETY: plain COM call; the string is taken and freed by take_pwstr.
    unsafe { device.GetId() }
        .os("IMMDevice::GetId")
        .map(take_pwstr)
}

/// Whether the endpoint captures or renders.
fn flow(device: &IMMDevice) -> Result<EDataFlow, PlatformError> {
    let endpoint: IMMEndpoint = device.cast().os("IMMDevice as IMMEndpoint")?;
    // SAFETY: plain COM call.
    unsafe { endpoint.GetDataFlow() }.os("IMMEndpoint::GetDataFlow")
}

/// The endpoint's friendly name, or a placeholder if it can't be read.
fn friendly_name(device: &IMMDevice) -> String {
    let read = || -> windows::core::Result<String> {
        // SAFETY: plain COM calls; the PROPVARIANT is cleared by its Drop.
        let store = unsafe { device.OpenPropertyStore(STGM_READ) }?;
        // SAFETY: as above.
        let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }?;
        // SAFETY: `value` is a valid PROPVARIANT; the result is freed by take_pwstr.
        let s = unsafe { PropVariantToStringAlloc(&value) }?;
        Ok(take_pwstr(s))
    };
    read().unwrap_or_else(|_| "(unnamed audio device)".into())
}

/// The system default endpoint for `flow` (console role).
pub(crate) fn default_endpoint(
    e: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> Result<IMMDevice, PlatformError> {
    // SAFETY: plain COM call.
    unsafe { e.GetDefaultAudioEndpoint(flow, eConsole) }.map_err(|err| {
        if err.code() == E_NOTFOUND {
            PlatformError::NotFound {
                kind: if flow == eCapture {
                    "default input device"
                } else {
                    "default output device"
                },
                id: String::new(),
            }
        } else {
            os_error("IMMDeviceEnumerator::GetDefaultAudioEndpoint", &err)
        }
    })
}

/// The active endpoint with this ID, which must capture (`want == eCapture`) or render.
pub(crate) fn by_id(
    e: &IMMDeviceEnumerator,
    id: &DeviceId,
    want: EDataFlow,
) -> Result<IMMDevice, PlatformError> {
    let not_found = || PlatformError::NotFound {
        kind: if want == eCapture {
            "input device"
        } else {
            "output device"
        },
        id: id.as_str().to_owned(),
    };
    // SAFETY: plain COM call; the HSTRING outlives it.
    let device = unsafe { e.GetDevice(&HSTRING::from(id.as_str())) }.map_err(|_| not_found())?;
    // SAFETY: plain COM call.
    let state: DEVICE_STATE = unsafe { device.GetState() }.os("IMMDevice::GetState")?;
    if state != DEVICE_STATE_ACTIVE || flow(&device)? != want {
        return Err(not_found());
    }
    Ok(device)
}

/// A wave format allocated by WASAPI, freed on drop.
struct CoFormat(*mut WAVEFORMATEX);

impl Drop for CoFormat {
    fn drop(&mut self) {
        // SAFETY: the pointer came from GetMixFormat, which allocates with CoTaskMemAlloc,
        // and is freed exactly once.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

impl CoFormat {
    /// What [`plan`] needs to know about the format.
    fn mix(&self) -> Result<MixFormat, PlatformError> {
        if self.0.is_null() {
            return Err(PlatformError::Os {
                call: "IAudioClient::GetMixFormat",
                code: None,
                message: "returned no format".into(),
            });
        }
        // SAFETY: GetMixFormat returned a valid WAVEFORMATEX. It is packed, so it is read
        // unaligned.
        let wf = unsafe { self.0.read_unaligned() };
        let tag = u32::from(wf.wFormatTag);
        let ext_len = size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>();
        let float32 = if tag == WAVE_FORMAT_EXTENSIBLE && usize::from(wf.cbSize) >= ext_len {
            // SAFETY: `cbSize` says the extensible fields follow, inside the same
            // allocation.
            let ext = unsafe { self.0.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
            let sub = ext.SubFormat;
            sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && wf.wBitsPerSample == 32
        } else {
            tag == WAVE_FORMAT_IEEE_FLOAT && wf.wBitsPerSample == 32
        };
        Ok(MixFormat {
            rate: wf.nSamplesPerSec,
            channels: wf.nChannels,
            float32,
        })
    }
}

/// Interleaved 32-bit float in `format`'s rate and layout, as a WAVEFORMATEXTENSIBLE.
fn float_format(format: AudioFormat) -> WAVEFORMATEXTENSIBLE {
    // At most 6 channels of 4 bytes, so these fit their fields.
    let channels = format.channels() as u16;
    let block = channels * 4;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: format.sample_rate(),
            nAvgBytesPerSec: format.sample_rate() * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: channel_mask(format.layout()),
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// Initialises `client` for event-driven shared mode with `wf` and creates its event.
///
/// # Safety
/// `wf` must point to a valid wave format (with its extension bytes) for the call.
unsafe fn initialise(
    client: IAudioClient,
    wf: *const WAVEFORMATEX,
    flags: u32,
    buffer_hns: i64,
    format: AudioFormat,
) -> Result<Session, PlatformError> {
    // SAFETY: the caller guarantees `wf` is valid for the call.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            buffer_hns,
            0,
            wf,
            None,
        )
    }
    .os("IAudioClient::Initialize")?;
    let event = Event::new()?;
    // SAFETY: the event lives as long as the client: both are owned by the Session.
    unsafe { client.SetEventHandle(event.handle()) }.os("IAudioClient::SetEventHandle")?;
    // SAFETY: plain COM call on an initialised client.
    let buffer_frames = unsafe { client.GetBufferSize() }.os("IAudioClient::GetBufferSize")?;
    Ok(Session {
        client,
        event,
        format,
        buffer_frames,
    })
}

/// Opens an event-driven shared-mode client on `device`, in the format [`plan`] picks
/// for its mix format.
pub(crate) fn open_endpoint(device: &IMMDevice, mode: Mode) -> Result<Session, PlatformError> {
    // SAFETY: plain COM activation.
    let client: IAudioClient =
        unsafe { device.Activate(CLSCTX_ALL, None) }.os("IMMDevice::Activate(IAudioClient)")?;
    // SAFETY: plain COM call; the returned memory is owned by CoFormat.
    let mix = CoFormat(unsafe { client.GetMixFormat() }.os("IAudioClient::GetMixFormat")?);
    let plan = plan(mix.mix()?)?;
    let mut flags = match mode {
        Mode::Loopback => AUDCLNT_STREAMFLAGS_LOOPBACK,
        Mode::Capture | Mode::Render => 0,
    };
    let buffer_hns = match mode {
        Mode::Capture | Mode::Loopback => CAPTURE_BUFFER_HNS,
        Mode::Render => RENDER_BUFFER_HNS,
    };
    if plan.convert {
        flags |= AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        let wf = float_format(plan.format);
        // SAFETY: `wf` is a complete WAVEFORMATEXTENSIBLE that outlives the call.
        unsafe {
            initialise(
                client,
                (&raw const wf).cast(),
                flags,
                buffer_hns,
                plan.format,
            )
        }
    } else {
        // SAFETY: `mix` is the device's own valid format and outlives the call.
        unsafe { initialise(client, mix.0, flags, buffer_hns, plan.format) }
    }
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
        // Called by COM (R-UNSAFE-005): nothing may unwind out of it.
        let _ = catch_unwind(AssertUnwindSafe(|| self.event.set()));
        Ok(())
    }
}

/// Opens loopback of process `pid` and its children through the virtual
/// process-loopback device (Windows 10 2004 and later).
pub(crate) fn open_process(pid: u32) -> Result<Session, PlatformError> {
    let done = Arc::new(Event::new()?);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated {
        event: Arc::clone(&done),
    }
    .into();
    // Boxed, so it can be leaked if activation times out while COM may still read it.
    let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    });
    // A VT_BLOB pointing at `params`. ManuallyDrop: PropVariantClear would try to free the
    // blob, which Rust owns.
    let prop = ManuallyDrop::new(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: (&raw mut *params).cast(),
                    },
                },
            }),
        },
    });
    // SAFETY: `prop` and the boxed `params` it points to stay alive until activation has
    // completed; on a timeout `params` is leaked below instead of freed.
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *prop),
            &handler,
        )
    }
    .os("ActivateAudioInterfaceAsync")?;
    if crate::com::wait_any(&[&done], ACTIVATE_TIMEOUT_MS)? == crate::com::Woke::Timeout {
        std::mem::forget(params);
        return Err(PlatformError::Os {
            call: "ActivateAudioInterfaceAsync",
            code: None,
            message: format!(
                "process loopback activation did not complete within {ACTIVATE_TIMEOUT_MS} ms"
            ),
        });
    }
    drop(params);
    let mut hr = HRESULT(0);
    let mut unk: Option<IUnknown> = None;
    // SAFETY: both out-pointers are valid locals.
    unsafe { op.GetActivateResult(&mut hr, &mut unk) }
        .os("IActivateAudioInterfaceAsyncOperation::GetActivateResult")?;
    hr.ok().os("process loopback activation")?;
    let client: IAudioClient = unk
        .ok_or_else(|| PlatformError::Os {
            call: "process loopback activation",
            code: None,
            message: "returned no interface".into(),
        })?
        .cast()
        .os("process loopback as IAudioClient")?;
    let plan = plan(MixFormat {
        rate: PROCESS_RATE,
        channels: PROCESS_CHANNELS,
        float32: true,
    })?;
    let wf = float_format(plan.format);
    let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    // SAFETY: `wf` is a complete WAVEFORMATEXTENSIBLE that outlives the call.
    unsafe {
        initialise(
            client,
            (&raw const wf).cast(),
            flags,
            CAPTURE_BUFFER_HNS,
            plan.format,
        )
    }
}

/// Signals an event when the default endpoint of one flow (console role) changes.
#[implement(IMMNotificationClient)]
struct DefaultChanged {
    flow: EDataFlow,
    event: Arc<Event>,
}

impl IMMNotificationClient_Impl for DefaultChanged_Impl {
    fn OnDeviceStateChanged(
        &self,
        _id: &PCWSTR,
        _state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _id: &PCWSTR,
    ) -> windows::core::Result<()> {
        // Called by COM (R-UNSAFE-005): nothing may unwind out of it.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if flow == self.flow && role == eConsole {
                self.event.set();
            }
        }));
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// A registration for default-device changes of one flow, removed on drop.
pub(crate) struct DefaultWatch {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    /// Signalled when the default device changes.
    pub(crate) event: Arc<Event>,
}

impl DefaultWatch {
    pub(crate) fn new(e: &IMMDeviceEnumerator, flow: EDataFlow) -> Result<Self, PlatformError> {
        let event = Arc::new(Event::new()?);
        let client: IMMNotificationClient = DefaultChanged {
            flow,
            event: Arc::clone(&event),
        }
        .into();
        // SAFETY: plain COM call; the client stays registered until Drop unregisters it.
        unsafe { e.RegisterEndpointNotificationCallback(&client) }
            .os("IMMDeviceEnumerator::RegisterEndpointNotificationCallback")?;
        Ok(Self {
            enumerator: e.clone(),
            client,
            event,
        })
    }
}

impl Drop for DefaultWatch {
    fn drop(&mut self) {
        // SAFETY: unregisters the client registered in `new`, on the same enumerator.
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        };
    }
}
