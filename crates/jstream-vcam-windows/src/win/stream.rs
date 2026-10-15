//! The one video stream (VCamSample's `MediaStream`): `IMFMediaStream2`, plus
//! `IKsControl` and `IMFAttributes`. 1920x1080 at 30 fps, offered as NV12 and RGB32.
//! Each `RequestSample` builds one system-memory sample on the calling thread from
//! [`super::frames::FrameSource`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFAttributes, IMFMediaEvent,
    IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource, IMFMediaStream_Impl,
    IMFMediaStream2, IMFMediaStream2_Impl, IMFMediaType, IMFStreamDescriptor,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MEMediaSample, MEStreamStarted, MEStreamStopped,
    MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MF_DEVICESTREAM_FRAMESERVER_SHARED,
    MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID, MF_E_MEDIA_SOURCE_WRONGSTATE,
    MF_E_SHUTDOWN, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_MT_YUV_MATRIX,
    MF_STREAM_STATE, MF_STREAM_STATE_RUNNING, MF_STREAM_STATE_STOPPED, MF_VERSION,
    MFCreateMediaType, MFCreateStreamDescriptor, MFFrameSourceTypes_Color, MFGetSystemTime,
    MFMediaType_Video, MFSTARTUP_LITE, MFSampleExtension_Token, MFScheduleWorkItem, MFShutdown,
    MFStartup, MFVideoFormat_NV12, MFVideoFormat_RGB32, MFVideoInterlace_Progressive,
    MFVideoTransferMatrix_BT709,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{ComObject, GUID, HRESULT, IUnknown, IUnknownImpl, Ref, Result, implement};

use super::activate::{lock, new_attributes};
use super::attrs::impl_attributes;
use super::dll::{ObjectToken, guard, guard_quiet};
use super::frames::FrameSource;
use super::log::log;
use super::source::{ks_not_found, pv_i64};
use crate::pace::Pacer;
use crate::pixels::nv12_size;
use jstream_vcam_proto::{FPS, HEIGHT, WIDTH};

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn video_type(subtype: &GUID) -> Result<IMFMediaType> {
    let (stride, size) = if *subtype == MFVideoFormat_NV12 {
        (WIDTH, nv12_size(WIDTH as usize, HEIGHT as usize) as u32)
    } else {
        (WIDTH * 4, WIDTH * HEIGHT * 4)
    };
    // SAFETY: plain calls on a live media type with pointers to statics and locals.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(WIDTH, HEIGHT))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, size)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, size.saturating_mul(8 * FPS))?;
        if *subtype == MFVideoFormat_NV12 {
            t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        }
        Ok(t)
    }
}

/// The device-stream attributes Frame Server looks for (as VCamSample).
fn set_stream_attributes(a: &IMFAttributes) -> Result<()> {
    // SAFETY: plain calls on a live attribute store with pointers to statics.
    unsafe {
        a.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
        a.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
        a.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
        a.SetUINT32(
            &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
            MFFrameSourceTypes_Color.0 as u32,
        )
    }
}

/// Stream 0's descriptor: NV12 (current, preferred by the Camera app) and RGB32.
pub(crate) fn create_descriptor() -> Result<IMFStreamDescriptor> {
    let nv12 = video_type(&MFVideoFormat_NV12)?;
    let rgb32 = video_type(&MFVideoFormat_RGB32)?;
    // SAFETY: plain calls; the slice outlives the call.
    let sd = unsafe { MFCreateStreamDescriptor(0, &[Some(nv12.clone()), Some(rgb32)]) }?;
    // SAFETY: plain calls on live COM objects.
    unsafe { sd.GetMediaTypeHandler()?.SetCurrentMediaType(&nv12) }?;
    set_stream_attributes(&sd)?;
    Ok(sd)
}

struct StreamState {
    shutdown: bool,
    state: MF_STREAM_STATE,
    queue: IMFMediaEventQueue,
    source: Option<IMFMediaSource>,
    /// Paces `RequestSample` to the stream's frame rate.
    pacer: Pacer,
}

/// Holds a Media Foundation start-up for the stream's lifetime, so its timer work items
/// ([`MFScheduleWorkItem`]) work in any host. Start-ups are counted, so this never stops MF
/// for the host process.
struct MfStarted;

impl MfStarted {
    fn new() -> Result<Self> {
        // SAFETY: plain call; paired with MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }?;
        Ok(Self)
    }
}

impl Drop for MfStarted {
    fn drop(&mut self) {
        // SAFETY: paired with the successful MFStartup in `new`.
        let _ = unsafe { MFShutdown() };
    }
}

/// A sample request waiting for its slot: Media Foundation's timer calls it back on a
/// work-queue thread, so `RequestSample` never blocks the caller's thread.
#[implement(IMFAsyncCallback)]
struct Deliver {
    stream: ComObject<MediaStream>,
    token: Option<IUnknown>,
}

impl IMFAsyncCallback_Impl for Deliver_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> Result<()> {
        // E_NOTIMPL means the defaults: the standard work queue, no flags.
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: Ref<IMFAsyncResult>) -> Result<()> {
        guard("Stream::Deliver", || {
            self.stream.deliver(self.token.as_ref())
        })
    }
}

#[implement(IMFMediaStream2, IKsControl, IMFAttributes)]
pub(crate) struct MediaStream {
    attrs: IMFAttributes,
    descriptor: IMFStreamDescriptor,
    state: Mutex<StreamState>,
    frames: Mutex<FrameSource>,
    _mf: MfStarted,
    _token: ObjectToken,
}

impl_attributes!(MediaStream_Impl, "Stream");

impl MediaStream {
    pub(crate) fn create(
        source: IMFMediaSource,
        descriptor: IMFStreamDescriptor,
    ) -> Result<ComObject<MediaStream>> {
        let attrs = new_attributes(4)?;
        set_stream_attributes(&attrs)?;
        // SAFETY: plain call.
        let queue = unsafe { windows::Win32::Media::MediaFoundation::MFCreateEventQueue() }?;
        Ok(ComObject::new(MediaStream {
            attrs,
            descriptor,
            state: Mutex::new(StreamState {
                shutdown: false,
                state: MF_STREAM_STATE_STOPPED,
                queue,
                source: Some(source),
                pacer: Pacer::new(FPS),
            }),
            frames: Mutex::new(FrameSource::new()),
            _mf: MfStarted::new()?,
            _token: ObjectToken::new(),
        }))
    }

    pub(crate) fn attributes(&self) -> IMFAttributes {
        self.attrs.clone()
    }

    pub(crate) fn is_running(&self) -> bool {
        lock(&self.state).state == MF_STREAM_STATE_RUNNING
    }

    pub(crate) fn start(&self, now: i64) -> Result<()> {
        let mut st = lock(&self.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        st.state = MF_STREAM_STATE_RUNNING;
        st.pacer.reset();
        log("Stream: started");
        // SAFETY: the PROPVARIANT outlives the call, which copies it.
        unsafe {
            st.queue.QueueEventParamVar(
                MEStreamStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &pv_i64(now),
            )
        }
    }

    pub(crate) fn stop(&self, now: i64) -> Result<()> {
        let mut st = lock(&self.state);
        if st.shutdown || st.state == MF_STREAM_STATE_STOPPED {
            return Ok(());
        }
        st.state = MF_STREAM_STATE_STOPPED;
        log("Stream: stopped");
        // SAFETY: the PROPVARIANT outlives the call, which copies it.
        unsafe {
            st.queue.QueueEventParamVar(
                MEStreamStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &pv_i64(now),
            )
        }
    }

    /// Ends the stream and breaks the stream -> source reference cycle.
    pub(crate) fn shutdown(&self) {
        let mut st = lock(&self.state);
        st.shutdown = true;
        st.source = None;
        // SAFETY: plain call on a live COM object.
        let _ = unsafe { st.queue.Shutdown() };
        lock(&self.frames).close();
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        let st = lock(&self.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(st.queue.clone())
    }

    /// Builds one sample and queues it as `MEMediaSample` with the request's `token`. A
    /// stream that stopped or shut down since the request drops it.
    fn deliver(&self, token: Option<&IUnknown>) -> Result<()> {
        let queue = {
            let st = lock(&self.state);
            if st.shutdown || st.state != MF_STREAM_STATE_RUNNING {
                return Ok(());
            }
            st.queue.clone()
        };
        let subtype = self.current_subtype();
        let sample = lock(&self.frames).make_sample(&subtype)?;
        // SAFETY: plain calls on live COM objects with pointers to statics.
        unsafe {
            sample.SetSampleTime(MFGetSystemTime())?;
            sample.SetSampleDuration(10_000_000 / i64::from(FPS))?;
            if let Some(token) = token {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
        }
    }

    /// The subtype the client chose (NV12 unless it set RGB32).
    fn current_subtype(&self) -> GUID {
        // SAFETY: plain calls on live COM objects with a pointer to a static.
        unsafe {
            self.descriptor
                .GetMediaTypeHandler()
                .and_then(|h| h.GetCurrentMediaType())
                .and_then(|t| t.GetGUID(&MF_MT_SUBTYPE))
        }
        .unwrap_or(MFVideoFormat_NV12)
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        guard("Stream::GetEvent", || {
            let q = self.queue()?;
            // SAFETY: plain call; may block, and no lock is held.
            unsafe { q.GetEvent(flags.0) }
        })
    }

    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        guard("Stream::BeginGetEvent", || {
            let q = self.queue()?;
            // SAFETY: plain call forwarding the caller's objects.
            unsafe { q.BeginGetEvent(callback.as_ref(), state.as_ref()) }
        })
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        guard("Stream::EndGetEvent", || {
            let q = self.queue()?;
            // SAFETY: plain call forwarding the caller's object.
            unsafe { q.EndGetEvent(result.as_ref()) }
        })
    }

    fn QueueEvent(
        &self,
        met: u32,
        ext: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> Result<()> {
        guard("Stream::QueueEvent", || {
            let q = self.queue()?;
            // SAFETY: forwards the caller's pointers unchanged; same contract.
            unsafe { q.QueueEventParamVar(met, ext, status, value) }
        })
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        guard("Stream::GetMediaSource", || {
            lock(&self.state)
                .source
                .clone()
                .ok_or_else(|| MF_E_SHUTDOWN.into())
        })
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        guard("Stream::GetStreamDescriptor", || {
            self.queue()?;
            Ok(self.descriptor.clone())
        })
    }

    fn RequestSample(&self, token: Ref<IUnknown>) -> Result<()> {
        guard("Stream::RequestSample", || {
            let wait = {
                let mut st = lock(&self.state);
                if st.shutdown {
                    return Err(MF_E_SHUTDOWN.into());
                }
                if st.state != MF_STREAM_STATE_RUNNING {
                    return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());
                }
                st.pacer.reserve(Instant::now())
            };
            let token = token.as_ref().cloned();
            if wait < Duration::from_millis(1) {
                return self.deliver(token.as_ref());
            }
            // Whole milliseconds, rounded up, so a sample is never early.
            let ms = i64::try_from(wait.as_micros().div_ceil(1000)).unwrap_or(i64::MAX);
            let callback: IMFAsyncCallback = Deliver {
                stream: self.to_object(),
                token,
            }
            .into();
            // SAFETY: plain call; a negative timeout is a delay in milliseconds. The work
            // queue holds `callback`, and with it the stream, until it runs.
            unsafe { MFScheduleWorkItem(&callback, None::<&IUnknown>, -ms, None) }
        })
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        guard("Stream::SetStreamState", || {
            log(format_args!("Stream::SetStreamState {}", value.0));
            let mut st = lock(&self.state);
            if st.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            st.state = value;
            Ok(())
        })
    }

    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        guard("Stream::GetStreamState", || {
            let st = lock(&self.state);
            if st.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            Ok(st.state)
        })
    }
}

impl IKsControl_Impl for MediaStream_Impl {
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        guard_quiet("Stream::KsProperty", || Err(ks_not_found()))
    }

    fn KsMethod(
        &self,
        _m: *const KSIDENTIFIER,
        _ml: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        guard_quiet("Stream::KsMethod", || Err(ks_not_found()))
    }

    fn KsEvent(
        &self,
        _e: *const KSIDENTIFIER,
        _el: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        guard_quiet("Stream::KsEvent", || Err(ks_not_found()))
    }
}
