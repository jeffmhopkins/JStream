//! The parts of the V4L2 kernel ABI (`<linux/videodev2.h>`) this crate uses, written by
//! hand: no library sits in between, the calls are `ioctl`s on the device node. The
//! structs are the 64-bit layouts (x86_64 and aarch64, ADR-0060's tiers), whose sizes
//! are part of each `ioctl` number; the test checks them against the kernel headers.

use std::ffi::c_ulong;

/// `struct v4l2_capability`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Capability {
    pub(crate) driver: [u8; 16],
    pub(crate) card: [u8; 32],
    pub(crate) bus_info: [u8; 32],
    pub(crate) version: u32,
    pub(crate) capabilities: u32,
    pub(crate) device_caps: u32,
    pub(crate) reserved: [u32; 3],
}

/// `struct v4l2_fmtdesc`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FmtDesc {
    pub(crate) index: u32,
    pub(crate) type_: u32,
    pub(crate) flags: u32,
    pub(crate) description: [u8; 32],
    pub(crate) pixelformat: u32,
    pub(crate) mbus_code: u32,
    pub(crate) reserved: [u32; 3],
}

/// `struct v4l2_frmsize_stepwise`, also the start of the `discrete` member's space.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrmSizeStepwise {
    pub(crate) min_width: u32,
    pub(crate) max_width: u32,
    pub(crate) step_width: u32,
    pub(crate) min_height: u32,
    pub(crate) max_height: u32,
    pub(crate) step_height: u32,
}

/// `struct v4l2_frmsizeenum`. The union is `stepwise`; `discrete` is its first two words
/// (`width`, `height`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrmSizeEnum {
    pub(crate) index: u32,
    pub(crate) pixel_format: u32,
    pub(crate) type_: u32,
    pub(crate) stepwise: FrmSizeStepwise,
    pub(crate) reserved: [u32; 2],
}

/// `struct v4l2_fract`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fract {
    pub(crate) numerator: u32,
    pub(crate) denominator: u32,
}

/// `struct v4l2_frmivalenum`. The union is `stepwise` (`min`, `max`, `step`); `discrete`
/// is `min`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrmIvalEnum {
    pub(crate) index: u32,
    pub(crate) pixel_format: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) type_: u32,
    pub(crate) min: Fract,
    pub(crate) max: Fract,
    pub(crate) step: Fract,
    pub(crate) reserved: [u32; 2],
}

/// `struct v4l2_pix_format`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PixFormat {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixelformat: u32,
    pub(crate) field: u32,
    pub(crate) bytesperline: u32,
    pub(crate) sizeimage: u32,
    pub(crate) colorspace: u32,
    pub(crate) priv_: u32,
    pub(crate) flags: u32,
    pub(crate) ycbcr_enc: u32,
    pub(crate) quantization: u32,
    pub(crate) xfer_func: u32,
}

/// `struct v4l2_format` for a single-planar capture: the union is 200 bytes, aligned to
/// 8 because other members hold pointers.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Format {
    pub(crate) type_: u32,
    /// The union's alignment.
    pub(crate) pad: u32,
    pub(crate) pix: PixFormat,
    pub(crate) rest: [u8; 200 - 48],
    pub(crate) align: [u64; 0],
}

/// `struct v4l2_plane_pix_format`: one plane of a multi-planar format.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PlaneFormat {
    pub(crate) sizeimage: u32,
    pub(crate) bytesperline: u32,
    pub(crate) reserved: [u16; 6],
}

/// `struct v4l2_pix_format_mplane` (packed in the header; these fields need no padding).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PixFormatMplane {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixelformat: u32,
    pub(crate) field: u32,
    pub(crate) colorspace: u32,
    pub(crate) plane_fmt: [PlaneFormat; MAX_PLANES],
    pub(crate) num_planes: u8,
    pub(crate) flags: u8,
    pub(crate) ycbcr_enc: u8,
    pub(crate) quantization: u8,
    pub(crate) xfer_func: u8,
    pub(crate) reserved: [u8; 7],
}

/// `struct v4l2_format` for a multi-planar capture: the same 208 bytes as [`Format`],
/// with the union read as `pix_mp`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FormatMplane {
    pub(crate) type_: u32,
    /// The union's alignment.
    pub(crate) pad: u32,
    pub(crate) pix_mp: PixFormatMplane,
    pub(crate) rest: [u8; 200 - 192],
    pub(crate) align: [u64; 0],
}

/// `struct v4l2_plane`: one plane of a multi-planar buffer. The `m` union is read as
/// `mem_offset` (memory-mapped buffers), in its first four bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Plane {
    pub(crate) bytesused: u32,
    pub(crate) length: u32,
    pub(crate) m: u64,
    pub(crate) data_offset: u32,
    pub(crate) reserved: [u32; 11],
}

impl Plane {
    /// The plane's offset to `mmap`, from the `m` union (see [`Buffer::offset`]).
    pub(crate) fn offset(&self) -> u32 {
        union_first_word(self.m)
    }
}

/// `VIDEO_MAX_PLANES`.
pub(crate) const MAX_PLANES: usize = 8;

/// A `__u32` at the start of a 64-bit union: its low 32 bits on a little-endian
/// machine, its high ones on a big-endian one.
fn union_first_word(m: u64) -> u32 {
    if cfg!(target_endian = "little") {
        m as u32
    } else {
        (m >> 32) as u32
    }
}

/// `struct v4l2_captureparm`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CaptureParm {
    pub(crate) capability: u32,
    pub(crate) capturemode: u32,
    pub(crate) timeperframe: Fract,
    pub(crate) extendedmode: u32,
    pub(crate) readbuffers: u32,
    pub(crate) reserved: [u32; 4],
}

/// `struct v4l2_streamparm` for a capture: the union is 200 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamParm {
    pub(crate) type_: u32,
    pub(crate) capture: CaptureParm,
    pub(crate) rest: [u8; 200 - 40],
}

/// `struct v4l2_requestbuffers`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RequestBuffers {
    pub(crate) count: u32,
    pub(crate) type_: u32,
    pub(crate) memory: u32,
    pub(crate) capabilities: u32,
    pub(crate) flags: u8,
    pub(crate) reserved: [u8; 3],
}

/// `struct timeval` on a 64-bit kernel.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Timeval {
    pub(crate) tv_sec: i64,
    pub(crate) tv_usec: i64,
}

/// `struct v4l2_timecode`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Timecode {
    pub(crate) type_: u32,
    pub(crate) flags: u32,
    pub(crate) frames: u8,
    pub(crate) seconds: u8,
    pub(crate) minutes: u8,
    pub(crate) hours: u8,
    pub(crate) userbits: [u8; 4],
}

/// `struct v4l2_buffer`. The `m` union is read as `offset` (memory-mapped buffers), in
/// its first four bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Buffer {
    pub(crate) index: u32,
    pub(crate) type_: u32,
    pub(crate) bytesused: u32,
    pub(crate) flags: u32,
    pub(crate) field: u32,
    pub(crate) timestamp: Timeval,
    pub(crate) timecode: Timecode,
    pub(crate) sequence: u32,
    pub(crate) memory: u32,
    pub(crate) m: u64,
    pub(crate) length: u32,
    pub(crate) reserved2: u32,
    pub(crate) request_fd: i32,
}

impl Buffer {
    /// A memory-mapped capture buffer of type `type_`, `index` or not yet known.
    pub(crate) fn mmap(type_: u32, index: u32) -> Self {
        Self {
            index,
            type_,
            memory: MEMORY_MMAP,
            ..Self::default()
        }
    }

    /// The buffer's offset to `mmap`, from the `m` union: `m.offset` is a `__u32` at the
    /// start of the union, whose other members are wider.
    pub(crate) fn offset(&self) -> u32 {
        union_first_word(self.m)
    }
}

/// `struct v4l2_exportbuffer`: one plane of a buffer, exported as a DMA-BUF
/// (`VIDIOC_EXPBUF`). The driver writes the new descriptor into `fd`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExportBuffer {
    pub(crate) type_: u32,
    pub(crate) index: u32,
    pub(crate) plane: u32,
    pub(crate) flags: u32,
    pub(crate) fd: i32,
    pub(crate) reserved: [u32; 11],
}

// The 64-bit layouts. A 32-bit target would need its own (`timeval` and the unions
// shrink, and with them the ioctl numbers), so the backend isn't built there.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<Capability>() == 104);
    assert!(size_of::<FmtDesc>() == 64);
    assert!(size_of::<FrmSizeEnum>() == 44);
    assert!(size_of::<FrmIvalEnum>() == 52);
    assert!(size_of::<Format>() == 208);
    assert!(size_of::<FormatMplane>() == 208);
    assert!(size_of::<PixFormatMplane>() == 192);
    assert!(size_of::<PlaneFormat>() == 20);
    assert!(size_of::<Plane>() == 64);
    assert!(size_of::<StreamParm>() == 204);
    assert!(size_of::<RequestBuffers>() == 20);
    assert!(size_of::<Buffer>() == 88);
    assert!(size_of::<ExportBuffer>() == 64);
};

/// `_IOC(dir, 'V', nr, size)`.
const fn ioc(dir: c_ulong, nr: c_ulong, size: usize) -> c_ulong {
    (dir << 30) | ((size as c_ulong) << 16) | ((b'V' as c_ulong) << 8) | nr
}
const READ: c_ulong = 2;
const WRITE: c_ulong = 1;

pub(crate) const VIDIOC_QUERYCAP: c_ulong = ioc(READ, 0, size_of::<Capability>());
pub(crate) const VIDIOC_ENUM_FMT: c_ulong = ioc(READ | WRITE, 2, size_of::<FmtDesc>());
pub(crate) const VIDIOC_G_FMT: c_ulong = ioc(READ | WRITE, 4, size_of::<Format>());
pub(crate) const VIDIOC_S_FMT: c_ulong = ioc(READ | WRITE, 5, size_of::<Format>());
pub(crate) const VIDIOC_REQBUFS: c_ulong = ioc(READ | WRITE, 8, size_of::<RequestBuffers>());
pub(crate) const VIDIOC_QUERYBUF: c_ulong = ioc(READ | WRITE, 9, size_of::<Buffer>());
pub(crate) const VIDIOC_QBUF: c_ulong = ioc(READ | WRITE, 15, size_of::<Buffer>());
pub(crate) const VIDIOC_EXPBUF: c_ulong = ioc(READ | WRITE, 16, size_of::<ExportBuffer>());
pub(crate) const VIDIOC_DQBUF: c_ulong = ioc(READ | WRITE, 17, size_of::<Buffer>());
pub(crate) const VIDIOC_STREAMON: c_ulong = ioc(WRITE, 18, size_of::<i32>());
pub(crate) const VIDIOC_STREAMOFF: c_ulong = ioc(WRITE, 19, size_of::<i32>());
pub(crate) const VIDIOC_G_PARM: c_ulong = ioc(READ | WRITE, 21, size_of::<StreamParm>());
pub(crate) const VIDIOC_S_PARM: c_ulong = ioc(READ | WRITE, 22, size_of::<StreamParm>());
pub(crate) const VIDIOC_ENUM_FRAMESIZES: c_ulong = ioc(READ | WRITE, 74, size_of::<FrmSizeEnum>());
pub(crate) const VIDIOC_ENUM_FRAMEINTERVALS: c_ulong =
    ioc(READ | WRITE, 75, size_of::<FrmIvalEnum>());

/// `V4L2_CAP_VIDEO_CAPTURE`: single-planar capture.
pub(crate) const CAP_VIDEO_CAPTURE: u32 = 0x1;
/// `V4L2_CAP_VIDEO_CAPTURE_MPLANE`: multi-planar capture.
pub(crate) const CAP_VIDEO_CAPTURE_MPLANE: u32 = 0x1000;
/// `V4L2_CAP_STREAMING`: streaming I/O (`mmap`).
pub(crate) const CAP_STREAMING: u32 = 0x0400_0000;
/// `V4L2_CAP_DEVICE_CAPS`: `device_caps` describes this node.
pub(crate) const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
/// `V4L2_CAP_TIMEPERFRAME` in `v4l2_captureparm::capability`.
pub(crate) const CAP_TIMEPERFRAME: u32 = 0x1000;

/// `V4L2_BUF_TYPE_VIDEO_CAPTURE`.
pub(crate) const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
/// `V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE`.
pub(crate) const BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
/// `V4L2_MEMORY_MMAP`.
pub(crate) const MEMORY_MMAP: u32 = 1;
/// `V4L2_FIELD_NONE`: progressive frames.
pub(crate) const FIELD_NONE: u32 = 1;

/// `V4L2_FMT_FLAG_EMULATED`: a format libv4l converts to; the kernel never sets it.
pub(crate) const FMT_FLAG_EMULATED: u32 = 0x2;
/// `V4L2_FRMSIZE_TYPE_DISCRETE`.
pub(crate) const FRMSIZE_TYPE_DISCRETE: u32 = 1;
/// `V4L2_FRMIVAL_TYPE_DISCRETE`.
pub(crate) const FRMIVAL_TYPE_DISCRETE: u32 = 1;

/// `V4L2_BUF_FLAG_ERROR`: the frame may be corrupt.
pub(crate) const BUF_FLAG_ERROR: u32 = 0x40;
/// `V4L2_BUF_FLAG_TIMESTAMP_MASK`.
pub(crate) const BUF_FLAG_TIMESTAMP_MASK: u32 = 0xe000;
/// `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC`: the timestamp is `CLOCK_MONOTONIC`.
pub(crate) const BUF_FLAG_TIMESTAMP_MONOTONIC: u32 = 0x2000;

/// A FourCC as V4L2 packs it, `v4l2_fourcc(a, b, c, d)`.
pub(crate) const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}
pub(crate) const PIX_FMT_YUYV: u32 = fourcc(b"YUYV");
pub(crate) const PIX_FMT_MJPEG: u32 = fourcc(b"MJPG");
/// `V4L2_PIX_FMT_JPEG`: what some webcams call their MJPEG.
pub(crate) const PIX_FMT_JPEG: u32 = fourcc(b"JPEG");
pub(crate) const PIX_FMT_NV12: u32 = fourcc(b"NV12");
/// `V4L2_PIX_FMT_YUV420`: I420.
pub(crate) const PIX_FMT_YUV420: u32 = fourcc(b"YU12");
/// `V4L2_PIX_FMT_NV12M`: NV12 with each plane in its own buffer plane.
pub(crate) const PIX_FMT_NV12M: u32 = fourcc(b"NM12");
/// `V4L2_PIX_FMT_YUV420M`: I420 with each plane in its own buffer plane.
pub(crate) const PIX_FMT_YUV420M: u32 = fourcc(b"YM12");
/// `V4L2_PIX_FMT_ABGR32` and `_XBGR32`: B, G, R and A (or padding) in memory, DRM's
/// `ARGB8888` and `XRGB8888`.
pub(crate) const PIX_FMT_ABGR32: u32 = fourcc(b"AR24");
pub(crate) const PIX_FMT_XBGR32: u32 = fourcc(b"XR24");
/// `V4L2_PIX_FMT_RGBA32` and `_RGBX32`: R, G, B and A (or padding) in memory, DRM's
/// `ABGR8888` and `XBGR8888`.
pub(crate) const PIX_FMT_RGBA32: u32 = fourcc(b"AB24");
pub(crate) const PIX_FMT_RGBX32: u32 = fourcc(b"XB24");

/// `V4L2_COLORSPACE_SMPTE170M`, `_REC709`, `_JPEG`, `_SRGB` and `_BT2020`.
pub(crate) const COLORSPACE_SMPTE170M: u32 = 1;
pub(crate) const COLORSPACE_REC709: u32 = 3;
pub(crate) const COLORSPACE_JPEG: u32 = 7;
pub(crate) const COLORSPACE_SRGB: u32 = 8;
pub(crate) const COLORSPACE_BT2020: u32 = 10;
/// `V4L2_YCBCR_ENC_601`, `_709` and `_BT2020`.
pub(crate) const YCBCR_ENC_601: u32 = 1;
pub(crate) const YCBCR_ENC_709: u32 = 2;
pub(crate) const YCBCR_ENC_BT2020: u32 = 6;
/// `V4L2_QUANTIZATION_FULL_RANGE` and `_LIM_RANGE`.
pub(crate) const QUANTIZATION_FULL_RANGE: u32 = 1;
pub(crate) const QUANTIZATION_LIM_RANGE: u32 = 2;

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    /// The numbers and offsets `<linux/videodev2.h>` gives on x86_64 (checked with a C
    /// program against the kernel headers).
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_and_numbers_match_the_kernel_headers() {
        assert_eq!(offset_of!(Capability, capabilities), 84);
        assert_eq!(offset_of!(Capability, device_caps), 88);
        assert_eq!(offset_of!(FmtDesc, pixelformat), 44);
        assert_eq!(offset_of!(FrmSizeEnum, stepwise), 12);
        assert_eq!(offset_of!(FrmIvalEnum, type_), 16);
        assert_eq!(offset_of!(FrmIvalEnum, min), 20);
        assert_eq!(offset_of!(Format, pix), 8);
        assert_eq!(offset_of!(PixFormat, bytesperline), 16);
        assert_eq!(offset_of!(PixFormat, colorspace), 24);
        assert_eq!(offset_of!(PixFormat, ycbcr_enc), 36);
        assert_eq!(offset_of!(PixFormat, quantization), 40);
        assert_eq!(offset_of!(StreamParm, capture), 4);
        assert_eq!(offset_of!(CaptureParm, timeperframe), 8);
        assert_eq!(offset_of!(Buffer, bytesused), 8);
        assert_eq!(offset_of!(Buffer, flags), 12);
        assert_eq!(offset_of!(Buffer, timestamp), 24);
        assert_eq!(offset_of!(Buffer, timecode), 40);
        assert_eq!(offset_of!(Buffer, sequence), 56);
        assert_eq!(offset_of!(Buffer, memory), 60);
        assert_eq!(offset_of!(Buffer, m), 64);
        assert_eq!(offset_of!(Buffer, length), 72);
        assert_eq!(offset_of!(PlaneFormat, bytesperline), 4);
        assert_eq!(offset_of!(PixFormatMplane, colorspace), 16);
        assert_eq!(offset_of!(PixFormatMplane, plane_fmt), 20);
        assert_eq!(offset_of!(PixFormatMplane, num_planes), 180);
        assert_eq!(offset_of!(PixFormatMplane, ycbcr_enc), 182);
        assert_eq!(offset_of!(PixFormatMplane, quantization), 183);
        assert_eq!(offset_of!(PixFormatMplane, xfer_func), 184);
        assert_eq!(offset_of!(FormatMplane, pix_mp), 8);
        assert_eq!(offset_of!(Plane, length), 4);
        assert_eq!(offset_of!(Plane, m), 8);
        assert_eq!(offset_of!(Plane, data_offset), 16);
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_ENUM_FMT, 0xc040_5602);
        assert_eq!(VIDIOC_ENUM_FRAMESIZES, 0xc02c_564a);
        assert_eq!(VIDIOC_ENUM_FRAMEINTERVALS, 0xc034_564b);
        assert_eq!(VIDIOC_G_FMT, 0xc0d0_5604);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_G_PARM, 0xc0cc_5615);
        assert_eq!(VIDIOC_S_PARM, 0xc0cc_5616);
        assert_eq!(CAP_TIMEPERFRAME, 0x1000);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xc058_5609);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_EXPBUF, 0xc040_5610);
        assert_eq!(offset_of!(ExportBuffer, plane), 8);
        assert_eq!(offset_of!(ExportBuffer, flags), 12);
        assert_eq!(offset_of!(ExportBuffer, fd), 16);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
    }

    /// The flag and type values `<linux/videodev2.h>` gives.
    #[test]
    fn constants_match_the_kernel_headers() {
        assert_eq!(
            (FIELD_NONE, FMT_FLAG_EMULATED, BUF_FLAG_ERROR),
            (1, 0x2, 0x40)
        );
        assert_eq!((FRMSIZE_TYPE_DISCRETE, FRMIVAL_TYPE_DISCRETE), (1, 1));
        assert_eq!(
            (CAP_VIDEO_CAPTURE, CAP_STREAMING, CAP_DEVICE_CAPS),
            (0x1, 0x0400_0000, 0x8000_0000)
        );
        assert_eq!(
            (BUF_FLAG_TIMESTAMP_MASK, BUF_FLAG_TIMESTAMP_MONOTONIC),
            (0xe000, 0x2000)
        );
        assert_eq!(
            (
                CAP_VIDEO_CAPTURE_MPLANE,
                BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                MAX_PLANES
            ),
            (0x1000, 9, 8)
        );
    }

    #[test]
    fn fourccs_pack_little_end_first() {
        assert_eq!(PIX_FMT_YUYV, 0x5659_5559);
        assert_eq!(PIX_FMT_MJPEG, 0x4750_4a4d);
        assert_eq!(PIX_FMT_JPEG, 0x4745_504a);
        assert_eq!(PIX_FMT_NV12, 0x3231_564e);
        assert_eq!(PIX_FMT_YUV420, 0x3231_5559);
        assert_eq!(PIX_FMT_NV12M, 0x3231_4d4e);
        assert_eq!(PIX_FMT_YUV420M, 0x3231_4d59);
        assert_eq!(PIX_FMT_ABGR32, 0x3432_5241);
        assert_eq!(PIX_FMT_XBGR32, 0x3432_5258);
        assert_eq!(PIX_FMT_RGBA32, 0x3432_4241);
        assert_eq!(PIX_FMT_RGBX32, 0x3432_4258);
    }

    #[test]
    fn a_buffers_mmap_offset_is_the_unions_first_word() {
        let mut b = Buffer::mmap(BUF_TYPE_VIDEO_CAPTURE, 2);
        b.m = u64::from_ne_bytes([0x10, 0x20, 0, 0, 0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(b.offset(), u32::from_ne_bytes([0x10, 0x20, 0, 0]));
        assert_eq!((b.index, b.type_, b.memory), (2, 1, 1));
        let p = Plane {
            m: b.m,
            ..Plane::default()
        };
        assert_eq!(p.offset(), b.offset());
    }
}
