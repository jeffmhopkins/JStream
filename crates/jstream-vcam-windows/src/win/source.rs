//! The media source (VCamSample's `MediaSource`): `IMFMediaSourceEx` with one video
//! stream, plus `IMFGetService`, `IKsControl` and `IMFAttributes`, which Frame Server
//! queries for.
//!
//! Lock order: the source's state, then the stream's. Never hold a lock across a blocking
//! event-queue call (`GetEvent`).

use std::mem::ManuallyDrop;
use std::sync::Mutex;

use windows::Win32::Foundation::{ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSCAMERAPROFILE_HighFrameRate, KSCAMERAPROFILE_Legacy,
    KSIDENTIFIER,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFGetService, IMFGetService_Impl,
    IMFMediaEvent, IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource,
    IMFMediaSource_Impl, IMFMediaSourceEx, IMFMediaSourceEx_Impl, IMFPresentationDescriptor,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MENewStream, MESourceStarted, MESourceStopped,
    MEUpdatedStream, MF_DEVICEMFT_SENSORPROFILE_COLLECTION, MF_E_INVALID_STATE_TRANSITION,
    MF_E_INVALIDSTREAMNUMBER, MF_E_SHUTDOWN, MF_E_UNSUPPORTED_SERVICE,
    MF_E_UNSUPPORTED_TIME_FORMAT, MFCreateEventQueue, MFCreatePresentationDescriptor,
    MFCreateSensorProfile, MFCreateSensorProfileCollection, MFGetSystemTime, MFMEDIASOURCE_IS_LIVE,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Variant::VT_I8;
use windows_core::{BOOL, ComObject, GUID, HRESULT, IUnknown, PCWSTR, Ref, Result, implement};

use super::activate::{lock, new_attributes};
use super::attrs::impl_attributes;
use super::dll::{ObjectToken, guard};
use super::log::log;
use super::stream::{self, MediaStream};

/// A `VT_I8` PROPVARIANT (no destructor needed).
pub(crate) fn pv_i64(v: i64) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: v },
            }),
        },
    }
}

/// The error every `IKsControl` method returns: no property sets are supported.
pub(crate) fn ks_not_found() -> windows_core::Error {
    HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into()
}

struct SourceState {
    shutdown: bool,
    queue: IMFMediaEventQueue,
    pd: IMFPresentationDescriptor,
    stream: Option<ComObject<MediaStream>>,
}

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl, IMFAttributes)]
pub(crate) struct MediaSource {
    attrs: IMFAttributes,
    state: Mutex<SourceState>,
    _token: ObjectToken,
}

impl_attributes!(MediaSource_Impl, "Source");

impl MediaSource {
    /// Builds the source and its stream. `init` are the activator's attributes.
    pub(crate) fn create(init: Option<&IMFAttributes>) -> Result<ComObject<MediaSource>> {
        let attrs = new_attributes(4)?;
        if let Some(init) = init {
            // SAFETY: plain call on live COM objects.
            unsafe { init.CopyAllItems(&attrs) }?;
        }
        if let Err(e) = add_sensor_profiles(&attrs) {
            log(format_args!("sensor profiles not set (continuing): {e}"));
        }

        let descriptor = stream::create_descriptor()?;
        // SAFETY: plain calls; the slice outlives the call.
        let pd = unsafe { MFCreatePresentationDescriptor(Some(&[Some(descriptor.clone())])) }?;
        // SAFETY: plain call on a live COM object.
        unsafe { pd.SelectStream(0) }?;
        // SAFETY: plain call.
        let queue = unsafe { MFCreateEventQueue() }?;

        let source = ComObject::new(MediaSource {
            attrs,
            state: Mutex::new(SourceState {
                shutdown: false,
                queue,
                pd,
                stream: None,
            }),
            _token: ObjectToken::new(),
        });
        let as_source: IMFMediaSource = source.cast()?;
        let stream = MediaStream::create(as_source, descriptor)?;
        lock(&source.state).stream = Some(stream);
        Ok(source)
    }

    fn queue(&self) -> Result<IMFMediaEventQueue> {
        let st = lock(&self.state);
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(st.queue.clone())
    }
}

/// As VCamSample: a legacy (<= 30 fps) and a high-frame-rate profile for stream 0.
fn add_sensor_profiles(attrs: &IMFAttributes) -> Result<()> {
    // SAFETY: plain calls on live COM objects with static GUIDs and literal strings.
    unsafe {
        let collection = MFCreateSensorProfileCollection()?;
        let legacy = MFCreateSensorProfile(&KSCAMERAPROFILE_Legacy, 0, PCWSTR::null())?;
        legacy.AddProfileFilter(0, windows_core::w!("((RES==;FRT<=30,1;SUT==))"))?;
        collection.AddProfile(&legacy)?;
        let high = MFCreateSensorProfile(&KSCAMERAPROFILE_HighFrameRate, 0, PCWSTR::null())?;
        high.AddProfileFilter(0, windows_core::w!("((RES==;FRT>=60,1;SUT==))"))?;
        collection.AddProfile(&high)?;
        attrs.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &collection)
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        guard("Source::GetEvent", || {
            let q = self.queue()?;
            // SAFETY: plain call; may block, and no lock is held.
            unsafe { q.GetEvent(flags.0) }
        })
    }

    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        guard("Source::BeginGetEvent", || {
            let q = self.queue()?;
            // SAFETY: plain call forwarding the caller's objects.
            unsafe { q.BeginGetEvent(callback.as_ref(), state.as_ref()) }
        })
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        guard("Source::EndGetEvent", || {
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
        guard("Source::QueueEvent", || {
            let q = self.queue()?;
            // SAFETY: forwards the caller's pointers unchanged; same contract.
            unsafe { q.QueueEventParamVar(met, ext, status, value) }
        })
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        guard("Source::GetCharacteristics", || {
            self.queue()?;
            Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
        })
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        guard("Source::CreatePresentationDescriptor", || {
            let st = lock(&self.state);
            if st.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            // SAFETY: plain call on a live COM object.
            unsafe { st.pd.Clone() }
        })
    }

    fn Start(
        &self,
        pd: Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        _start: *const PROPVARIANT,
    ) -> Result<()> {
        guard("Source::Start", || {
            log("Source::Start");
            let pd = pd.ok()?;
            // SAFETY: checked for null before the read; points to the caller's GUID.
            if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
                return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
            }
            let st = lock(&self.state);
            if st.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            let stream = st.stream.clone().ok_or(MF_E_SHUTDOWN)?;
            // SAFETY: plain call.
            let now = unsafe { MFGetSystemTime() };
            // SAFETY: plain call on a live COM object.
            let count = unsafe { pd.GetStreamDescriptorCount() }?;
            for i in 0..count {
                let mut selected = BOOL::default();
                let mut sd = None;
                // SAFETY: both out-pointers are valid locals.
                unsafe { pd.GetStreamDescriptorByIndex(i, &mut selected, &mut sd) }?;
                let sd = sd.ok_or(MF_E_INVALIDSTREAMNUMBER)?;
                // SAFETY: plain call on a live COM object.
                if unsafe { sd.GetStreamIdentifier() }? != 0 {
                    return Err(MF_E_INVALIDSTREAMNUMBER.into());
                }
                if selected.as_bool() {
                    let was_running = stream.is_running();
                    let event = if was_running {
                        MEUpdatedStream
                    } else {
                        MENewStream
                    };
                    let unknown: IUnknown = stream.to_interface();
                    // SAFETY: plain calls on live COM objects.
                    unsafe {
                        st.pd.SelectStream(0)?;
                        st.queue.QueueEventParamUnk(
                            event.0 as u32,
                            &GUID::zeroed(),
                            S_OK,
                            &unknown,
                        )?;
                    }
                    stream.start(now)?;
                } else {
                    // SAFETY: plain call on a live COM object.
                    unsafe { st.pd.DeselectStream(0) }?;
                    stream.stop(now)?;
                }
            }
            // SAFETY: the PROPVARIANT outlives the call, which copies it.
            unsafe {
                st.queue.QueueEventParamVar(
                    MESourceStarted.0 as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &pv_i64(now),
                )
            }
        })
    }

    fn Stop(&self) -> Result<()> {
        guard("Source::Stop", || {
            log("Source::Stop");
            let st = lock(&self.state);
            if st.shutdown {
                return Err(MF_E_SHUTDOWN.into());
            }
            // SAFETY: plain call.
            let now = unsafe { MFGetSystemTime() };
            if let Some(stream) = &st.stream {
                stream.stop(now)?;
            }
            // SAFETY: the PROPVARIANT outlives the call, which copies it.
            unsafe {
                st.queue.QueueEventParamVar(
                    MESourceStopped.0 as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &pv_i64(now),
                )
            }
        })
    }

    fn Pause(&self) -> Result<()> {
        guard(
            "Source::Pause",
            || Err(MF_E_INVALID_STATE_TRANSITION.into()),
        )
    }

    fn Shutdown(&self) -> Result<()> {
        guard("Source::Shutdown", || {
            log("Source::Shutdown");
            let mut st = lock(&self.state);
            if st.shutdown {
                return Ok(());
            }
            st.shutdown = true;
            if let Some(stream) = st.stream.take() {
                stream.shutdown();
            }
            // SAFETY: plain call on a live COM object.
            unsafe { st.queue.Shutdown() }
        })
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        guard("Source::GetSourceAttributes", || {
            self.queue()?;
            Ok(self.attrs.clone())
        })
    }

    fn GetStreamAttributes(&self, id: u32) -> Result<IMFAttributes> {
        guard("Source::GetStreamAttributes", || {
            if id != 0 {
                return Err(MF_E_INVALIDSTREAMNUMBER.into());
            }
            let st = lock(&self.state);
            let stream = st.stream.as_ref().ok_or(MF_E_SHUTDOWN)?;
            Ok(stream.attributes())
        })
    }

    fn SetD3DManager(&self, manager: Ref<IUnknown>) -> Result<()> {
        guard("Source::SetD3DManager", || {
            // Samples are always system memory for now, so the manager is unused.
            log(format_args!(
                "Source::SetD3DManager (null={}, ignored)",
                manager.is_null()
            ));
            Ok(())
        })
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(
        &self,
        service: *const GUID,
        _riid: *const GUID,
        _ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        guard_service(service)
    }
}

fn guard_service(service: *const GUID) -> Result<()> {
    super::dll::guard_quiet("Source::GetService", || {
        // SAFETY: checked for null before the read.
        let s = (!service.is_null()).then(|| unsafe { *service });
        log(format_args!("Source::GetService {s:?} -> unsupported"));
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    })
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        super::dll::guard_quiet("Source::KsProperty", || Err(ks_not_found()))
    }

    fn KsMethod(
        &self,
        _m: *const KSIDENTIFIER,
        _ml: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        super::dll::guard_quiet("Source::KsMethod", || Err(ks_not_found()))
    }

    fn KsEvent(
        &self,
        _e: *const KSIDENTIFIER,
        _el: u32,
        _d: *mut core::ffi::c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        super::dll::guard_quiet("Source::KsEvent", || Err(ks_not_found()))
    }
}
