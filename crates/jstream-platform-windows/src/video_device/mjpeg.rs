//! The MJPEG decode hook through Media Foundation's MJPEG decoder MFT: an OS decoder, as
//! docs/adr/0031-source-types-and-decoding-scope.md asks for video codecs.

use std::mem::ManuallyDrop;

use jstream_platform::media::{ColorInfo, FrameSize, PixelFormat, VideoFrameInfo};
use jstream_platform::{
    CaptureDecoder, CaptureEncoding, CompressedFrame, PlatformError, VideoSurface,
};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFTransform, MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_LOCALMFT,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_MJPG,
};
use windows::Win32::System::Com::CoTaskMemFree;

use super::convert::Layout;
use super::format::{NV12, YUY2, color};
use super::source::{MfStarted, TypeInfo, type_info, with_bytes};
use crate::com::{OsResult, os_error};

/// Decodes MJPEG frames from a [`MfVideoDevices`](super::MfVideoDevices) stream with
/// Media Foundation's MJPEG decoder, into NV12 CPU frames.
///
/// It is the platform's [`CaptureDecoder`] for [`CaptureEncoding::Mjpeg`]: the engine
/// builds one and passes it to [`VideoDevices::open`](jstream_platform::VideoDevices::open).
/// The decoder is created on the capture thread at the first frame, and again if the
/// frame size changes. It never names the device.
pub struct MfMjpegDecoder {
    session: Option<Session>,
    /// Declared last: Media Foundation outlives the session.
    _mf: MfStarted,
}

// SAFETY: the decoder holds no COM object until its first `decode`, so the engine can
// build it on one thread and move it to the capture thread. The MFT is then created,
// used and dropped by whichever thread calls `decode` (the stream's capture thread, an
// implicit MTA member while `MfStarted` keeps the MTA alive). Enumeration asks only for
// synchronous, in-proc MFTs (`SYNCMFT | LOCALMFT`); Microsoft's in-box MJPEG decoder is
// free-threaded, so moving the decoder after its first frame is sound for it too.
// `decode` takes `&mut self`, so one thread uses it at a time; the trait asks only for
// `Send`.
unsafe impl Send for MfMjpegDecoder {}

impl std::fmt::Debug for MfMjpegDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfMjpegDecoder")
            .field("size", &self.session.as_ref().map(|s| s.size))
            .finish_non_exhaustive()
    }
}

impl MfMjpegDecoder {
    /// Starts Media Foundation. The decoder itself is created at the first frame.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when Media Foundation can't start (a Windows N edition
    /// without the Media Feature Pack).
    pub fn new() -> Result<Self, PlatformError> {
        Ok(Self {
            session: None,
            _mf: MfStarted::start()?,
        })
    }
}

impl CaptureDecoder for MfMjpegDecoder {
    fn decode(&mut self, frame: &CompressedFrame<'_>) -> Result<VideoSurface, PlatformError> {
        if frame.encoding != CaptureEncoding::Mjpeg {
            return Err(PlatformError::Unsupported {
                what: format!("{:?} frames in the MJPEG decoder", frame.encoding),
            });
        }
        if self.session.as_ref().is_none_or(|s| s.size != frame.size) {
            self.session = None;
            self.session = Some(Session::open(frame.size)?);
        }
        let Some(session) = self.session.as_mut() else {
            return Err(PlatformError::InvalidState {
                what: "MJPEG decode",
                state: "no decoder session",
            });
        };
        let result = session.decode(frame);
        if result.is_err() && session.broken {
            self.session = None;
        }
        result
    }
}

/// The decoder MFT, configured for one frame size.
struct Session {
    transform: IMFTransform,
    activate: IMFActivate,
    size: FrameSize,
    layout: Layout,
    stride: usize,
    color: ColorInfo,
    /// Whether the MFT allocates its own output samples.
    provides_samples: bool,
    /// The output buffer to allocate otherwise.
    buffer_size: u32,
    /// The output sample reused for every frame when the MFT doesn't allocate its own.
    reusable: Option<IMFSample>,
    /// Set when the MFT is in a state it may not recover from; the next frame starts a
    /// new session.
    broken: bool,
}

impl Session {
    fn open(size: FrameSize) -> Result<Self, PlatformError> {
        let (transform, activate) = create()?;
        // From here on, dropping `session` shuts the transform down.
        let mut session = Self {
            transform,
            activate,
            size,
            layout: Layout::Nv12,
            stride: 0,
            color: ColorInfo::default(),
            provides_samples: false,
            buffer_size: 0,
            reusable: None,
            broken: false,
        };
        let frame_size = (u64::from(size.width()) << 32) | u64::from(size.height());
        // SAFETY: plain COM calls on live objects.
        unsafe {
            let input = MFCreateMediaType().os("MFCreateMediaType")?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .os("IMFMediaType::SetGUID")?;
            input
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_MJPG)
                .os("IMFMediaType::SetGUID")?;
            input
                .SetUINT64(&MF_MT_FRAME_SIZE, frame_size)
                .os("IMFMediaType::SetUINT64")?;
            session
                .transform
                .SetInputType(0, &input, 0)
                .os("IMFTransform::SetInputType")?;
        }
        session.set_output()?;
        // SAFETY: plain COM calls on a configured transform.
        unsafe {
            session
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .os("IMFTransform::ProcessMessage")?;
            session
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .os("IMFTransform::ProcessMessage")?;
        }
        Ok(session)
    }

    /// Chooses the output type: NV12 when the decoder offers it, else YUY2 (converted to
    /// NV12 here). Then reads the output stream's requirements.
    fn set_output(&mut self) -> Result<(), PlatformError> {
        let mut chosen: Option<(IMFMediaType, Layout)> = None;
        for i in 0.. {
            // SAFETY: plain COM call on a transform with its input type set.
            let t = match unsafe { self.transform.GetOutputAvailableType(0, i) } {
                Ok(t) => t,
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                Err(e) => return Err(os_error("IMFTransform::GetOutputAvailableType", &e)),
            };
            // SAFETY: plain attribute read.
            let subtype = unsafe { t.GetGUID(&MF_MT_SUBTYPE) }
                .map(|g| g.to_u128())
                .unwrap_or(0);
            match subtype {
                NV12 => {
                    chosen = Some((t, Layout::Nv12));
                    break;
                }
                YUY2 if chosen.is_none() => chosen = Some((t, Layout::Yuy2)),
                _ => {}
            }
        }
        let Some((t, layout)) = chosen else {
            return Err(PlatformError::Unsupported {
                what: "an MJPEG decoder output without NV12 or YUY2".into(),
            });
        };
        // SAFETY: plain COM calls on live objects.
        let info = unsafe {
            self.transform
                .SetOutputType(0, &t, 0)
                .os("IMFTransform::SetOutputType")?;
            self.transform
                .GetOutputStreamInfo(0)
                .os("IMFTransform::GetOutputStreamInfo")?
        };
        let TypeInfo {
            matrix,
            range,
            stride,
        } = type_info(&t);
        let packed = PixelFormat::Nv12
            .packed_frame_bytes(self.size)
            .ok()
            .and_then(|n| u32::try_from(n * 2).ok())
            .unwrap_or(0);
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        self.layout = layout;
        self.stride = layout.stride(self.size.width(), stride);
        self.color = color(matrix, range, self.size.height(), true);
        self.provides_samples = info.dwFlags & provides != 0;
        self.buffer_size = info.cbSize.max(packed);
        self.reusable = None;
        Ok(())
    }

    fn decode(&mut self, frame: &CompressedFrame<'_>) -> Result<VideoSurface, PlatformError> {
        let input = sample_from(frame.data)?;
        // SAFETY: plain COM call on a streaming transform.
        let mut fed = unsafe { self.transform.ProcessInput(0, &input, 0) };
        if fed.as_ref().is_err_and(|e| e.code() == MF_E_NOTACCEPTING) {
            // Output left from an earlier frame: drop it, then feed again.
            while self.pull()?.is_some() {}
            // SAFETY: as above.
            fed = unsafe { self.transform.ProcessInput(0, &input, 0) };
        }
        fed.os("IMFTransform::ProcessInput")?;
        let info = VideoFrameInfo {
            size: self.size,
            format: PixelFormat::Nv12,
            color: self.color,
            pts: frame.pts,
        };
        match self.pull()? {
            Some(out) => {
                let (layout, stride) = (self.layout, self.stride);
                let decoded = with_bytes(&out, |bytes| layout.frame(bytes, stride, info))??;
                Ok(VideoSurface::Cpu(decoded))
            }
            None => Err(PlatformError::Os {
                call: "IMFTransform::ProcessOutput",
                code: None,
                message: "the MJPEG decoder gave no frame for this JPEG".into(),
            }),
        }
    }

    /// The next output sample, or `None` when the MFT needs more input. A stream change
    /// renegotiates the output type and tries again, a bounded number of times.
    fn pull(&mut self) -> Result<Option<IMFSample>, PlatformError> {
        const MAX_CHANGES: u32 = 2;
        for _ in 0..=MAX_CHANGES {
            let sample = if self.provides_samples {
                None
            } else {
                Some(self.output_sample()?)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: `buffers` is a valid one-element array and `status` a valid
            // out-pointer.
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            // SAFETY: each field is taken exactly once and `buffers` is not used again, so
            // the references in it are released exactly once.
            let (sample, _events) = unsafe {
                (
                    ManuallyDrop::take(&mut buffers[0].pSample),
                    ManuallyDrop::take(&mut buffers[0].pEvents),
                )
            };
            match result {
                Ok(()) => {
                    return sample.map(Some).ok_or_else(|| PlatformError::Os {
                        call: "IMFTransform::ProcessOutput",
                        code: None,
                        message: "succeeded without a sample".into(),
                    });
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    if let Err(e) = self.set_output() {
                        self.broken = true;
                        return Err(e);
                    }
                }
                Err(e) => {
                    self.broken = true;
                    return Err(os_error("IMFTransform::ProcessOutput", &e));
                }
            }
        }
        self.broken = true;
        Err(PlatformError::Os {
            call: "IMFTransform::ProcessOutput",
            code: None,
            message: "the MJPEG decoder's output format kept changing".into(),
        })
    }

    /// The reusable output sample, emptied, created on first use.
    fn output_sample(&mut self) -> Result<IMFSample, PlatformError> {
        let sample = match &self.reusable {
            Some(s) => s.clone(),
            None => {
                // SAFETY: plain constructor and method calls on live objects.
                let s = unsafe {
                    let s = MFCreateSample().os("MFCreateSample")?;
                    s.AddBuffer(
                        &MFCreateMemoryBuffer(self.buffer_size).os("MFCreateMemoryBuffer")?,
                    )
                    .os("IMFSample::AddBuffer")?;
                    s
                };
                self.reusable = Some(s.clone());
                s
            }
        };
        // SAFETY: plain COM calls on the sample's own buffer; emptying it lets the MFT
        // write from the start.
        unsafe {
            sample
                .GetBufferByIndex(0)
                .and_then(|b| b.SetCurrentLength(0))
                .os("IMFMediaBuffer::SetCurrentLength")?;
        }
        Ok(sample)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: plain COM calls on live objects; errors at teardown change nothing.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// The first synchronous decoder MFT Media Foundation ranks for MJPEG in, created.
fn create() -> Result<(IMFTransform, IMFActivate), PlatformError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_MJPG,
    };
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type info outlives the call; the out-pointers point to valid locals.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            None,
            &mut array,
            &mut count,
        )
    }
    .os("MFTEnumEx")?;
    if array.is_null() {
        return Err(no_decoder());
    }
    // SAFETY: MFTEnumEx returned `count` initialised entries at `array`. Each reference is
    // moved out exactly once, then the array itself is freed with CoTaskMemFree.
    let activates: Vec<IMFActivate> = unsafe {
        let items = std::slice::from_raw_parts_mut(array, count as usize);
        let out = items.iter_mut().filter_map(Option::take).collect();
        CoTaskMemFree(Some(array as *const _));
        out
    };
    let activate = activates.into_iter().next().ok_or_else(no_decoder)?;
    // SAFETY: plain COM call on a live activation object.
    let transform: IMFTransform =
        unsafe { activate.ActivateObject() }.os("IMFActivate::ActivateObject")?;
    Ok((transform, activate))
}

fn no_decoder() -> PlatformError {
    PlatformError::Unsupported {
        what: "MJPEG decoding (no Media Foundation MJPEG decoder is installed)".into(),
    }
}

/// A sample holding a copy of `bytes`.
fn sample_from(bytes: &[u8]) -> Result<IMFSample, PlatformError> {
    let len = u32::try_from(bytes.len()).map_err(|_| PlatformError::Unsupported {
        what: format!("a {}-byte JPEG", bytes.len()),
    })?;
    // SAFETY: plain constructor and method calls on live objects. The locked pointer is
    // valid for `len` bytes (the buffer was created with that capacity) until Unlock, and
    // nothing else holds the buffer yet.
    unsafe {
        let buffer = MFCreateMemoryBuffer(len.max(1)).os("MFCreateMemoryBuffer")?;
        let mut ptr = std::ptr::null_mut();
        buffer
            .Lock(&mut ptr, None, None)
            .os("IMFMediaBuffer::Lock")?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        buffer.Unlock().os("IMFMediaBuffer::Unlock")?;
        buffer
            .SetCurrentLength(len)
            .os("IMFMediaBuffer::SetCurrentLength")?;
        let sample = MFCreateSample().os("MFCreateSample")?;
        sample.AddBuffer(&buffer).os("IMFSample::AddBuffer")?;
        Ok(sample)
    }
}

/// Checks the GUID constants this module compares with as `u128`s.
#[cfg(test)]
mod tests {
    use windows::Win32::Media::MediaFoundation::{
        MFVideoFormat_MJPG, MFVideoFormat_NV12, MFVideoFormat_YUY2,
    };

    use super::super::format::{MJPG, NV12, YUY2};

    #[test]
    fn subtype_constants_match_the_sdk() {
        assert_eq!(MFVideoFormat_NV12.to_u128(), NV12);
        assert_eq!(MFVideoFormat_YUY2.to_u128(), YUY2);
        assert_eq!(MFVideoFormat_MJPG.to_u128(), MJPG);
    }
}
