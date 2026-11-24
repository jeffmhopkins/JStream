//! AMD's AMF encoders, driven directly through the AMF runtime (`amfrt64.dll`, which
//! AMD's display driver installs) on the context's D3D11 device: H.264
//! (`AMFVideoEncoderVCE_AVC`), HEVC (`AMFVideoEncoderHW_HEVC`) and AV1
//! (`AMFVideoEncoderHW_AV1`).
//!
//! # Behaviour
//!
//! - **Runtime:** loaded once per process from System32 and kept loaded: AMF's factory is
//!   a process-wide object whose worker threads outlive a component's `Terminate`, so the
//!   DLL is never unloaded under them (OBS and AMD's samples keep it loaded too).
//!   `AMFQueryVersion` must report at least [`MIN_VERSION`] (H.264 and HEVC) or
//!   [`MIN_AV1_VERSION`] (AV1), and `AMFInit` asks for the lower of the runtime's version
//!   and [`TARGET_VERSION`], the newest whose properties this module uses.
//! - **Configuration** ([`properties`]): usage *transcoding*, the usage OBS streams with,
//!   then the *quality* preset (the MFT path's quality-over-speed band; *high quality*
//!   doesn't keep 1080p60 real time on integrated GPUs such as the 780M), the codec's
//!   profile (H.264 High, HEVC Main, AV1 Main), 8-bit, the frame size and rate, CBR
//!   (with HRD and filler data, the strict CBR RTMP ingest expects) or peak-constrained
//!   VBR, a VBV buffer of one second at the peak rate, the keyframe interval as the IDR
//!   period (H.264) or GOP (HEVC, one GOP per IDR; AV1), no B-frames (so `dts == pts`),
//!   the encoder's low-latency mode, BT.709 limited-range colour signalling, and the
//!   parameter sets (VPS/SPS/PPS, or AV1's sequence header) on every IDR or key frame.
//!   The properties a stream can't do without (usage, size, rate, rate control, bit rate,
//!   keyframe interval, profile) must be accepted; the rest are best effort, so an older
//!   runtime that doesn't know one still opens.
//! - **Input:** each encode target is copied on the GPU into one of eight NV12 textures
//!   of the session's (`Ring`), wrapped as an AMF surface (`CreateSurfaceFromDX11Native`)
//!   and submitted. The slot is free again once the frame's packet is out (`Slots`). A
//!   frame is refused as `Busy` when every slot is held or AMF answers `AMF_INPUT_FULL`;
//!   `can_accept` says no until output has been taken.
//! - **Timing:** each surface carries its frame's sequence number as the custom property
//!   `JStreamSeq`, which AMF copies onto the output buffer (FFmpeg's AMF encoder relies on
//!   the same), and `seq × frame duration` as its 100 ns `pts`. The packet gets back its
//!   frame's exact `pts` from the sequence number, or from the `pts` when the property
//!   is missing.
//! - **Keyframes:** `request_keyframe` makes the next surface force an IDR (key) frame
//!   and insert the parameter sets with it. A packet is a keyframe when the encoder's
//!   output type says IDR (H.264, HEVC) or key frame (AV1).
//! - **Output:** `QueryOutput` never waits (query timeout 0); `poll` takes every buffer
//!   ready. H.264 and HEVC come out as Annex B, AV1 as one temporal unit of low-overhead
//!   OBUs per frame (frame output mode). `finish` drains and waits up to five seconds
//!   for `AMF_EOF`.
//! - **Teardown:** the component, then the context, are terminated and released before
//!   the input textures.

// The tables and declarations build everywhere for their tests and layout checks; the
// session that uses all of them is Windows only.
#![cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "used by the Windows session; built elsewhere for its tests"
    )
)]

pub(crate) mod ffi;

#[cfg(windows)]
pub(crate) use session::AmfEncoder;

use crate::codec::video::{Format, Settings};

/// The newest AMF version whose properties this module sets; `AMFInit` asks for it, or
/// for the runtime's own version when that is older.
pub(crate) const TARGET_VERSION: u64 = ffi::full_version(1, 4, 29, 0);
/// The oldest runtime the H.264 and HEVC encoders open on.
pub(crate) const MIN_VERSION: u64 = ffi::full_version(1, 4, 0, 0);
/// The oldest runtime with the AV1 encoder.
pub(crate) const MIN_AV1_VERSION: u64 = ffi::full_version(1, 4, 29, 0);

/// An ASCII string literal as a null-terminated UTF-16 slice, the `wchar_t*` AMF names
/// properties and components with.
macro_rules! wide {
    ($s:literal) => {{
        const W: &[u16] = &{
            let bytes = $s.as_bytes();
            let mut out = [0u16; $s.len() + 1];
            let mut i = 0;
            while i < bytes.len() {
                out[i] = bytes[i] as u16;
                i += 1;
            }
            out
        };
        W
    }};
}

/// The custom surface property each frame's sequence number rides on to its packet.
pub(crate) const SEQ_PROPERTY: &[u16] = wide!("JStreamSeq");

/// A property value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// `amf_int64`.
    Int(i64),
    /// `amf_bool`.
    Bool(bool),
    /// `AMFSize`: width and height.
    Size(i32, i32),
    /// `AMFRate`: numerator and denominator.
    Rate(u32, u32),
}

/// One encoder property to set before `Init`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Property {
    /// Its name, null-terminated UTF-16.
    pub(crate) name: &'static [u16],
    /// Its value.
    pub(crate) value: Value,
    /// Whether opening fails when the encoder refuses it; otherwise it is best effort.
    pub(crate) required: bool,
}

/// What differs between the three AMF encoders besides their configuration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Codec {
    /// The component ID `CreateComponent` takes.
    pub(crate) component: &'static [u16],
    /// The oldest runtime that has it.
    pub(crate) min_version: u64,
    /// The surface property that forces a picture type, and the value that forces an
    /// IDR (key) frame.
    pub(crate) force: (&'static [u16], i64),
    /// The surface properties set to true with a forced keyframe, which insert the
    /// parameter sets (or sequence header) with it.
    pub(crate) headers: &'static [&'static [u16]],
    /// The output buffer's picture-type property, and the value of an IDR (key) frame.
    pub(crate) output_type: (&'static [u16], i64),
    /// Its name in messages.
    pub(crate) name: &'static str,
}

/// The AMF encoder of `format`.
pub(crate) fn codec(format: Format) -> Codec {
    match format {
        Format::H264 => Codec {
            component: wide!("AMFVideoEncoderVCE_AVC"),
            min_version: MIN_VERSION,
            // AMF_VIDEO_ENCODER_PICTURE_TYPE_IDR
            force: (wide!("ForcePictureType"), 2),
            headers: &[wide!("InsertSPS"), wide!("InsertPPS")],
            // AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_IDR
            output_type: (wide!("OutputDataType"), 0),
            name: "H.264",
        },
        Format::Hevc => Codec {
            component: wide!("AMFVideoEncoderHW_HEVC"),
            min_version: MIN_VERSION,
            // AMF_VIDEO_ENCODER_HEVC_PICTURE_TYPE_IDR
            force: (wide!("HevcForcePictureType"), 2),
            headers: &[wide!("HevcInsertHeader")],
            // AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_IDR
            output_type: (wide!("HevcOutputDataType"), 0),
            name: "HEVC",
        },
        Format::Av1 => Codec {
            component: wide!("AMFVideoEncoderHW_AV1"),
            min_version: MIN_AV1_VERSION,
            // AMF_VIDEO_ENCODER_AV1_FORCE_FRAME_TYPE_KEY
            force: (wide!("Av1ForceFrameType"), 1),
            headers: &[wide!("Av1ForceInsertSequenceHeader")],
            // AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_KEY
            output_type: (wide!("Av1OutputFrameType"), 0),
            name: "AV1",
        },
    }
}

/// `AMF_VIDEO_CONVERTER_COLOR_PROFILE_709`, `AMF_COLOR_TRANSFER_CHARACTERISTIC_BT709` and
/// `AMF_COLOR_PRIMARIES_BT709`: all 1.
const BT709: i64 = 1;

/// The encoder properties for `settings`, a `width`x`height` stream with a keyframe
/// every `keyframe_interval` frames, in the order they are set: the usage first, since
/// it resets the others to its defaults.
pub(crate) fn properties(
    settings: &Settings,
    keyframe_interval: u32,
    width: u32,
    height: u32,
) -> Vec<Property> {
    let int = |name, v: i64, required| Property {
        name,
        value: Value::Int(v),
        required,
    };
    let flag = |name, v: bool| Property {
        name,
        value: Value::Bool(v),
        required: false,
    };
    let size = Value::Size(
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    );
    let rate = Value::Rate(settings.fps.0, settings.fps.1);
    let cbr = settings.max_bitrate.is_none();
    let target = i64::from(settings.bitrate);
    let peak = i64::from(settings.max_bitrate.unwrap_or(settings.bitrate));
    // One second of the peak rate.
    let vbv = peak;
    let keyint = i64::from(keyframe_interval);
    let at = |name, value| Property {
        name,
        value,
        required: true,
    };
    match settings.format {
        Format::H264 => vec![
            // AMF_VIDEO_ENCODER_USAGE_TRANSCODING
            int(wide!("Usage"), 0, true),
            // AMF_VIDEO_ENCODER_PROFILE_HIGH
            int(wide!("Profile"), 100, true),
            // AMF_VIDEO_ENCODER_QUALITY_PRESET_QUALITY
            int(wide!("QualityPreset"), 2, false),
            int(wide!("ColorBitDepth"), 8, false),
            at(wide!("FrameSize"), size),
            at(wide!("FrameRate"), rate),
            flag(wide!("LowLatencyInternal"), true),
            int(wide!("MaxConsecutiveBPictures"), 0, false),
            int(wide!("BPicturesPattern"), 0, false),
            // CBR, or PEAK_CONSTRAINED_VBR
            int(wide!("RateControlMethod"), if cbr { 1 } else { 2 }, true),
            int(wide!("TargetBitrate"), target, true),
            int(wide!("PeakBitrate"), peak, false),
            int(wide!("VBVBufferSize"), vbv, false),
            flag(wide!("EnforceHRD"), true),
            flag(wide!("FillerDataEnable"), cbr),
            int(wide!("IDRPeriod"), keyint, true),
            int(wide!("HeaderInsertionSpacing"), keyint, false),
            int(wide!("InColorProfile"), BT709, false),
            int(wide!("OutColorProfile"), BT709, false),
            int(wide!("InColorTransferChar"), BT709, false),
            int(wide!("OutColorTransferChar"), BT709, false),
            int(wide!("InColorPrimaries"), BT709, false),
            int(wide!("OutColorPrimaries"), BT709, false),
            flag(wide!("InputFullRangeColor"), false),
            flag(wide!("FullRangeColor"), false),
            int(wide!("QueryTimeout"), 0, false),
        ],
        Format::Hevc => vec![
            // AMF_VIDEO_ENCODER_HEVC_USAGE_TRANSCODING
            int(wide!("HevcUsage"), 0, true),
            // AMF_VIDEO_ENCODER_HEVC_PROFILE_MAIN, TIER_MAIN
            int(wide!("HevcProfile"), 1, true),
            int(wide!("HevcTier"), 0, false),
            // AMF_VIDEO_ENCODER_HEVC_QUALITY_PRESET_QUALITY
            int(wide!("HevcQualityPreset"), 0, false),
            int(wide!("HevcColorBitDepth"), 8, false),
            at(wide!("HevcFrameSize"), size),
            at(wide!("HevcFrameRate"), rate),
            flag(wide!("LowLatencyInternal"), true),
            // CBR, or PEAK_CONSTRAINED_VBR (HEVC numbers them differently)
            int(
                wide!("HevcRateControlMethod"),
                if cbr { 3 } else { 2 },
                true,
            ),
            int(wide!("HevcTargetBitrate"), target, true),
            int(wide!("HevcPeakBitrate"), peak, false),
            int(wide!("HevcVBVBufferSize"), vbv, false),
            flag(wide!("HevcEnforceHRD"), true),
            flag(wide!("HevcFillerDataEnable"), cbr),
            int(wide!("HevcGOPSize"), keyint, true),
            int(wide!("HevcGOPSPerIDR"), 1, true),
            // AMF_VIDEO_ENCODER_HEVC_HEADER_INSERTION_MODE_IDR_ALIGNED
            int(wide!("HevcHeaderInsertionMode"), 2, false),
            int(wide!("HevcInColorProfile"), BT709, false),
            int(wide!("HevcOutColorProfile"), BT709, false),
            int(wide!("HevcInColorTransferChar"), BT709, false),
            int(wide!("HevcOutColorTransferChar"), BT709, false),
            int(wide!("HevcInColorPrimaries"), BT709, false),
            int(wide!("HevcOutColorPrimaries"), BT709, false),
            flag(wide!("HevcInputFullRangeColor"), false),
            flag(wide!("HevcNominalRange"), false),
            int(wide!("HevcQueryTimeout"), 0, false),
        ],
        Format::Av1 => vec![
            // AMF_VIDEO_ENCODER_AV1_USAGE_TRANSCODING
            int(wide!("Av1Usage"), 0, true),
            // AMF_VIDEO_ENCODER_AV1_PROFILE_MAIN
            int(wide!("Av1Profile"), 1, true),
            // AMF_VIDEO_ENCODER_AV1_QUALITY_PRESET_QUALITY
            int(wide!("Av1QualityPreset"), 30, false),
            int(wide!("Av1ColorBitDepth"), 8, false),
            at(wide!("Av1FrameSize"), size),
            at(wide!("Av1FrameRate"), rate),
            // AMF_VIDEO_ENCODER_AV1_ALIGNMENT_MODE_NO_RESTRICTIONS: any even size
            // (1080p stays 1080p rather than being refused or coded as 1088).
            int(wide!("Av1AlignmentMode"), 3, false),
            // AMF_VIDEO_ENCODER_AV1_ENCODING_LATENCY_MODE_REAL_TIME
            int(wide!("Av1EncodingLatencyMode"), 2, false),
            int(wide!("Av1MaxConsecutiveBPictures"), 0, false),
            int(wide!("Av1BPicturesPattern"), 0, false),
            // CBR, or PEAK_CONSTRAINED_VBR
            int(wide!("Av1RateControlMethod"), if cbr { 3 } else { 2 }, true),
            int(wide!("Av1TargetBitrate"), target, true),
            int(wide!("Av1PeakBitrate"), peak, false),
            int(wide!("Av1VBVBufferSize"), vbv, false),
            flag(wide!("Av1EnforceHRD"), true),
            flag(wide!("Av1FillerData"), cbr),
            int(wide!("Av1GOPSize"), keyint, true),
            // AMF_VIDEO_ENCODER_AV1_HEADER_INSERTION_MODE_KEY_FRAME_ALIGNED
            int(wide!("Av1HeaderInsertionMode"), 2, false),
            // AMF_VIDEO_ENCODER_AV1_OUTPUT_MODE_FRAME: one temporal unit per buffer.
            int(wide!("AV1OutputMode"), 0, false),
            int(wide!("Av1InputColorProfile"), BT709, false),
            int(wide!("Av1OutputColorProfile"), BT709, false),
            int(wide!("Av1InputColorTransferChar"), BT709, false),
            int(wide!("Av1OutputColorTransferChar"), BT709, false),
            int(wide!("Av1InputColorPrimaries"), BT709, false),
            int(wide!("Av1OutputColorPrimaries"), BT709, false),
            flag(wide!("Av1InputFullRangeColor"), false),
            flag(wide!("Av1NominalRange"), false),
            int(wide!("Av1QueryTimeout"), 0, false),
        ],
    }
}

/// The sequence number an output buffer's frame was sent with: its `JStreamSeq`
/// property, or else its 100 ns `pts` divided by the frame duration it was sent with.
pub(crate) fn sequence(property: Option<i64>, pts_hns: i64, frame_hns: i64) -> Option<u64> {
    match property {
        Some(seq) => u64::try_from(seq).ok(),
        None if frame_hns > 0 && pts_hns >= 0 && pts_hns % frame_hns == 0 => {
            u64::try_from(pts_hns / frame_hns).ok()
        }
        None => None,
    }
}

#[cfg(windows)]
mod session {
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use jstream_platform::media::{MediaTime, Packet, PixelFormat};
    use jstream_platform::{
        CodecInfo, CodecRole, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface,
    };
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    use windows::core::{Interface, PCSTR, w};

    use super::ffi::{
        self, AMF_DX11_0, AMF_EOF, AMF_INPUT_FULL, AMF_NEED_MORE_INPUT, AMF_OK, AMF_REPEAT,
        AMF_SURFACE_NV12, AMF_VARIANT_BOOL, AMF_VARIANT_INT64, AMF_VARIANT_RATE, AMF_VARIANT_SIZE,
        AMFBuffer, AMFComponent, AMFContext, AMFData, AMFFactory, AMFRate, AMFSize, AMFSurface,
        AMFVariantStruct, AMFVariantValue, AmfResult, IID_AMF_BUFFER,
    };
    use super::{Codec, SEQ_PROPERTY, TARGET_VERSION, Value, codec, properties, sequence};
    use crate::codec::direct::{Ring, Slots};
    use crate::codec::video::{Settings, check, hns};
    use crate::gpu::EncodeGpu;

    /// Input textures, and so frames in flight at most.
    const SLOTS: usize = 8;
    /// How long `finish` waits for the drain.
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

    /// An AMF failure as [`PlatformError::Os`], naming the call and the result.
    fn amf_error(call: &'static str, result: AmfResult) -> PlatformError {
        PlatformError::Os {
            call,
            code: u32::try_from(result).ok(),
            message: ffi::result_name(result).to_owned(),
        }
    }

    /// `Ok` for `AMF_OK`, else the failure.
    fn ok(call: &'static str, result: AmfResult) -> Result<(), PlatformError> {
        if result == AMF_OK {
            Ok(())
        } else {
            Err(amf_error(call, result))
        }
    }

    /// The loaded runtime: its factory and version. Loaded once and never unloaded
    /// (module docs).
    struct Runtime {
        factory: NonNull<AMFFactory>,
        version: u64,
    }

    // SAFETY: AMF's factory is a process-wide object whose methods are thread-safe (AMD's
    // AMF API reference: the factory may be used from any thread). `Runtime` only reads
    // the pointer and version after initialisation.
    unsafe impl Send for Runtime {}
    // SAFETY: as above.
    unsafe impl Sync for Runtime {}

    /// The runtime, loaded on first use; the error is kept, so a machine without AMF
    /// is asked once.
    fn runtime() -> Result<&'static Runtime, PlatformError> {
        static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
        RUNTIME
            .get_or_init(load)
            .as_ref()
            .map_err(|why| PlatformError::Unsupported { what: why.clone() })
    }

    /// Loads `amfrt64.dll` from System32 and initialises AMF.
    fn load() -> Result<Runtime, String> {
        // SAFETY: plain call with a static, null-terminated name; only System32 is
        // searched, so a stray copy beside the exe can't be loaded. The module is never
        // freed (module docs), so the function pointers below stay valid.
        let module =
            unsafe { LoadLibraryExW(w!("amfrt64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|e| format!("AMF: {} doesn't load ({e}); not an AMD driver", ffi::DLL))?;
        // SAFETY: the module is loaded for the process's life and the names are static
        // and null-terminated.
        let (query, init) = unsafe {
            (
                GetProcAddress(module, PCSTR(ffi::QUERY_VERSION_FUNCTION.as_ptr())),
                GetProcAddress(module, PCSTR(ffi::INIT_FUNCTION.as_ptr())),
            )
        };
        let (Some(query), Some(init)) = (query, init) else {
            return Err(format!(
                "AMF: {} lacks AMFQueryVersion or AMFInit",
                ffi::DLL
            ));
        };
        // SAFETY: the exports have these signatures (`AMFQueryVersion_Fn`, `AMFInit_Fn`
        // in AMD's Factory.h); a function pointer transmutes to another of the same size.
        let (query, init): (ffi::AmfQueryVersionFn, ffi::AmfInitFn) = unsafe {
            (
                std::mem::transmute::<unsafe extern "system" fn() -> isize, ffi::AmfQueryVersionFn>(
                    query,
                ),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, ffi::AmfInitFn>(init),
            )
        };
        let mut version = 0u64;
        // SAFETY: valid out-pointer to a local.
        let result = unsafe { query(&mut version) };
        if result != AMF_OK {
            return Err(format!(
                "AMF: AMFQueryVersion failed ({})",
                ffi::result_name(result)
            ));
        }
        let mut factory = ptr::null_mut();
        // SAFETY: valid out-pointer; the version asked for is one the runtime has.
        let result = unsafe { init(version.min(TARGET_VERSION), &mut factory) };
        if result != AMF_OK {
            return Err(format!(
                "AMF: AMFInit failed on runtime {} ({})",
                ffi::version_name(version),
                ffi::result_name(result)
            ));
        }
        let factory =
            NonNull::new(factory).ok_or_else(|| "AMF: AMFInit returned no factory".to_owned())?;
        Ok(Runtime { factory, version })
    }

    /// An AMF context on the context's D3D11 device: terminated and released on drop.
    struct Context(NonNull<AMFContext>);

    impl Drop for Context {
        fn drop(&mut self) {
            let this = self.0.as_ptr();
            // SAFETY: a live context this wrapper owns one reference to, released once.
            unsafe {
                ((*(*this).vtbl).terminate)(this);
                ((*(*this).vtbl).release)(this);
            }
        }
    }

    /// An encoder component: terminated and released on drop.
    struct Component(NonNull<AMFComponent>);

    impl Component {
        fn ptr(&self) -> *mut AMFComponent {
            self.0.as_ptr()
        }

        /// Sets one property.
        fn set(&self, name: &[u16], value: AMFVariantStruct) -> AmfResult {
            let this = self.ptr();
            // SAFETY: a live component; `name` is null-terminated (`wide!`), and AMF
            // copies the plain-data variant.
            unsafe { ((*(*this).vtbl).set_property)(this, name.as_ptr(), value) }
        }
    }

    impl Drop for Component {
        fn drop(&mut self) {
            let this = self.ptr();
            // SAFETY: a live component this wrapper owns one reference to, released once.
            unsafe {
                ((*(*this).vtbl).terminate)(this);
                ((*(*this).vtbl).release)(this);
            }
        }
    }

    /// One reference to an `AMFData` (a surface or an output buffer), released on drop.
    struct Data(NonNull<AMFData>);

    impl Data {
        fn ptr(&self) -> *mut AMFData {
            self.0.as_ptr()
        }

        /// Sets one property on the data.
        fn set(&self, name: &[u16], value: AMFVariantStruct) -> AmfResult {
            let this = self.ptr();
            // SAFETY: live data; `name` is null-terminated and the variant plain data.
            unsafe { ((*(*this).vtbl).set_property)(this, name.as_ptr(), value) }
        }

        /// An integer (or boolean) property, or `None` when it is missing.
        fn int(&self, name: &[u16]) -> Option<i64> {
            let this = self.ptr();
            let mut value = AMFVariantStruct::empty();
            // SAFETY: live data; `name` is null-terminated and `value` a valid
            // out-pointer. Only integer and boolean values are read, which own nothing,
            // so no `AMFVariantClear` is needed.
            let result = unsafe { ((*(*this).vtbl).get_property)(this, name.as_ptr(), &mut value) };
            (result == AMF_OK).then(|| value.as_i64()).flatten()
        }
    }

    impl Drop for Data {
        fn drop(&mut self) {
            let this = self.ptr();
            // SAFETY: live data this wrapper owns one reference to, released once.
            unsafe { ((*(*this).vtbl).release)(this) };
        }
    }

    /// The variant of a property value.
    fn variant(value: Value) -> AMFVariantStruct {
        let (kind, value) = match value {
            Value::Int(v) => (AMF_VARIANT_INT64, AMFVariantValue { int64_value: v }),
            Value::Bool(v) => (
                AMF_VARIANT_BOOL,
                AMFVariantValue {
                    bool_value: u8::from(v),
                },
            ),
            Value::Size(width, height) => (
                AMF_VARIANT_SIZE,
                AMFVariantValue {
                    size_value: AMFSize { width, height },
                },
            ),
            Value::Rate(num, den) => (
                AMF_VARIANT_RATE,
                AMFVariantValue {
                    rate_value: AMFRate { num, den },
                },
            ),
        };
        // Zeroed first, so the bytes past a short member are defined.
        let mut v = AMFVariantStruct::empty();
        v.kind = kind;
        v.value = value;
        v
    }

    /// A property name as text, for messages.
    fn text(name: &[u16]) -> String {
        String::from_utf16_lossy(name.strip_suffix(&[0]).unwrap_or(name))
    }

    /// One H.264, HEVC or AV1 session on an AMF hardware encoder, reading the context's
    /// encode targets.
    pub(crate) struct AmfEncoder {
        // Fields drop in order: the component and context release their references to
        // the textures and device before the ring (and its `EncodeGpu`) goes.
        component: Component,
        _context: Context,
        ring: Ring,
        slots: Slots,
        codec: Codec,
        info: CodecInfo,
        config: VideoEncoderConfig,
        settings: Settings,
        /// AMF refused the last input as full; no more until output is taken.
        input_full: bool,
        /// The next frame is to be a keyframe.
        keyframe: bool,
        pending: Vec<Packet>,
        finished: bool,
        eof: bool,
    }

    // SAFETY: the session is owned and driven by one thread at a time (`&mut self` for
    // every call except `can_accept`, which reads plain fields). AMF's context and
    // component may be called from any thread as long as calls don't overlap, which
    // `&mut self` guarantees; the context's D3D11 device is multithread-protected.
    unsafe impl Send for AmfEncoder {}

    impl AmfEncoder {
        /// Opens an AMF encoder for `config` on `gpu`'s context.
        ///
        /// # Errors
        /// [`PlatformError::Unsupported`] when the configuration isn't NV12 into one of
        /// the three codecs, the AMF runtime is missing or too old, or the encoder
        /// refuses the configuration (no such hardware encoder, or a required property
        /// refused), and [`PlatformError::Os`] when AMF or the device fails otherwise.
        pub(crate) fn open(
            config: &VideoEncoderConfig,
            gpu: &EncodeGpu,
        ) -> Result<Self, PlatformError> {
            let settings = check(config)?;
            if config.input_format != PixelFormat::Nv12 {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "AMF from {:?} encode targets (they are NV12)",
                        config.input_format
                    ),
                });
            }
            let codec = codec(settings.format);
            let runtime = runtime()?;
            if runtime.version < codec.min_version {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "AMF {} needs runtime {} or newer; the driver has {}",
                        codec.name,
                        ffi::version_name(codec.min_version),
                        ffi::version_name(runtime.version)
                    ),
                });
            }
            let refused = |call: &'static str, result: AmfResult| PlatformError::Unsupported {
                what: format!(
                    "AMF {}: {call} failed ({})",
                    codec.name,
                    ffi::result_name(result)
                ),
            };
            let factory = runtime.factory.as_ptr();
            let mut raw = ptr::null_mut();
            // SAFETY: the factory lives for the process; valid out-pointer.
            let result = unsafe { ((*(*factory).vtbl).create_context)(factory, &mut raw) };
            ok("AMFFactory::CreateContext", result)?;
            let context = Context(
                NonNull::new(raw)
                    .ok_or(amf_error("AMFFactory::CreateContext", ffi::AMF_NOT_FOUND))?,
            );
            let ctx = context.0.as_ptr();
            // SAFETY: a live context; the device is the context's D3D11 device, live for
            // the session (the ring's `EncodeGpu` holds it), and AMF takes its own
            // reference.
            let result =
                unsafe { ((*(*ctx).vtbl).init_dx11)(ctx, gpu.device().as_raw(), AMF_DX11_0) };
            if result != AMF_OK {
                return Err(refused("AMFContext::InitDX11", result));
            }
            let mut raw = ptr::null_mut();
            // SAFETY: live factory and context; the ID is null-terminated; valid
            // out-pointer.
            let result = unsafe {
                ((*(*factory).vtbl).create_component)(
                    factory,
                    ctx,
                    codec.component.as_ptr(),
                    &mut raw,
                )
            };
            if result != AMF_OK {
                return Err(refused("AMFFactory::CreateComponent", result));
            }
            let component = Component(
                NonNull::new(raw)
                    .ok_or(amf_error("AMFFactory::CreateComponent", ffi::AMF_NOT_FOUND))?,
            );
            let (w, h) = (config.size.width(), config.size.height());
            for property in properties(&settings, config.keyframe_interval, w, h) {
                let result = component.set(property.name, variant(property.value));
                if result != AMF_OK && property.required {
                    return Err(PlatformError::Unsupported {
                        what: format!(
                            "AMF {} refused {} = {:?} ({})",
                            codec.name,
                            text(property.name),
                            property.value,
                            ffi::result_name(result)
                        ),
                    });
                }
            }
            let c = component.ptr();
            // SAFETY: a live, configured component. NV12 at the configured size, which is
            // even (`check`) and positive.
            let result = unsafe {
                ((*(*c).vtbl).init)(
                    c,
                    AMF_SURFACE_NV12,
                    i32::try_from(w).unwrap_or(i32::MAX),
                    i32::try_from(h).unwrap_or(i32::MAX),
                )
            };
            if result != AMF_OK {
                return Err(refused("AMFComponent::Init", result));
            }
            let ring = Ring::new(gpu, config.size, SLOTS)?;
            Ok(Self {
                component,
                _context: context,
                slots: Slots::new(ring.len()),
                ring,
                codec,
                info: CodecInfo {
                    name: format!("AMD AMF {} (direct)", codec.name),
                    codec: settings.format.codec(),
                    role: CodecRole::VideoEncoder,
                    hardware: true,
                },
                config: *config,
                settings,
                input_full: false,
                keyframe: false,
                pending: Vec::new(),
                finished: false,
                eof: false,
            })
        }

        /// Wraps `texture` (one of the ring's) as an AMF surface.
        fn surface(&self, slot: usize) -> Result<Data, PlatformError> {
            let ctx = self._context.0.as_ptr();
            let mut raw: *mut AMFSurface = ptr::null_mut();
            // SAFETY: a live context initialised on the texture's device; the texture is
            // live for the session (the ring holds it), and AMF takes its own reference.
            let result = unsafe {
                ((*(*ctx).vtbl).create_surface_from_dx11_native)(
                    ctx,
                    self.ring.texture(slot).as_raw(),
                    &mut raw,
                    ptr::null_mut(),
                )
            };
            ok("AMFContext::CreateSurfaceFromDX11Native", result)?;
            // An `AMFSurface` is an `AMFData` (single inheritance; module `ffi`).
            NonNull::new(raw.cast::<AMFData>())
                .map(Data)
                .ok_or(amf_error(
                    "AMFContext::CreateSurfaceFromDX11Native",
                    ffi::AMF_NOT_FOUND,
                ))
        }

        /// Takes every output buffer that is ready, without waiting.
        fn collect(&mut self) -> Result<(), PlatformError> {
            let c = self.component.ptr();
            loop {
                let mut raw = ptr::null_mut();
                // SAFETY: a live component; valid out-pointer. The query timeout is 0,
                // so the call doesn't wait.
                let result = unsafe { ((*(*c).vtbl).query_output)(c, &mut raw) };
                if let Some(data) = NonNull::new(raw).map(Data) {
                    self.input_full = false;
                    self.packet(&data)?;
                }
                match result {
                    AMF_OK if !raw.is_null() => {}
                    AMF_OK | AMF_REPEAT | AMF_NEED_MORE_INPUT => return Ok(()),
                    AMF_EOF => {
                        self.eof = true;
                        return Ok(());
                    }
                    other => return Err(amf_error("AMFComponent::QueryOutput", other)),
                }
            }
        }

        /// Turns one output buffer into a packet.
        fn packet(&mut self, data: &Data) -> Result<(), PlatformError> {
            let mut raw: *mut c_void = ptr::null_mut();
            let this = data.ptr();
            // SAFETY: live data; the IID is AMFBuffer's and `raw` a valid out-pointer,
            // which on success holds a new reference.
            let result =
                unsafe { ((*(*this).vtbl).query_interface)(this, &IID_AMF_BUFFER, &mut raw) };
            ok("AMFData::QueryInterface (AMFBuffer)", result)?;
            let buffer = NonNull::new(raw.cast::<AMFBuffer>()).ok_or(amf_error(
                "AMFData::QueryInterface (AMFBuffer)",
                ffi::AMF_NOT_FOUND,
            ))?;
            // Released with the bytes copied out.
            let _buffer_ref = Data(buffer.cast::<AMFData>());
            let b = buffer.as_ptr();
            // SAFETY: a live buffer in host memory (the encoder's output); `GetNative`
            // points at `GetSize` bytes that stay valid while the reference is held.
            let bytes = unsafe {
                let size = ((*(*b).vtbl).get_size)(b);
                let native = ((*(*b).vtbl).get_native)(b).cast::<u8>();
                if native.is_null() || size == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(native, size).to_vec()
                }
            };
            if bytes.is_empty() {
                return Ok(());
            }
            // SAFETY: live data.
            let pts_hns = unsafe { ((*(*this).vtbl).get_pts)(this) };
            let keyframe = data.int(self.codec.output_type.0) == Some(self.codec.output_type.1);
            let seq = sequence(data.int(SEQ_PROPERTY), pts_hns, self.settings.frame_hns);
            let pts = seq
                .and_then(|seq| self.slots.complete(seq))
                .map_or_else(|| MediaTime::new(pts_hns, hns()), |sent| sent.pts);
            self.pending.push(Packet::new(
                self.settings.format.codec(),
                keyframe,
                pts,
                pts,
                bytes,
            )?);
            Ok(())
        }
    }

    impl VideoEncoder for AmfEncoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        /// Reflects the last `encode` or `poll`: a free input texture, and AMF not
        /// having said its input is full since output was last taken.
        fn can_accept(&self) -> bool {
            !self.finished && !self.input_full && self.slots.peek_free().is_some()
        }

        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "video encode",
                    state: "the encoder is finished",
                });
            }
            let VideoSurface::Gpu(frame) = frame else {
                return Err(PlatformError::Unsupported {
                    what: "a CPU frame on the GPU video encoder path".into(),
                });
            };
            let info = *frame.info();
            if info.size != self.config.size || info.format != PixelFormat::Nv12 {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "a {:?} {}x{} frame (configured for NV12 {}x{})",
                        info.format,
                        info.size.width(),
                        info.size.height(),
                        self.config.size.width(),
                        self.config.size.height()
                    ),
                });
            }
            if self.input_full {
                self.collect()?;
            }
            let Some(slot) = self.slots.peek_free().filter(|_| !self.input_full) else {
                return Err(PlatformError::Busy {
                    what: "AMF encoder input",
                });
            };
            self.ring.copy(&frame, slot)?;
            // The encode target goes back to its producer now; the copy is queued.
            drop(frame);
            let surface = self.surface(slot)?;
            let sent = self.slots.send(slot, info.pts);
            let frame_hns = self.settings.frame_hns;
            let s = surface.ptr();
            let seq = i64::try_from(sent.seq).unwrap_or(i64::MAX);
            // SAFETY: a live surface.
            unsafe {
                ((*(*s).vtbl).set_pts)(s, seq.saturating_mul(frame_hns));
                ((*(*s).vtbl).set_duration)(s, frame_hns);
            }
            let _ = surface.set(SEQ_PROPERTY, variant(Value::Int(seq)));
            if self.keyframe {
                let (force, idr) = self.codec.force;
                let _ = surface.set(force, variant(Value::Int(idr)));
                for header in self.codec.headers {
                    let _ = surface.set(header, variant(Value::Bool(true)));
                }
            }
            let c = self.component.ptr();
            // SAFETY: a live component and surface; AMF takes its own reference to the
            // surface, ours is released when `surface` drops.
            let result = unsafe { ((*(*c).vtbl).submit_input)(c, s) };
            drop(surface);
            match result {
                AMF_OK | AMF_NEED_MORE_INPUT => self.keyframe = false,
                AMF_INPUT_FULL => {
                    self.slots.unsend(sent.seq);
                    self.input_full = true;
                    return Err(PlatformError::Busy {
                        what: "AMF encoder input",
                    });
                }
                other => {
                    self.slots.unsend(sent.seq);
                    return Err(amf_error("AMFComponent::SubmitInput", other));
                }
            }
            self.collect()
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.eof {
                self.collect()?;
            }
            out.append(&mut self.pending);
            Ok(())
        }

        /// The next frame submitted forces an IDR (AV1: key) frame, with the parameter
        /// sets (sequence header) inserted.
        fn request_keyframe(&mut self) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "video keyframe request",
                    state: "the encoder is finished",
                });
            }
            self.keyframe = true;
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if self.finished {
                out.append(&mut self.pending);
                return Ok(());
            }
            self.finished = true;
            let deadline = Instant::now() + DRAIN_TIMEOUT;
            let c = self.component.ptr();
            loop {
                // SAFETY: a live component.
                let result = unsafe { ((*(*c).vtbl).drain)(c) };
                match result {
                    AMF_OK | AMF_EOF => break,
                    // Output must be taken before the drain is accepted.
                    AMF_INPUT_FULL if Instant::now() < deadline => {
                        self.collect()?;
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    other => {
                        out.append(&mut self.pending);
                        return Err(amf_error("AMFComponent::Drain", other));
                    }
                }
            }
            while !self.eof {
                self.collect()?;
                if self.eof {
                    break;
                }
                if Instant::now() > deadline {
                    out.append(&mut self.pending);
                    return Err(PlatformError::Os {
                        call: "AMFComponent::Drain",
                        code: None,
                        message: format!(
                            "the encoder didn't drain within {} s ({} frames still in flight)",
                            DRAIN_TIMEOUT.as_secs(),
                            self.slots.in_flight()
                        ),
                    });
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            out.append(&mut self.pending);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Format, Property, Value, codec, properties, sequence};
    use crate::codec::video::Settings;

    fn text(name: &[u16]) -> String {
        assert_eq!(name.last(), Some(&0), "null-terminated");
        String::from_utf16(&name[..name.len() - 1]).expect("utf-16")
    }

    fn settings(format: Format, max_bitrate: Option<u32>) -> Settings {
        Settings {
            format,
            fps: (30_000, 1001),
            frame_hns: 333_667,
            bitrate: 6_000_000,
            max_bitrate,
        }
    }

    fn find(props: &[Property], name: &str) -> Option<Property> {
        props.iter().copied().find(|p| text(p.name) == name)
    }

    #[test]
    fn usage_goes_first_and_every_name_is_wide_and_terminated() {
        for (format, usage) in [
            (Format::H264, "Usage"),
            (Format::Hevc, "HevcUsage"),
            (Format::Av1, "Av1Usage"),
        ] {
            let props = properties(&settings(format, None), 60, 1920, 1080);
            assert_eq!(text(props[0].name), usage);
            assert!(props[0].required);
            let mut names: Vec<_> = props.iter().map(|p| text(p.name)).collect();
            let n = names.len();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), n, "{format:?}: a property set twice");
        }
    }

    #[test]
    fn h264_is_high_profile_cbr_with_the_keyframe_interval_and_no_b_frames() {
        let props = properties(&settings(Format::H264, None), 120, 1280, 720);
        let value = |n| find(&props, n).map(|p| p.value);
        assert_eq!(value("Profile"), Some(Value::Int(100)));
        assert_eq!(value("FrameSize"), Some(Value::Size(1280, 720)));
        assert_eq!(value("FrameRate"), Some(Value::Rate(30_000, 1001)));
        assert_eq!(value("RateControlMethod"), Some(Value::Int(1)));
        assert_eq!(value("TargetBitrate"), Some(Value::Int(6_000_000)));
        assert_eq!(value("PeakBitrate"), Some(Value::Int(6_000_000)));
        assert_eq!(value("VBVBufferSize"), Some(Value::Int(6_000_000)));
        assert_eq!(value("FillerDataEnable"), Some(Value::Bool(true)));
        assert_eq!(value("IDRPeriod"), Some(Value::Int(120)));
        assert_eq!(value("HeaderInsertionSpacing"), Some(Value::Int(120)));
        assert_eq!(value("BPicturesPattern"), Some(Value::Int(0)));
        assert_eq!(value("QualityPreset"), Some(Value::Int(2)));
        assert_eq!(value("OutColorPrimaries"), Some(Value::Int(1)));
        assert_eq!(value("FullRangeColor"), Some(Value::Bool(false)));
        for required in [
            "Usage",
            "FrameSize",
            "FrameRate",
            "RateControlMethod",
            "TargetBitrate",
            "IDRPeriod",
        ] {
            assert!(
                find(&props, required).expect(required).required,
                "{required}"
            );
        }
        assert!(!find(&props, "OutColorProfile").expect("colour").required);
    }

    #[test]
    fn vbr_peaks_and_hevc_and_av1_use_their_own_numbers() {
        let s = settings(Format::H264, Some(9_000_000));
        let props = properties(&s, 60, 1920, 1080);
        assert_eq!(
            find(&props, "RateControlMethod").map(|p| p.value),
            Some(Value::Int(2))
        );
        assert_eq!(
            find(&props, "PeakBitrate").map(|p| p.value),
            Some(Value::Int(9_000_000))
        );
        assert_eq!(
            find(&props, "VBVBufferSize").map(|p| p.value),
            Some(Value::Int(9_000_000))
        );
        assert_eq!(
            find(&props, "FillerDataEnable").map(|p| p.value),
            Some(Value::Bool(false))
        );

        let hevc = properties(&settings(Format::Hevc, None), 60, 1920, 1080);
        let value = |n| find(&hevc, n).map(|p| p.value);
        assert_eq!(
            value("HevcRateControlMethod"),
            Some(Value::Int(3)),
            "HEVC's CBR is 3"
        );
        assert_eq!(value("HevcProfile"), Some(Value::Int(1)));
        assert_eq!(value("HevcGOPSize"), Some(Value::Int(60)));
        assert_eq!(value("HevcGOPSPerIDR"), Some(Value::Int(1)));
        assert_eq!(value("HevcHeaderInsertionMode"), Some(Value::Int(2)));
        assert_eq!(
            value("HevcQualityPreset"),
            Some(Value::Int(0)),
            "HEVC's quality is 0"
        );
        let hevc_vbr = properties(&settings(Format::Hevc, Some(8_000_000)), 60, 1920, 1080);
        assert_eq!(
            find(&hevc_vbr, "HevcRateControlMethod").map(|p| p.value),
            Some(Value::Int(2))
        );

        let av1 = properties(&settings(Format::Av1, None), 60, 1920, 1080);
        let value = |n| find(&av1, n).map(|p| p.value);
        assert_eq!(value("Av1RateControlMethod"), Some(Value::Int(3)));
        assert_eq!(value("Av1QualityPreset"), Some(Value::Int(30)));
        assert_eq!(value("Av1GOPSize"), Some(Value::Int(60)));
        assert_eq!(value("Av1HeaderInsertionMode"), Some(Value::Int(2)));
        assert_eq!(value("Av1AlignmentMode"), Some(Value::Int(3)));
        assert_eq!(value("AV1OutputMode"), Some(Value::Int(0)));
    }

    #[test]
    fn each_codec_names_its_component_and_keyframe_properties() {
        let h264 = codec(Format::H264);
        assert_eq!(text(h264.component), "AMFVideoEncoderVCE_AVC");
        assert_eq!(text(h264.force.0), "ForcePictureType");
        assert_eq!(h264.force.1, 2);
        assert_eq!(text(h264.output_type.0), "OutputDataType");
        let headers: Vec<_> = h264.headers.iter().map(|h| text(h)).collect();
        assert_eq!(headers, ["InsertSPS", "InsertPPS"]);
        let hevc = codec(Format::Hevc);
        assert_eq!(text(hevc.component), "AMFVideoEncoderHW_HEVC");
        assert_eq!(text(hevc.force.0), "HevcForcePictureType");
        assert_eq!(text(hevc.output_type.0), "HevcOutputDataType");
        let av1 = codec(Format::Av1);
        assert_eq!(text(av1.component), "AMFVideoEncoderHW_AV1");
        assert_eq!(
            (text(av1.force.0), av1.force.1),
            ("Av1ForceFrameType".into(), 1)
        );
        assert_eq!(text(av1.output_type.0), "Av1OutputFrameType");
        assert!(av1.min_version > h264.min_version);
    }

    #[test]
    fn a_packets_sequence_comes_from_its_property_or_its_pts() {
        assert_eq!(sequence(Some(7), 0, 333_667), Some(7));
        assert_eq!(sequence(Some(-1), 0, 333_667), None);
        assert_eq!(sequence(None, 3 * 333_667, 333_667), Some(3));
        assert_eq!(
            sequence(None, 3 * 333_667 + 1, 333_667),
            None,
            "not a frame boundary"
        );
        assert_eq!(sequence(None, 100, 0), None);
        assert_eq!(sequence(None, -333_667, 333_667), None);
    }
}
