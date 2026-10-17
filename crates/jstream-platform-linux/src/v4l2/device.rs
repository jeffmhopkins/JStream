//! A V4L2 device node, its `ioctl`s and its memory-mapped buffers, behind safe RAII
//! types (R-UNSAFE-003): [`Device`] owns the file descriptor, [`Streaming`] the mapped
//! buffers and the stream it turned on, and both undo what they did when dropped.

use std::ffi::{CString, c_ulong, c_void};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;

use jstream_platform::PlatformError;
use jstream_platform::media::Timebase;

use super::abi::{
    BUF_TYPE_VIDEO_CAPTURE, Buffer, CAP_TIMEPERFRAME, Capability, FIELD_NONE, FMT_FLAG_EMULATED,
    FRMIVAL_TYPE_DISCRETE, FRMSIZE_TYPE_DISCRETE, FmtDesc, Format, Fract, FrmIvalEnum, FrmSizeEnum,
    MEMORY_MMAP, PixFormat, RequestBuffers, StreamParm, VIDIOC_DQBUF, VIDIOC_ENUM_FMT,
    VIDIOC_ENUM_FRAMEINTERVALS, VIDIOC_ENUM_FRAMESIZES, VIDIOC_G_PARM, VIDIOC_QBUF,
    VIDIOC_QUERYBUF, VIDIOC_QUERYCAP, VIDIOC_REQBUFS, VIDIOC_S_FMT, VIDIOC_S_PARM,
    VIDIOC_STREAMOFF, VIDIOC_STREAMON,
};
use super::format::{Enumerated, encoding, stepwise_sizes};

/// Buffers asked for: enough that the driver always has one to fill while the capture
/// thread converts another.
const BUFFERS: u32 = 4;

/// A failed call as a [`PlatformError`]: a device that went away is lost, one another
/// application holds is busy, and anything else is the OS refusing.
pub(crate) fn os_error(call: &'static str, what: &str, e: &io::Error) -> PlatformError {
    match e.raw_os_error() {
        Some(libc::ENODEV | libc::ENXIO) => PlatformError::DeviceLost {
            what: what.to_owned(),
            reason: "it was unplugged".into(),
        },
        Some(libc::EBUSY) => PlatformError::Busy {
            what: "the video capture device",
        },
        code => PlatformError::Os {
            call,
            code: code.and_then(|c| u32::try_from(c).ok()),
            message: format!("{what}: {e}"),
        },
    }
}

/// An open video device node.
#[derive(Debug)]
pub(crate) struct Device {
    fd: OwnedFd,
    /// What the device is, for errors.
    pub(crate) what: String,
}

impl Device {
    /// Opens `path` for reading frames, non-blocking.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when there is no such node, and [`PlatformError::Os`]
    /// when it can't be opened (most often permissions: the user isn't in the `video`
    /// group and the session doesn't grant the device).
    pub(crate) fn open(path: &str) -> Result<Self, PlatformError> {
        let what = format!("video capture device {path:?}");
        let c_path = CString::new(path).map_err(|_| PlatformError::NotFound {
            kind: "video capture device",
            id: path.to_owned(),
        })?;
        // SAFETY: a NUL-terminated path and plain flags; the returned descriptor, if
        // any, is owned below.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let e = io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::ENOENT | libc::ENODEV | libc::ENXIO) => PlatformError::NotFound {
                    kind: "video capture device",
                    id: path.to_owned(),
                },
                _ => os_error("open", &what, &e),
            });
        }
        // SAFETY: `fd` is a descriptor this call just opened and nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self { fd, what })
    }

    pub(crate) fn raw(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// `ioctl(fd, request, arg)`, retried when a signal interrupts it.
    ///
    /// # Safety
    /// `request` is a V4L2 request whose argument is a `T` (its size is in the number).
    unsafe fn ioctl<T>(&self, request: c_ulong, arg: &mut T) -> io::Result<()> {
        #[allow(
            clippy::unnecessary_cast,
            reason = "`libc::Ioctl` is `c_ulong` on glibc and `c_int` on musl"
        )]
        let request = request as libc::Ioctl;
        loop {
            // SAFETY: the function's contract: `arg` is a valid, writable `T` for the
            // call, and the request reads and writes only that.
            let rc = unsafe {
                libc::ioctl(
                    self.fd.as_raw_fd(),
                    request,
                    std::ptr::from_mut(arg).cast::<c_void>(),
                )
            };
            if rc >= 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// `VIDIOC_QUERYCAP`.
    pub(crate) fn capability(&self) -> Result<Capability, PlatformError> {
        // SAFETY: all-zero is a valid `v4l2_capability`.
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        // SAFETY: `VIDIOC_QUERYCAP` takes a `v4l2_capability`.
        unsafe { self.ioctl(VIDIOC_QUERYCAP, &mut cap) }
            .map_err(|e| os_error("VIDIOC_QUERYCAP", &self.what, &e))?;
        Ok(cap)
    }

    /// The capture pixel formats the driver itself offers (not libv4l's emulated ones),
    /// in its order.
    pub(crate) fn pixel_formats(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for index in 0.. {
            // SAFETY: all-zero is a valid `v4l2_fmtdesc`.
            let mut desc: FmtDesc = unsafe { std::mem::zeroed() };
            desc.index = index;
            desc.type_ = BUF_TYPE_VIDEO_CAPTURE;
            // SAFETY: `VIDIOC_ENUM_FMT` takes a `v4l2_fmtdesc`.
            if unsafe { self.ioctl(VIDIOC_ENUM_FMT, &mut desc) }.is_err() {
                break;
            }
            if desc.flags & FMT_FLAG_EMULATED == 0 && !out.contains(&desc.pixelformat) {
                out.push(desc.pixelformat);
            }
        }
        out
    }

    /// The sizes of `fourcc`: each discrete one, or a stepwise range's useful ones.
    fn sizes(&self, fourcc: u32) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for index in 0.. {
            // SAFETY: all-zero is a valid `v4l2_frmsizeenum`.
            let mut e: FrmSizeEnum = unsafe { std::mem::zeroed() };
            e.index = index;
            e.pixel_format = fourcc;
            // SAFETY: `VIDIOC_ENUM_FRAMESIZES` takes a `v4l2_frmsizeenum`.
            if unsafe { self.ioctl(VIDIOC_ENUM_FRAMESIZES, &mut e) }.is_err() {
                break;
            }
            if e.type_ == FRMSIZE_TYPE_DISCRETE {
                // `discrete` (`width`, `height`) is the union's first two words, which
                // the stepwise view calls `min_width` and `max_width`.
                out.push((e.stepwise.min_width, e.stepwise.max_width));
            } else {
                // Stepwise and continuous ranges have one entry.
                out.extend(stepwise_sizes(&e.stepwise));
                break;
            }
        }
        out
    }

    /// The frame intervals of `fourcc` at `width` x `height`: each discrete one, or a
    /// range's shortest. A driver that doesn't enumerate them gets its current one.
    fn intervals(&self, fourcc: u32, width: u32, height: u32) -> Vec<Fract> {
        let mut out = Vec::new();
        for index in 0.. {
            // SAFETY: all-zero is a valid `v4l2_frmivalenum`.
            let mut e: FrmIvalEnum = unsafe { std::mem::zeroed() };
            e.index = index;
            e.pixel_format = fourcc;
            e.width = width;
            e.height = height;
            // SAFETY: `VIDIOC_ENUM_FRAMEINTERVALS` takes a `v4l2_frmivalenum`.
            if unsafe { self.ioctl(VIDIOC_ENUM_FRAMEINTERVALS, &mut e) }.is_err() {
                break;
            }
            out.push(e.min);
            if e.type_ != FRMIVAL_TYPE_DISCRETE {
                break;
            }
        }
        if out.is_empty()
            && let Some(current) = self.current_interval()
        {
            out.push(current);
        }
        out
    }

    /// `VIDIOC_G_PARM`'s frame interval, when the driver reports one.
    fn current_interval(&self) -> Option<Fract> {
        // SAFETY: all-zero is a valid `v4l2_streamparm`.
        let mut parm: StreamParm = unsafe { std::mem::zeroed() };
        parm.type_ = BUF_TYPE_VIDEO_CAPTURE;
        // SAFETY: `VIDIOC_G_PARM` takes a `v4l2_streamparm`.
        unsafe { self.ioctl(VIDIOC_G_PARM, &mut parm) }.ok()?;
        let t = parm.capture.timeperframe;
        (t.numerator != 0 && t.denominator != 0).then_some(t)
    }

    /// Every size and interval of each pixel format the backend delivers.
    pub(crate) fn enumerate(&self) -> Vec<Enumerated> {
        self.pixel_formats()
            .into_iter()
            .filter(|&f| encoding(f).is_some())
            .map(|fourcc| Enumerated {
                fourcc,
                sizes: self
                    .sizes(fourcc)
                    .into_iter()
                    .map(|(w, h)| ((w, h), self.intervals(fourcc, w, h)))
                    .collect(),
            })
            .collect()
    }

    /// `VIDIOC_S_FMT` for progressive `fourcc` frames of `width` x `height`. Returns what
    /// the driver settled on, which may differ.
    pub(crate) fn set_format(
        &self,
        fourcc: u32,
        width: u32,
        height: u32,
    ) -> Result<PixFormat, PlatformError> {
        // SAFETY: all-zero is a valid `v4l2_format`.
        let mut f: Format = unsafe { std::mem::zeroed() };
        f.type_ = BUF_TYPE_VIDEO_CAPTURE;
        f.pix = PixFormat {
            width,
            height,
            pixelformat: fourcc,
            field: FIELD_NONE,
            ..PixFormat::default()
        };
        // SAFETY: `VIDIOC_S_FMT` takes a `v4l2_format`.
        unsafe { self.ioctl(VIDIOC_S_FMT, &mut f) }
            .map_err(|e| os_error("VIDIOC_S_FMT", &self.what, &e))?;
        Ok(f.pix)
    }

    /// `VIDIOC_S_PARM` with `period` as the frame interval, when the driver takes one.
    /// Returns the interval it settled on, or `None` when it has no say over the rate.
    pub(crate) fn set_period(&self, period: Timebase) -> Result<Option<Fract>, PlatformError> {
        // SAFETY: all-zero is a valid `v4l2_streamparm`.
        let mut parm: StreamParm = unsafe { std::mem::zeroed() };
        parm.type_ = BUF_TYPE_VIDEO_CAPTURE;
        // SAFETY: `VIDIOC_G_PARM` takes a `v4l2_streamparm`.
        if unsafe { self.ioctl(VIDIOC_G_PARM, &mut parm) }.is_err()
            || parm.capture.capability & CAP_TIMEPERFRAME == 0
        {
            return Ok(None);
        }
        parm.capture.timeperframe = Fract {
            numerator: period.num(),
            denominator: period.den(),
        };
        // SAFETY: `VIDIOC_S_PARM` takes a `v4l2_streamparm`.
        unsafe { self.ioctl(VIDIOC_S_PARM, &mut parm) }
            .map_err(|e| os_error("VIDIOC_S_PARM", &self.what, &e))?;
        Ok(Some(parm.capture.timeperframe))
    }

    /// Requests and maps the capture buffers, queues them, and turns the stream on.
    ///
    /// # Errors
    /// [`PlatformError::Busy`] when another application is streaming from the device,
    /// and [`PlatformError::Os`] when the driver refuses a step.
    pub(crate) fn stream(self) -> Result<Streaming, PlatformError> {
        let mut req = RequestBuffers {
            count: BUFFERS,
            type_: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..RequestBuffers::default()
        };
        // SAFETY: `VIDIOC_REQBUFS` takes a `v4l2_requestbuffers`.
        unsafe { self.ioctl(VIDIOC_REQBUFS, &mut req) }
            .map_err(|e| os_error("VIDIOC_REQBUFS", &self.what, &e))?;
        if req.count < 2 {
            return Err(PlatformError::Os {
                call: "VIDIOC_REQBUFS",
                code: None,
                message: format!("{}: the driver gave {} buffers", self.what, req.count),
            });
        }
        let mut streaming = Streaming {
            device: self,
            buffers: Vec::new(),
            on: false,
        };
        for index in 0..req.count {
            let mut b = Buffer::mmap(index);
            let device = &streaming.device;
            // SAFETY: `VIDIOC_QUERYBUF` takes a `v4l2_buffer`.
            unsafe { device.ioctl(VIDIOC_QUERYBUF, &mut b) }
                .map_err(|e| os_error("VIDIOC_QUERYBUF", &device.what, &e))?;
            let mapped = Mapped::new(device, b.offset(), b.length)?;
            streaming.buffers.push(mapped);
            streaming.queue(index)?;
        }
        let mut ty = BUF_TYPE_VIDEO_CAPTURE as i32;
        // SAFETY: `VIDIOC_STREAMON` takes the buffer type as an `int`.
        unsafe { streaming.device.ioctl(VIDIOC_STREAMON, &mut ty) }
            .map_err(|e| os_error("VIDIOC_STREAMON", &streaming.device.what, &e))?;
        streaming.on = true;
        Ok(streaming)
    }
}

/// One memory-mapped buffer, unmapped on drop.
#[derive(Debug)]
struct Mapped {
    ptr: NonNull<c_void>,
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this value; only the capture thread
// reads it, between dequeuing and queuing the buffer back.
unsafe impl Send for Mapped {}

impl Mapped {
    fn new(device: &Device, offset: u32, len: u32) -> Result<Self, PlatformError> {
        let len = len as usize;
        // SAFETY: maps `len` bytes of the device's buffer at the offset the driver gave,
        // read-only and shared; the mapping is owned by the returned value.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                device.raw(),
                libc::off_t::from(offset),
            )
        };
        if ptr == libc::MAP_FAILED || len == 0 {
            return Err(os_error("mmap", &device.what, &io::Error::last_os_error()));
        }
        let ptr = NonNull::new(ptr).ok_or_else(|| PlatformError::Os {
            call: "mmap",
            code: None,
            message: format!("{}: a null mapping", device.what),
        })?;
        Ok(Self { ptr, len })
    }

    /// The first `used` bytes (all of them if `used` is larger).
    fn bytes(&self, used: usize) -> &[u8] {
        // SAFETY: the mapping is `len` readable bytes for as long as `self` lives.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().cast::<u8>(), used.min(self.len)) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: a mapping this value made, unmapped once.
        unsafe { libc::munmap(self.ptr.as_ptr(), self.len) };
    }
}

/// A device that is streaming into its mapped buffers. Dropping it turns the stream off,
/// unmaps the buffers and closes the device.
#[derive(Debug)]
pub(crate) struct Streaming {
    /// Dropped last: the buffers are unmapped before the descriptor closes.
    buffers: Vec<Mapped>,
    device: Device,
    on: bool,
}

/// A dequeued buffer: its bytes until it is queued back.
pub(crate) struct Filled<'a> {
    pub(crate) index: u32,
    pub(crate) flags: u32,
    pub(crate) timestamp: super::abi::Timeval,
    pub(crate) bytes: &'a [u8],
}

impl Streaming {
    pub(crate) fn raw(&self) -> i32 {
        self.device.raw()
    }

    pub(crate) fn what(&self) -> &str {
        &self.device.what
    }

    /// Hands buffer `index` back to the driver.
    pub(crate) fn queue(&self, index: u32) -> Result<(), PlatformError> {
        let mut b = Buffer::mmap(index);
        // SAFETY: `VIDIOC_QBUF` takes a `v4l2_buffer`.
        unsafe { self.device.ioctl(VIDIOC_QBUF, &mut b) }
            .map_err(|e| os_error("VIDIOC_QBUF", &self.device.what, &e))
    }

    /// The next filled buffer, or `None` when none is ready yet.
    pub(crate) fn dequeue(&self) -> Result<Option<Filled<'_>>, PlatformError> {
        let mut b = Buffer::mmap(0);
        // SAFETY: `VIDIOC_DQBUF` takes a `v4l2_buffer`.
        match unsafe { self.device.ioctl(VIDIOC_DQBUF, &mut b) } {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(os_error("VIDIOC_DQBUF", &self.device.what, &e)),
        }
        let mapped = self
            .buffers
            .get(b.index as usize)
            .ok_or_else(|| PlatformError::Os {
                call: "VIDIOC_DQBUF",
                code: None,
                message: format!(
                    "{}: the driver returned buffer {}",
                    self.device.what, b.index
                ),
            })?;
        Ok(Some(Filled {
            index: b.index,
            flags: b.flags,
            timestamp: b.timestamp,
            bytes: mapped.bytes(b.bytesused as usize),
        }))
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        if self.on {
            let mut ty = BUF_TYPE_VIDEO_CAPTURE as i32;
            // SAFETY: `VIDIOC_STREAMOFF` takes the buffer type as an `int`. It dequeues
            // every buffer, so none is in the driver's hands when they are unmapped.
            let _ = unsafe { self.device.ioctl(VIDIOC_STREAMOFF, &mut ty) };
        }
    }
}
