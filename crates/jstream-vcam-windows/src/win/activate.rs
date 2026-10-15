//! The registered COM class: an `IMFActivate` (VCamSample's `Activator`). Frame Server
//! creates it, may set attributes on it, then calls `ActivateObject` for the media source.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFMediaSource,
    MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, MFCreateAttributes,
    MFT_TRANSFORM_CLSID_Attribute,
};
use windows_core::{ComObject, GUID, Interface, Result, implement};

use super::CLSID;
use super::attrs::impl_attributes;
use super::dll::{ObjectToken, guard};
use super::log::log;
use super::source::MediaSource;

/// A fresh, empty attribute store.
pub(crate) fn new_attributes(initial: u32) -> Result<IMFAttributes> {
    let mut attrs = None;
    // SAFETY: `attrs` is a valid out-pointer.
    unsafe { MFCreateAttributes(&mut attrs, initial) }?;
    attrs.ok_or_else(|| E_POINTER.into())
}

/// Locks a mutex, ignoring poisoning: a panic is already caught and logged by the guard,
/// and the state it protects stays usable.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[implement(IMFActivate)]
pub(crate) struct Activate {
    attrs: IMFAttributes,
    source: Mutex<Option<IMFMediaSource>>,
    _token: ObjectToken,
}

impl_attributes!(Activate_Impl, "Activate");

impl Activate {
    pub(crate) fn create() -> Result<ComObject<Activate>> {
        let attrs = new_attributes(2)?;
        // As VCamSample: tell Frame Server this source exposes associated camera sources,
        // and which class it is.
        // SAFETY: plain calls on a live attribute store with pointers to statics.
        unsafe {
            attrs.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)?;
            attrs.SetGUID(&MFT_TRANSFORM_CLSID_Attribute, &CLSID)?;
        }
        Ok(ComObject::new(Activate {
            attrs,
            source: Mutex::new(None),
            _token: ObjectToken::new(),
        }))
    }

    /// Creates the media source on first use and returns interface `riid` of it.
    pub(crate) fn activate(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if riid.is_null() || ppv.is_null() {
            return Err(E_POINTER.into());
        }
        let mut slot = lock(&self.source);
        let source = match &*slot {
            Some(s) => s.clone(),
            None => {
                let s: IMFMediaSource = MediaSource::create(Some(&self.attrs))?.cast()?;
                log("Activate: media source created");
                *slot = Some(s.clone());
                s
            }
        };
        // SAFETY: `riid` and `ppv` are valid caller pointers (checked above).
        unsafe { source.query(riid, ppv) }.ok()
    }
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        guard("Activate::ActivateObject", || {
            // SAFETY: `riid` is checked for null before the read.
            let iid = (!riid.is_null()).then(|| unsafe { *riid });
            log(format_args!(
                "Activate::ActivateObject riid={iid:?} {}",
                super::log::identity()
            ));
            self.activate(riid, ppv)
        })
    }

    fn ShutdownObject(&self) -> Result<()> {
        guard("Activate::ShutdownObject", || {
            log("Activate::ShutdownObject");
            if let Some(s) = lock(&self.source).take() {
                // SAFETY: plain call on a live COM object.
                unsafe { s.Shutdown() }?;
            }
            Ok(())
        })
    }

    fn DetachObject(&self) -> Result<()> {
        guard("Activate::DetachObject", || {
            log("Activate::DetachObject");
            lock(&self.source).take();
            Ok(())
        })
    }
}
