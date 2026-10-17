//! Hand-written declarations for the OpenH264 encoder calls JStream makes (ADR-0061).
//!
//! They follow `codec_api.h`, `codec_app_def.h` and `codec_def.h` at the pinned release
//! (v2.6.0) and cover only the encoder. Several structs gained fields in that release
//! (`bPsnr*`, `rPsnr`), so the layouts belong to the pin: [`Api::load`] refuses a library
//! that reports another version. `int` is `c_int`, C enums are `c_int`, `long long` is
//! `i64` and `bool` is Rust's `bool` (both one byte, 0 or 1, on Linux).

use std::ffi::{c_int, c_uint, c_void};

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
/// `void WelsGetCodecVersionEx(OpenH264Version* pVersion)`.
pub(super) type VersionFn = unsafe extern "C" fn(*mut Version);

/// The library's entry points, and the library they came from, kept loaded while any
/// encoder made from them lives.
#[derive(Debug)]
pub(super) struct Api {
    pub create: CreateFn,
    pub destroy: DestroyFn,
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
        // SAFETY: the three types are the C declarations quoted on them, from
        // `codec_api.h` at the pinned release; `lib` is kept in the returned `Api`, so
        // the pointers are never called after it is unloaded.
        let (create, destroy, version) = unsafe {
            (
                lib.function::<CreateFn>("WelsCreateSVCEncoder")?,
                lib.function::<DestroyFn>("WelsDestroySVCEncoder")?,
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
            _lib: Some(lib),
        })
    }

    /// Entry points that aren't from a loaded library: the tests' fake encoder.
    #[cfg(test)]
    pub(super) fn fake(create: CreateFn, destroy: DestroyFn) -> Self {
        Self {
            create,
            destroy,
            _lib: None,
        }
    }
}
