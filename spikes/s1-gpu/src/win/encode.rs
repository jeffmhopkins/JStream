//! Media Foundation hardware H.264 encoder MFT fed with D3D11 NV12 textures through the
//! DXGI device manager, so frames never leave the GPU.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use crate::err::{Context, Result, bail};
use windows::Win32::Foundation::LUID;
use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVLowLatencyMode,
    ICodecAPI, IMF2DBuffer, IMFActivate, IMFAttributes, IMFDXGIDeviceManager,
    IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown, IMFTransform,
    METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_EVENT_FLAG_NO_WAIT, MF_EVENT_TYPE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_SA_D3D11_AWARE,
    MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFNominalRange_16_235, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR,
    eAVEncH264VProfile_High,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{GUID, Interface, PWSTR};

use super::d3d::luid_u64;

/// One encoded access unit (Annex B bytes) and its sample time in 100 ns units.
pub struct Packet {
    pub data: Vec<u8>,
    pub time: i64,
}

pub struct Encoder {
    activate: IMFActivate,
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    _manager: IMFDXGIDeviceManager,
    pub name: String,
    pub is_async: bool,
    pub d3d11_aware: bool,
    provides_samples: bool,
    out_buffer_size: u32,
    need_input: u32,
    pub warnings: Vec<String>,
}

impl Encoder {
    pub fn new(
        device: &ID3D11Device,
        adapter_luid: LUID,
        (width, height): (u32, u32),
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self> {
        let mut warnings = Vec::new();
        let (activate, name) = find_hardware_encoder(adapter_luid, &mut warnings)?;
        // SAFETY: plain method call on a live COM object.
        let transform: IMFTransform =
            unsafe { activate.ActivateObject() }.context("IMFActivate::ActivateObject")?;

        // SAFETY: plain method call on a live COM object.
        let attrs = unsafe { transform.GetAttributes() }.context("IMFTransform::GetAttributes")?;
        let is_async = get_u32(&attrs, &MF_TRANSFORM_ASYNC).unwrap_or(0) != 0;
        if is_async {
            // SAFETY: plain method call on a live COM object.
            unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
                .context("MF_TRANSFORM_ASYNC_UNLOCK")?;
        }
        let d3d11_aware = get_u32(&attrs, &MF_SA_D3D11_AWARE).unwrap_or(0) != 0;
        if !d3d11_aware {
            bail!("encoder `{name}` is not D3D11-aware; it would need a CPU copy");
        }

        let mut reset_token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        // SAFETY: both out-pointers point to valid locals.
        unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager) }
            .context("MFCreateDXGIDeviceManager")?;
        let manager = manager.context("MFCreateDXGIDeviceManager returned nothing")?;
        // SAFETY: `device` is a live, multithread-protected D3D11 device.
        unsafe { manager.ResetDevice(device, reset_token) }
            .context("IMFDXGIDeviceManager::ResetDevice")?;
        // SAFETY: the message parameter is the manager's IUnknown pointer, which stays alive
        // in `self._manager` for as long as the transform exists.
        unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) }
            .context("MFT_MESSAGE_SET_D3D_MANAGER")?;

        let bitrate = bitrate_kbps.saturating_mul(1000);
        match transform.cast::<ICodecAPI>() {
            Ok(codec) => {
                for (what, api, value) in [
                    (
                        "rate control CBR",
                        &CODECAPI_AVEncCommonRateControlMode,
                        Val::U32(eAVEncCommonRateControlMode_CBR.0 as u32),
                    ),
                    (
                        "mean bitrate",
                        &CODECAPI_AVEncCommonMeanBitRate,
                        Val::U32(bitrate),
                    ),
                    ("low latency", &CODECAPI_AVLowLatencyMode, Val::Bool(true)),
                    ("GOP size", &CODECAPI_AVEncMPVGOPSize, Val::U32(fps * 2)),
                    (
                        "no B-frames",
                        &CODECAPI_AVEncMPVDefaultBPictureCount,
                        Val::U32(0),
                    ),
                ] {
                    if let Err(e) = set_codec_value(&codec, api, value) {
                        warnings.push(format!("ICodecAPI {what}: {e}"));
                    }
                }
            }
            Err(e) => warnings.push(format!("no ICodecAPI: {e}")),
        }

        // Encoders take the output type first.
        // SAFETY: plain constructor call.
        let out_type = unsafe { MFCreateMediaType() }.context("MFCreateMediaType")?;
        set_video_attrs(&out_type, &MFVideoFormat_H264, width, height, fps)?;
        // SAFETY: plain method calls on a live COM object.
        unsafe {
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            transform.SetOutputType(0, &out_type, 0)
        }
        .context("SetOutputType H.264")?;

        let in_type = input_type(&transform)?;
        set_video_attrs(&in_type, &MFVideoFormat_NV12, width, height, fps)?;
        // SAFETY: plain method calls on a live COM object.
        unsafe {
            in_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            in_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            transform.SetInputType(0, &in_type, 0)
        }
        .context("SetInputType NV12")?;

        // SAFETY: plain method call on a live COM object.
        let info = unsafe { transform.GetOutputStreamInfo(0) }.context("GetOutputStreamInfo")?;
        let provides_samples = info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32
            != 0;

        let events = if is_async {
            Some(
                transform
                    .cast()
                    .context("async MFT without IMFMediaEventGenerator")?,
            )
        } else {
            None
        };
        // SAFETY: plain method calls on a live COM object.
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
        }
        .context("start streaming")?;

        Ok(Self {
            activate,
            transform,
            events,
            _manager: manager,
            name,
            is_async,
            d3d11_aware,
            provides_samples,
            out_buffer_size: info.cbSize.max(width * height * 3 / 2),
            need_input: 0,
            warnings,
        })
    }

    /// Whether the encoder has asked for (at least) one more input.
    pub fn can_accept(&self) -> bool {
        !self.is_async || self.need_input > 0
    }

    /// Queues one NV12 texture. The caller has already queued a GPU wait on the D3D11
    /// immediate context for the conversion that wrote it.
    pub fn submit(&mut self, texture: &ID3D11Texture2D, time: i64, duration: i64) -> Result<()> {
        // SAFETY: `texture` is a live D3D11 texture; the IID matches its interface.
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false) }
            .context("MFCreateDXGISurfaceBuffer")?;
        if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
            // SAFETY: plain method calls on live COM objects.
            unsafe {
                let len = b2.GetContiguousLength()?;
                buffer.SetCurrentLength(len)?;
            }
        }
        // SAFETY: plain constructor and method calls on live COM objects.
        unsafe {
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.transform.ProcessInput(0, &sample, 0)
        }
        .context("IMFTransform::ProcessInput")?;
        self.need_input = self.need_input.saturating_sub(1);
        Ok(())
    }

    /// Handles every pending event without blocking. Returns true once draining finished.
    pub fn poll(&mut self, out: &mut Vec<Packet>) -> Result<bool> {
        let Some(events) = self.events.clone() else {
            // Synchronous MFT: pull until it wants more input.
            while self.pull_output(out)? {}
            return Ok(false);
        };
        let mut drained = false;
        loop {
            // SAFETY: plain method call on a live COM object.
            let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                Err(e) => return Err(e).context("IMFMediaEventGenerator::GetEvent"),
            };
            // SAFETY: plain method call on a live COM object.
            let kind = MF_EVENT_TYPE(unsafe { event.GetType() }? as i32);
            if kind == METransformNeedInput {
                self.need_input += 1;
            } else if kind == METransformHaveOutput {
                self.pull_output(out)?;
            } else if kind == METransformDrainComplete {
                drained = true;
            }
        }
        Ok(drained)
    }

    /// Ends the stream and collects the remaining output (at most `timeout`).
    pub fn drain(&mut self, out: &mut Vec<Packet>, timeout: Duration) -> Result<()> {
        // SAFETY: plain method calls on a live COM object.
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        if !self.is_async {
            while self.pull_output(out)? {}
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        while !self.poll(out)? {
            if Instant::now() > deadline {
                self.warnings.push("drain timed out".into());
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// One ProcessOutput call. Ok(false) means the MFT needs more input.
    fn pull_output(&mut self, out: &mut Vec<Packet>) -> Result<bool> {
        let sample: Option<IMFSample> = if self.provides_samples {
            None
        } else {
            // SAFETY: plain constructor and method calls on live COM objects.
            Some(unsafe {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_buffer_size)?)?;
                s
            })
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: `buffers` is a valid one-element array and `status` a valid out-pointer.
        let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
        // SAFETY: each field is taken exactly once, then `buffers` is never used again, so
        // the references the MFT handed us are released exactly once.
        let (sample, _events) = unsafe {
            (
                ManuallyDrop::take(&mut buffers[0].pSample),
                ManuallyDrop::take(&mut buffers[0].pEvents),
            )
        };
        match result {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(false),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: plain method calls on a live COM object.
                unsafe {
                    let t = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &t, 0)?;
                }
                self.warnings
                    .push("output stream change renegotiated".into());
                return Ok(true);
            }
            Err(e) => return Err(e).context("IMFTransform::ProcessOutput"),
        }
        let sample = sample.context("ProcessOutput succeeded without a sample")?;
        // SAFETY: plain method calls on live COM objects; the locked pointer is valid for
        // `len` bytes until Unlock, and we copy out before unlocking.
        let packet = unsafe {
            let time = sample.GetSampleTime().unwrap_or(-1);
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            Packet { data, time }
        };
        out.push(packet);
        Ok(true)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: plain method calls on live COM objects; errors are irrelevant at teardown.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.transform.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// Enumerates hardware H.264 encoders (NV12 in), preferring the one on `adapter_luid`.
fn find_hardware_encoder(
    adapter_luid: LUID,
    warnings: &mut Vec<String>,
) -> Result<(IMFActivate, String)> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type infos outlive the call; the out-pointers point to valid locals.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut array,
            &mut count,
        )
    }
    .context("MFTEnumEx")?;
    let activates = take_activates(array, count);
    if activates.is_empty() {
        bail!("no hardware H.264 encoder MFT (MFTEnumEx with MFT_ENUM_FLAG_HARDWARE found none)");
    }

    let want = luid_u64(adapter_luid);
    let mut names = Vec::new();
    let mut chosen = None;
    for (i, activate) in activates.iter().enumerate() {
        let name = friendly_name(activate);
        let luid = mft_luid(activate);
        names.push(format!(
            "{name} (LUID {})",
            luid.map_or("unknown".into(), |l| format!("{l:#x}"))
        ));
        if chosen.is_none() && luid == Some(want) {
            chosen = Some(i);
        }
    }
    let index = match chosen {
        Some(i) => i,
        None => {
            warnings.push(format!(
                "no encoder MFT reports the wgpu adapter's LUID {want:#x}; using the first one. \
                 Found: {}",
                names.join("; ")
            ));
            0
        }
    };
    let activate = activates[index].clone();
    Ok((activate.clone(), friendly_name(&activate)))
}

/// Takes ownership of the CoTaskMem array MFTEnumEx returned.
fn take_activates(array: *mut Option<IMFActivate>, count: u32) -> Vec<IMFActivate> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: MFTEnumEx returned `count` initialised entries at `array`; we move each
    // reference out exactly once and then free the array itself with CoTaskMemFree.
    unsafe {
        let items = std::slice::from_raw_parts_mut(array, count as usize);
        let out = items.iter_mut().filter_map(Option::take).collect();
        CoTaskMemFree(Some(array as *const _));
        out
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut ptr = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: valid out-pointers; the returned string is CoTaskMem-allocated and freed once.
    unsafe {
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len)
            .is_err()
        {
            return "unknown".into();
        }
        let name = ptr.to_string().unwrap_or_else(|_| "invalid UTF-16".into());
        CoTaskMemFree(Some(ptr.0 as *const _));
        name
    }
}

/// MFT_ENUM_ADAPTER_LUID, read as a UINT64 or as an 8-byte blob (LUID struct).
fn mft_luid(activate: &IMFActivate) -> Option<u64> {
    // SAFETY: plain method call on a live COM object.
    if let Ok(v) = unsafe { activate.GetUINT64(&MFT_ENUM_ADAPTER_LUID) } {
        return Some(v);
    }
    let mut blob = [0u8; 8];
    let mut size = 0u32;
    // SAFETY: `blob` is 8 writable bytes and we pass its length.
    unsafe { activate.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut blob, Some(&mut size)) }.ok()?;
    let low = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    let high = i32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
    Some(luid_u64(LUID {
        LowPart: low,
        HighPart: high,
    }))
}

fn get_u32(attrs: &IMFAttributes, key: &GUID) -> Option<u32> {
    // SAFETY: plain method call on a live COM object.
    unsafe { attrs.GetUINT32(key) }.ok()
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn set_video_attrs(t: &IMFMediaType, subtype: &GUID, w: u32, h: u32, fps: u32) -> Result<()> {
    // SAFETY: plain method calls on a live COM object; GUID pointers refer to statics/locals.
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    }
    Ok(())
}

/// The encoder's own NV12 input type if it lists one (keeps vendor attributes), else a
/// fresh type.
fn input_type(transform: &IMFTransform) -> Result<IMFMediaType> {
    for i in 0.. {
        // SAFETY: plain method call on a live COM object; errors end the enumeration.
        let Ok(t) = (unsafe { transform.GetInputAvailableType(0, i) }) else {
            break;
        };
        // SAFETY: plain method call on a live COM object.
        if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
            return Ok(t);
        }
    }
    // SAFETY: plain constructor call.
    Ok(unsafe { MFCreateMediaType() }?)
}

enum Val {
    U32(u32),
    Bool(bool),
}

fn set_codec_value(codec: &ICodecAPI, api: &GUID, value: Val) -> windows::core::Result<()> {
    let mut v = VARIANT::default();
    // SAFETY: we initialise the discriminant and the matching union member of a
    // default (VT_EMPTY) VARIANT; neither type owns resources, so no clear is needed.
    unsafe {
        let inner = &mut v.Anonymous.Anonymous;
        match value {
            Val::U32(x) => {
                inner.vt = VT_UI4;
                inner.Anonymous.ulVal = x;
            }
            Val::Bool(b) => {
                inner.vt = VT_BOOL;
                inner.Anonymous.boolVal = if b { VARIANT_TRUE } else { VARIANT_FALSE };
            }
        }
        codec.SetValue(api, &v)
    }
}
