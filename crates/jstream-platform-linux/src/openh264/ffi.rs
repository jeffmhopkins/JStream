//! Hand-written declarations for the OpenH264 encoder and decoder calls JStream makes
//! (ADR-0140).
//!
//! They follow `codec_api.h`, `codec_app_def.h` and `codec_def.h` at the pinned release
//! (v2.6.0) and cover only the calls the encoder and decoder make. Several structs gained fields in that release
//! (`bPsnr*`, `rPsnr`), so the layouts belong to the pin: [`Api::load`] refuses a library
//! that reports another version. `int` is `c_int`, C enums are `c_int`, `long` is `c_long`,
//! `long long` is `i64` and `bool` is Rust's `bool` (both one byte, 0 or 1, on Linux).

use std::ffi::{c_int, c_long, c_uchar, c_uint, c_void};

use jstream_platform::PlatformError;

use crate::dl::SystemLibrary;

/// `EUsageType::CAMERA_VIDEO_REAL_TIME`: natural video in real time, the general mode.
pub(super) const CAMERA_VIDEO_REAL_TIME: c_int = 0;
/// `RC_MODES::RC_BITRATE_MODE`.
pub(super) const RC_BITRATE_MODE: c_int = 1;
/// `EVideoFormatType::videoFormatI420`.
pub(super) const VIDEO_FORMAT_I420: c_int = 23;
/// `EVideoFrameType::videoFrameTypeIDR`.
pub(super) const FRAME_TYPE_IDR: c_int = 1;
/// `EVideoFrameType::videoFrameTypeSkip`: no output for this frame.
pub(super) const FRAME_TYPE_SKIP: c_int = 4;
/// `EVideoFrameType::videoFrameTypeInvalid`.
pub(super) const FRAME_TYPE_INVALID: c_int = 0;
/// `ENCODER_OPTION_IDR_INTERVAL` (an `int`, in frames).
pub(super) const OPTION_IDR_INTERVAL: c_int = 1;
/// `ENCODER_OPTION_MAX_BITRATE` (an `SBitrateInfo`, [`BitrateInfo`]).
pub(super) const OPTION_MAX_BITRATE: c_int = 6;
/// `LAYER_NUM::SPATIAL_LAYER_0`: the first spatial layer, the only one this encoder has.
pub(super) const SPATIAL_LAYER_0: c_int = 0;
/// `ENCODER_OPTION_RC_FRAME_SKIP` (a `bool`).
pub(super) const OPTION_RC_FRAME_SKIP: c_int = 9;
/// `ENCODER_OPTION_TRACE_LEVEL` (a `uint32_t`; the levels fit in `c_int`, the same size).
pub(super) const OPTION_TRACE_LEVEL: c_int = 25;
/// `WELS_LOG_QUIET`: the library writes nothing to stdout or stderr (R-RS-005); every
/// failure still comes back as a return code.
pub(super) const WELS_LOG_QUIET: c_int = 0;
/// `MAX_LAYER_NUM_OF_FRAME`.
pub(super) const MAX_LAYERS: usize = 128;

/// `SBitrateInfo`, the value of [`OPTION_MAX_BITRATE`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct BitrateInfo {
    /// A `LAYER_NUM`, here [`SPATIAL_LAYER_0`].
    pub layer: c_int,
    /// Bits per second.
    pub bitrate: c_int,
}

/// `SEncParamBase`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct EncParamBase {
    pub usage_type: c_int,
    pub pic_width: c_int,
    pub pic_height: c_int,
    /// Bits per second.
    pub target_bitrate: c_int,
    pub rc_mode: c_int,
    pub max_frame_rate: f32,
}

/// `SSourcePicture`.
#[repr(C)]
#[derive(Debug)]
pub(super) struct SourcePicture {
    pub color_format: c_int,
    pub stride: [c_int; 4],
    /// The encoder only reads the planes; the C type isn't `const`.
    pub data: [*mut u8; 4],
    pub pic_width: c_int,
    pub pic_height: c_int,
    /// Milliseconds.
    pub time_stamp: i64,
    pub psnr_y: bool,
    pub psnr_u: bool,
    pub psnr_v: bool,
}

/// `SLayerBSInfo`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct LayerBsInfo {
    pub temporal_id: u8,
    pub spatial_id: u8,
    pub quality_id: u8,
    pub frame_type: c_int,
    pub layer_type: u8,
    pub sub_seq_id: c_int,
    pub nal_count: c_int,
    pub nal_length_in_byte: *mut c_int,
    pub bs_buf: *mut u8,
    pub psnr: [f32; 3],
}

/// `SFrameBSInfo`.
#[repr(C)]
#[derive(Debug)]
pub(super) struct FrameBsInfo {
    pub layer_num: c_int,
    pub layer_info: [LayerBsInfo; MAX_LAYERS],
    pub frame_type: c_int,
    pub frame_size_in_bytes: c_int,
    pub time_stamp: i64,
}

impl FrameBsInfo {
    /// An empty result, for the encoder to fill.
    pub(super) fn zeroed() -> Box<Self> {
        let layer = LayerBsInfo {
            temporal_id: 0,
            spatial_id: 0,
            quality_id: 0,
            frame_type: FRAME_TYPE_INVALID,
            layer_type: 0,
            sub_seq_id: 0,
            nal_count: 0,
            nal_length_in_byte: std::ptr::null_mut(),
            bs_buf: std::ptr::null_mut(),
            psnr: [0.0; 3],
        };
        Box::new(Self {
            layer_num: 0,
            layer_info: [layer; MAX_LAYERS],
            frame_type: FRAME_TYPE_INVALID,
            frame_size_in_bytes: 0,
            time_stamp: 0,
        })
    }
}

/// `ISVCEncoder`: a pointer to the vtable. The library's encoder object starts with it.
pub(super) type Encoder = *const EncoderVtbl;

/// `ISVCEncoderVtbl`, in declaration order. Only some entries are called; the rest are
/// declared so the offsets are right.
#[repr(C)]
pub(super) struct EncoderVtbl {
    pub initialize: unsafe extern "C" fn(*mut Encoder, *const EncParamBase) -> c_int,
    pub initialize_ext: unsafe extern "C" fn(*mut Encoder, *const c_void) -> c_int,
    pub get_default_params: unsafe extern "C" fn(*mut Encoder, *mut c_void) -> c_int,
    pub uninitialize: unsafe extern "C" fn(*mut Encoder) -> c_int,
    pub encode_frame:
        unsafe extern "C" fn(*mut Encoder, *const SourcePicture, *mut FrameBsInfo) -> c_int,
    pub encode_parameter_sets: unsafe extern "C" fn(*mut Encoder, *mut FrameBsInfo) -> c_int,
    pub force_intra_frame: unsafe extern "C" fn(*mut Encoder, bool) -> c_int,
    pub set_option: unsafe extern "C" fn(*mut Encoder, c_int, *mut c_void) -> c_int,
    pub get_option: unsafe extern "C" fn(*mut Encoder, c_int, *mut c_void) -> c_int,
}

/// `DECODING_STATE::dsErrorFree`: the call decoded without error.
pub(super) const DS_ERROR_FREE: c_int = 0;
/// `ERROR_CON_IDC::ERROR_CON_DISABLE`: no concealment, so a broken packet is an error
/// rather than a picture patched from the last one.
pub(super) const ERROR_CON_DISABLE: c_int = 0;
/// `VIDEO_BITSTREAM_TYPE::VIDEO_BITSTREAM_AVC`: plain H.264, not SVC.
pub(super) const VIDEO_BITSTREAM_AVC: c_int = 0;
/// `DECODER_OPTION_TRACE_LEVEL` (an `int`, a `WELS_LOG_*` level).
pub(super) const DECODER_OPTION_TRACE_LEVEL: c_int = 9;
/// `DECODER_OPTION_NUM_OF_FRAMES_REMAINING_IN_BUFFER` (an `int`, read with `GetOption`).
pub(super) const DECODER_OPTION_FRAMES_REMAINING: c_int = 18;

/// `SVideoProperty`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct VideoProperty {
    /// The struct's own size, in bytes.
    pub size: c_uint,
    /// A `VIDEO_BITSTREAM_TYPE`, here [`VIDEO_BITSTREAM_AVC`].
    pub bs_type: c_int,
}

/// `SDecodingParam`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct DecodingParam {
    /// A debug file the decoder writes its reconstruction to; always null.
    pub file_name_restructed: *mut std::ffi::c_char,
    pub cpu_load: c_uint,
    /// `UCHAR_MAX` decodes every layer.
    pub target_dq_layer: c_uchar,
    /// An `ERROR_CON_IDC`, here [`ERROR_CON_DISABLE`].
    pub ec_active_idc: c_int,
    pub parse_only: bool,
    pub video_property: VideoProperty,
}

/// `SSysMEMBuffer`: where a decoded picture is, once `iBufferStatus` is 1.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SysMemBuffer {
    pub width: c_int,
    pub height: c_int,
    /// An `EVideoFormatType`; [`VIDEO_FORMAT_I420`] for every decoded picture.
    pub format: c_int,
    /// The luma stride, then the stride of both chroma planes.
    pub stride: [c_int; 2],
}

/// `SBufferInfo`. Its `UsrData` union has the one member, `sSystemBuffer`.
#[repr(C)]
#[derive(Debug)]
pub(super) struct BufferInfo {
    /// 1 when the call put out a picture.
    pub buffer_status: c_int,
    /// The input's time stamp, which the decoder carries to the picture it decodes.
    pub in_bs_time_stamp: u64,
    /// The picture's time stamp, once `buffer_status` is 1.
    pub out_yuv_time_stamp: u64,
    pub system_buffer: SysMemBuffer,
    /// The picture's Y, U and V planes, owned by the decoder until its next call.
    pub dst: [*mut u8; 3],
}

impl BufferInfo {
    /// An empty one, for a call to fill, carrying `time_stamp` in.
    pub(super) fn with_time_stamp(time_stamp: u64) -> Self {
        Self {
            buffer_status: 0,
            in_bs_time_stamp: time_stamp,
            out_yuv_time_stamp: 0,
            system_buffer: SysMemBuffer::default(),
            dst: [std::ptr::null_mut(); 3],
        }
    }
}

/// `ISVCDecoder`: a pointer to the vtable. The library's decoder object starts with it.
pub(super) type Decoder = *const DecoderVtbl;

/// `ISVCDecoderVtbl`, in declaration order. Only some entries are called; the rest are
/// declared as untyped pointers so the offsets are right.
#[repr(C)]
pub(super) struct DecoderVtbl {
    pub initialize: unsafe extern "C" fn(*mut Decoder, *const DecodingParam) -> c_long,
    pub uninitialize: unsafe extern "C" fn(*mut Decoder) -> c_long,
    /// `DecodeFrame`, not called.
    pub decode_frame: *const c_void,
    pub decode_frame_no_delay: unsafe extern "C" fn(
        *mut Decoder,
        *const u8,
        c_int,
        *mut *mut u8,
        *mut BufferInfo,
    ) -> c_int,
    /// `DecodeFrame2`, not called.
    pub decode_frame2: *const c_void,
    pub flush_frame: unsafe extern "C" fn(*mut Decoder, *mut *mut u8, *mut BufferInfo) -> c_int,
    /// `DecodeParser`, not called.
    pub decode_parser: *const c_void,
    /// `DecodeFrameEx`, not called.
    pub decode_frame_ex: *const c_void,
    pub set_option: unsafe extern "C" fn(*mut Decoder, c_int, *mut c_void) -> c_long,
    pub get_option: unsafe extern "C" fn(*mut Decoder, c_int, *mut c_void) -> c_long,
}

/// `OpenH264Version`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Version {
    pub major: c_uint,
    pub minor: c_uint,
    pub revision: c_uint,
    pub reserved: c_uint,
}

/// `int WelsCreateSVCEncoder(ISVCEncoder** ppEncoder)`.
pub(super) type CreateFn = unsafe extern "C" fn(*mut *mut Encoder) -> c_int;
/// `void WelsDestroySVCEncoder(ISVCEncoder* pEncoder)`.
pub(super) type DestroyFn = unsafe extern "C" fn(*mut Encoder);
/// `long WelsCreateDecoder(ISVCDecoder** ppDecoder)`.
pub(super) type CreateDecoderFn = unsafe extern "C" fn(*mut *mut Decoder) -> c_long;
/// `void WelsDestroyDecoder(ISVCDecoder* pDecoder)`.
pub(super) type DestroyDecoderFn = unsafe extern "C" fn(*mut Decoder);
/// `void WelsGetCodecVersionEx(OpenH264Version* pVersion)`.
pub(super) type VersionFn = unsafe extern "C" fn(*mut Version);

/// The library's entry points, and the library they came from, kept loaded while any
/// encoder or decoder made from them lives.
#[derive(Debug)]
pub(super) struct Api {
    pub create: CreateFn,
    pub destroy: DestroyFn,
    /// The decoder's pair. `None` only for the tests' fake encoder library.
    pub decoder: Option<(CreateDecoderFn, DestroyDecoderFn)>,
    /// `None` for the fake library in tests.
    _lib: Option<SystemLibrary>,
}

impl Api {
    /// Resolves the entry points from `lib` and checks that it is release `expected`.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when a symbol is missing or the library is
    /// another release, whose struct layouts these declarations don't match.
    pub(super) fn load(
        lib: SystemLibrary,
        expected: (u32, u32, u32),
    ) -> Result<Self, PlatformError> {
        // SAFETY: the five types are the C declarations quoted on them, from
        // `codec_api.h` at the pinned release; `lib` is kept in the returned `Api`, so
        // the pointers are never called after it is unloaded.
        let (create, destroy, create_decoder, destroy_decoder, version) = unsafe {
            (
                lib.function::<CreateFn>("WelsCreateSVCEncoder")?,
                lib.function::<DestroyFn>("WelsDestroySVCEncoder")?,
                lib.function::<CreateDecoderFn>("WelsCreateDecoder")?,
                lib.function::<DestroyDecoderFn>("WelsDestroyDecoder")?,
                lib.function::<VersionFn>("WelsGetCodecVersionEx")?,
            )
        };
        let mut found = Version::default();
        // SAFETY: `WelsGetCodecVersionEx` writes one `OpenH264Version` through the
        // pointer, which points at a live, writable one.
        unsafe { version(&mut found) };
        if (found.major, found.minor, found.revision) != expected {
            return Err(PlatformError::MissingComponent {
                component: "Cisco's OpenH264",
                remedy: format!(
                    "the library is OpenH264 {}.{}.{}, not the {}.{}.{} this JStream pins; \
                     remove it and download it again",
                    found.major, found.minor, found.revision, expected.0, expected.1, expected.2
                ),
            });
        }
        Ok(Self {
            create,
            destroy,
            decoder: Some((create_decoder, destroy_decoder)),
            _lib: Some(lib),
        })
    }

    /// Entry points that aren't from a loaded library: the tests' fake encoder.
    #[cfg(test)]
    pub(super) fn fake(create: CreateFn, destroy: DestroyFn) -> Self {
        Self {
            create,
            destroy,
            decoder: None,
            _lib: None,
        }
    }
}
