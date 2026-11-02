//! The encoder wrapper against a fake OpenH264: Rust functions behind the same vtable,
//! recording what the wrapper asks of them. No library is loaded.

use std::cell::RefCell;
use std::ffi::{c_int, c_void};
use std::sync::Arc;

use jstream_platform::media::{
    Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{
    HardwarePreference, PlatformError, RateControl, VideoEncoder, VideoEncoderConfig, VideoSurface,
};

use super::super::ffi::{
    Api, BitrateInfo, EncParamBase, Encoder, EncoderVtbl, FRAME_TYPE_IDR, FrameBsInfo,
    OPTION_IDR_INTERVAL, OPTION_MAX_BITRATE, OPTION_RC_FRAME_SKIP, OPTION_TRACE_LEVEL,
    SPATIAL_LAYER_0, SourcePicture, VIDEO_FORMAT_I420,
};
use super::OpenH264Encoder;

const P_FRAME: c_int = 3;
const SKIP: c_int = 4;

/// What the fake saw, per test thread.
#[derive(Default)]
struct Log {
    params: Option<(c_int, c_int, c_int, c_int, c_int, f32)>,
    options: Vec<(c_int, i64)>,
    /// Per frame: the first U and V samples and the timestamp.
    frames: Vec<(u8, u8, i64)>,
    forced: usize,
    next_skip: bool,
    uninitialized: bool,
    destroyed: bool,
}

thread_local! {
    static LOG: RefCell<Log> = RefCell::new(Log::default());
}

/// The fake encoder object: the vtable pointer first, as the library lays it out, then
/// the buffers its output points into.
#[repr(C)]
struct Fake {
    vtbl: Encoder,
    nal_lengths: [c_int; 2],
    bitstream: Vec<u8>,
    frames: usize,
    force: bool,
}

fn fake<'a>(e: *mut Encoder) -> &'a mut Fake {
    // SAFETY: every encoder pointer the fake hands out is a leaked `Box<Fake>`, whose
    // first field is the vtable pointer.
    unsafe { &mut *e.cast::<Fake>() }
}

unsafe extern "C" fn initialize(_: *mut Encoder, p: *const EncParamBase) -> c_int {
    // SAFETY: the wrapper passes a live `SEncParamBase`.
    let p = unsafe { *p };
    LOG.with_borrow_mut(|l| {
        l.params = Some((
            p.usage_type,
            p.pic_width,
            p.pic_height,
            p.target_bitrate,
            p.rc_mode,
            p.max_frame_rate,
        ));
    });
    0
}
unsafe extern "C" fn unused_ptr(_: *mut Encoder, _: *const c_void) -> c_int {
    -1
}
unsafe extern "C" fn unused_mut(_: *mut Encoder, _: *mut c_void) -> c_int {
    -1
}
unsafe extern "C" fn unused_get(_: *mut Encoder, _: c_int, _: *mut c_void) -> c_int {
    -1
}
unsafe extern "C" fn uninitialize(_: *mut Encoder) -> c_int {
    LOG.with_borrow_mut(|l| l.uninitialized = true);
    0
}
unsafe extern "C" fn encode_frame(
    e: *mut Encoder,
    pic: *const SourcePicture,
    out: *mut FrameBsInfo,
) -> c_int {
    let f = fake(e);
    // SAFETY: the wrapper passes a live picture and result.
    let (pic, out) = unsafe { (&*pic, &mut *out) };
    assert_eq!(pic.color_format, VIDEO_FORMAT_I420);
    // SAFETY: the U and V planes hold at least one sample.
    let (u, v) = unsafe { (*pic.data[1], *pic.data[2]) };
    let skip = LOG.with_borrow_mut(|l| {
        l.frames.push((u, v, pic.time_stamp));
        std::mem::take(&mut l.next_skip)
    });
    if skip {
        out.frame_type = SKIP;
        out.layer_num = 0;
        return 0;
    }
    let idr = f.frames == 0 || f.force;
    f.force = false;
    f.frames += 1;
    // Two NAL units in one layer: a short header and a slice.
    f.bitstream = if idr {
        vec![0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x65, 0xBB, 0xCC]
    } else {
        vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x41, 0xDD, 0xEE]
    };
    f.nal_lengths = [6, 7];
    out.layer_num = 1;
    out.layer_info[0].nal_count = 2;
    out.layer_info[0].nal_length_in_byte = f.nal_lengths.as_mut_ptr();
    out.layer_info[0].bs_buf = f.bitstream.as_mut_ptr();
    out.frame_type = if idr { FRAME_TYPE_IDR } else { P_FRAME };
    out.frame_size_in_bytes = 13;
    0
}
unsafe extern "C" fn encode_parameter_sets(_: *mut Encoder, _: *mut FrameBsInfo) -> c_int {
    -1
}
unsafe extern "C" fn force_intra_frame(e: *mut Encoder, idr: bool) -> c_int {
    assert!(idr);
    fake(e).force = true;
    LOG.with_borrow_mut(|l| l.forced += 1);
    0
}
unsafe extern "C" fn set_option(_: *mut Encoder, option: c_int, value: *mut c_void) -> c_int {
    // SAFETY: each option's value is the type its constant names.
    let v = unsafe {
        if option == OPTION_RC_FRAME_SKIP {
            i64::from(*value.cast::<bool>())
        } else if option == OPTION_MAX_BITRATE {
            // `SBitrateInfo`, for the one spatial layer.
            let info = *value.cast::<BitrateInfo>();
            assert_eq!(info.layer, SPATIAL_LAYER_0);
            i64::from(info.bitrate)
        } else {
            i64::from(*value.cast::<c_int>())
        }
    };
    LOG.with_borrow_mut(|l| l.options.push((option, v)));
    0
}

static VTBL: EncoderVtbl = EncoderVtbl {
    initialize,
    initialize_ext: unused_ptr,
    get_default_params: unused_mut,
    uninitialize,
    encode_frame,
    encode_parameter_sets,
    force_intra_frame,
    set_option,
    get_option: unused_get,
};

unsafe extern "C" fn create(out: *mut *mut Encoder) -> c_int {
    let f = Box::new(Fake {
        vtbl: &raw const VTBL,
        nal_lengths: [0; 2],
        bitstream: Vec::new(),
        frames: 0,
        force: false,
    });
    // SAFETY: the wrapper passes a live out pointer.
    unsafe { *out = Box::into_raw(f).cast() };
    0
}
unsafe extern "C" fn destroy(e: *mut Encoder) {
    // SAFETY: `e` is the `Box<Fake>` `create` leaked, destroyed once.
    drop(unsafe { Box::from_raw(e.cast::<Fake>()) });
    LOG.with_borrow_mut(|l| l.destroyed = true);
}

fn config(format: PixelFormat, rate_control: RateControl) -> VideoEncoderConfig {
    VideoEncoderConfig {
        codec: Codec::H264,
        size: FrameSize::new(4, 2).expect("valid size"),
        input_format: format,
        frame_period: Timebase::new(1, 30).expect("valid period"),
        rate_control,
        keyframe_interval: 60,
        hardware: HardwarePreference::Avoid,
    }
}

/// A 4x2 NV12 frame whose chroma row is U=10, V=20, U=11, V=21, at `ms`.
fn nv12(ms: i64) -> VideoSurface {
    let info = VideoFrameInfo {
        size: FrameSize::new(4, 2).expect("valid size"),
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: MediaTime::new(ms, Timebase::MILLIS),
    };
    let planes = vec![
        Plane::new(vec![0; 8], 4),
        Plane::new(vec![10, 20, 11, 21], 4),
    ];
    VideoSurface::Cpu(VideoFrame::new(info, planes).expect("frame"))
}

fn open(cfg: &VideoEncoderConfig) -> OpenH264Encoder {
    LOG.with_borrow_mut(|l| *l = Log::default());
    OpenH264Encoder::open(Arc::new(Api::fake(create, destroy)), cfg).expect("opens")
}

#[test]
fn configures_the_library_from_the_config() {
    let enc = open(&config(
        PixelFormat::Nv12,
        RateControl::Vbr {
            kbps: 3000,
            max_kbps: 4500,
        },
    ));
    LOG.with_borrow(|l| {
        assert_eq!(l.params, Some((0, 4, 2, 3_000_000, 1, 30.0)));
        assert_eq!(
            l.options,
            [
                (OPTION_TRACE_LEVEL, 0),
                (OPTION_IDR_INTERVAL, 60),
                (OPTION_RC_FRAME_SKIP, 0),
                (OPTION_MAX_BITRATE, 4_500_000),
            ]
        );
    });
    assert_eq!(enc.info().name, super::OPENH264_ENCODER_NAME);
    assert!(!enc.info().hardware);
    drop(enc);
    LOG.with_borrow(|l| assert!(l.uninitialized && l.destroyed));
}

#[test]
fn encodes_frames_into_annex_b_packets() {
    let mut enc = open(&config(PixelFormat::Nv12, RateControl::Cbr { kbps: 2500 }));
    let mut out = Vec::new();
    enc.encode(nv12(0)).expect("encodes");
    enc.encode(nv12(33)).expect("encodes");
    enc.poll(&mut out).expect("polls");
    assert_eq!(out.len(), 2);
    assert!(out[0].is_keyframe());
    assert_eq!(
        out[0].data(),
        [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x65, 0xBB, 0xCC]
    );
    assert!(!out[1].is_keyframe());
    assert_eq!(out[1].pts(), MediaTime::new(33, Timebase::MILLIS));
    assert_eq!(out[1].dts(), out[1].pts());
    // NV12's interleaved chroma reaches the library as separate U and V planes, and the
    // timestamp in milliseconds.
    LOG.with_borrow(|l| assert_eq!(l.frames, [(10, 20, 0), (10, 20, 33)]));

    // A requested keyframe is forced once, on the next frame.
    enc.request_keyframe().expect("requests");
    enc.request_keyframe().expect("requests");
    enc.encode(nv12(66)).expect("encodes");
    // A skipped frame makes no packet.
    LOG.with_borrow_mut(|l| l.next_skip = true);
    enc.encode(nv12(100)).expect("encodes");
    out.clear();
    enc.finish(&mut out).expect("finishes");
    assert_eq!(out.len(), 1);
    assert!(out[0].is_keyframe());
    LOG.with_borrow(|l| assert_eq!(l.forced, 1));

    assert!(!enc.can_accept());
    assert!(matches!(
        enc.encode(nv12(133)),
        Err(PlatformError::InvalidState { .. })
    ));
}

#[test]
fn refuses_what_it_cant_encode() {
    let mut enc = open(&config(PixelFormat::I420, RateControl::Cbr { kbps: 2500 }));
    // An NV12 frame for an I420 session.
    assert!(matches!(
        enc.encode(nv12(0)),
        Err(PlatformError::Unsupported { .. })
    ));
    for bad in [
        VideoEncoderConfig {
            codec: Codec::Hevc,
            ..config(PixelFormat::Nv12, RateControl::Cbr { kbps: 1 })
        },
        config(PixelFormat::Bgra8, RateControl::Cbr { kbps: 2500 }),
        VideoEncoderConfig {
            size: FrameSize::new(5, 2).expect("valid size"),
            ..config(PixelFormat::Nv12, RateControl::Cbr { kbps: 2500 })
        },
        config(PixelFormat::Nv12, RateControl::Cbr { kbps: 0 }),
        config(PixelFormat::Nv12, RateControl::Cbr { kbps: u32::MAX }),
        config(
            PixelFormat::Nv12,
            RateControl::Vbr {
                kbps: 2500,
                max_kbps: u32::MAX,
            },
        ),
        config(
            PixelFormat::Nv12,
            RateControl::Vbr {
                kbps: 2500,
                max_kbps: 2000,
            },
        ),
        VideoEncoderConfig {
            keyframe_interval: 0,
            ..config(PixelFormat::Nv12, RateControl::Cbr { kbps: 2500 })
        },
    ] {
        assert!(
            matches!(super::params(&bad), Err(PlatformError::Unsupported { .. })),
            "{bad:?}"
        );
    }
}
