//! Media Foundation plumbing for capture devices: starting MF, enumerating the devices,
//! opening one as a source reader, reading its native media types, and finding the
//! container ID that pairs it with an audio endpoint.

use std::iter;

use jstream_platform::{DeviceId, PlatformError};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL,
    CM_Locate_DevNodeW, CR_SUCCESS,
};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_ContainerId;
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_ContainerId, DEVPKEY_Device_InstanceId, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
    DEVPROPTYPE,
};
use windows::Win32::Foundation::{E_ACCESSDENIED, ERROR_SHARING_VIOLATION};
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaSource, IMFMediaType, IMFSample,
    IMFSourceReader, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_E_HW_MFT_FAILED_START_STREAMING,
    MF_E_NO_MORE_TYPES, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED,
    MF_E_VIDEO_RECORDING_DEVICE_PREEMPTED, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX,
    MF_READWRITE_DISABLE_CONVERTERS, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
    MFCreateAttributes, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources,
    MFSTARTUP_NOSOCKET, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToGUID;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{GUID, PCWSTR, PWSTR};

use super::format::{Native, RawType, natives};
use crate::com::{MtaUsage, OsResult, os_error, take_pwstr};

/// What `NotFound` names.
pub(crate) const KIND: &str = "video capture device";

/// Media Foundation started, and the process's multithreaded apartment kept alive, for as
/// long as the guard lives. Both are reference-counted by the OS, so every holder starts
/// its own. With the MTA alive, a thread that never initialised COM (the engine's control
/// thread, a capture thread) is an implicit MTA member and may use MF's free-threaded
/// objects.
pub(crate) struct MfStarted {
    /// Dropped after `MFShutdown` in `Drop`, so the apartment outlives Media Foundation.
    _mta: MtaUsage,
}

impl MfStarted {
    pub(crate) fn start() -> Result<Self, PlatformError> {
        let mta = MtaUsage::new()?;
        // SAFETY: plain call; balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.os("MFStartup")?;
        Ok(Self { _mta: mta })
    }
}

impl Drop for MfStarted {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup in `start`, exactly once.
        let _ = unsafe { MFShutdown() };
    }
}

/// The source reader's first video stream, as the `u32` its methods take.
pub(crate) const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// A capture device that enumeration found.
pub(crate) struct Found {
    activate: IMFActivate,
    /// Its symbolic link: the stable `DeviceId`.
    pub(crate) link: String,
    /// Its friendly name.
    pub(crate) name: String,
}

fn attributes(count: u32) -> Result<IMFAttributes, PlatformError> {
    let mut attrs = None;
    // SAFETY: `attrs` is a valid out-pointer.
    unsafe { MFCreateAttributes(&mut attrs, count) }.os("MFCreateAttributes")?;
    attrs.ok_or_else(|| PlatformError::Os {
        call: "MFCreateAttributes",
        code: None,
        message: "succeeded without attributes".into(),
    })
}

/// A string attribute, or `None` when it is missing.
fn string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut ptr = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: valid out-pointers; the string is CoTaskMem-allocated and take_pwstr frees
    // it.
    unsafe { attrs.GetAllocatedString(key, &mut ptr, &mut len) }
        .ok()
        .map(|()| take_pwstr(ptr))
}

/// The video capture devices present now, in Media Foundation's order.
pub(crate) fn enumerate() -> Result<Vec<Found>, PlatformError> {
    let attrs = attributes(1)?;
    // SAFETY: plain COM call on live attributes.
    unsafe {
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
    }
    .os("IMFAttributes::SetGUID")?;
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the out-pointers point to valid locals.
    unsafe { MFEnumDeviceSources(&attrs, &mut array, &mut count) }.os("MFEnumDeviceSources")?;
    if array.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: MFEnumDeviceSources returned `count` initialised entries at `array`. Each
    // reference is moved out exactly once, then the array itself is freed with
    // CoTaskMemFree.
    let activates: Vec<IMFActivate> = unsafe {
        let items = std::slice::from_raw_parts_mut(array, count as usize);
        let out = items.iter_mut().filter_map(Option::take).collect();
        CoTaskMemFree(Some(array as *const _));
        out
    };
    Ok(activates
        .into_iter()
        .filter_map(|activate| {
            let link = string(
                &activate,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
            )?;
            let name = string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
                .unwrap_or_else(|| "(unnamed video capture device)".into());
            Some(Found {
                activate,
                link,
                name,
            })
        })
        .collect())
}

/// The device with this ID, or `NotFound`.
pub(crate) fn find(id: &DeviceId) -> Result<Found, PlatformError> {
    enumerate()?
        .into_iter()
        .find(|f| f.link == id.as_str())
        .ok_or_else(|| PlatformError::NotFound {
            kind: KIND,
            id: id.as_str().into(),
        })
}

/// Maps an error from activating or starting a device. Another application holding the
/// device is `Busy`; a device that went away between the lookup and the start is
/// `NotFound`.
pub(crate) fn start_error(call: &'static str, id: &str, e: &windows::core::Error) -> PlatformError {
    let code = e.code();
    if code == MF_E_HW_MFT_FAILED_START_STREAMING
        || code == MF_E_VIDEO_RECORDING_DEVICE_PREEMPTED
        || code == ERROR_SHARING_VIOLATION.to_hresult()
    {
        PlatformError::Busy { what: KIND }
    } else if code == MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED {
        PlatformError::NotFound {
            kind: KIND,
            id: id.into(),
        }
    } else if code == E_ACCESSDENIED {
        PlatformError::Os {
            call,
            code: Some(code.0.cast_unsigned()),
            message: "access to the camera was denied; check Windows' camera privacy \
                      settings"
                .into(),
        }
    } else {
        os_error(call, e)
    }
}

/// A device opened as a source reader that delivers its native media types unconverted.
/// Dropping it shuts the media source down, as Microsoft asks for objects made by
/// `ActivateObject`.
pub(crate) struct Opened {
    pub(crate) reader: IMFSourceReader,
    source: IMFMediaSource,
    activate: IMFActivate,
}

impl Found {
    pub(crate) fn open(&self) -> Result<Opened, PlatformError> {
        // SAFETY: plain COM call on a live activation object.
        let source: IMFMediaSource = unsafe { self.activate.ActivateObject() }
            .map_err(|e| start_error("IMFActivate::ActivateObject", &self.link, &e))?;
        let opened = |reader| Opened {
            reader,
            source: source.clone(),
            activate: self.activate.clone(),
        };
        let attrs = attributes(1)?;
        // SAFETY: plain COM calls on live objects. Converters are disabled so the reader
        // delivers exactly the native type selected.
        let reader = unsafe {
            attrs
                .SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)
                .os("IMFAttributes::SetUINT32")?;
            MFCreateSourceReaderFromMediaSource(&source, &attrs)
        };
        match reader {
            Ok(reader) => Ok(opened(reader)),
            Err(e) => {
                // SAFETY: shuts down what ActivateObject created, once.
                unsafe {
                    let _ = source.Shutdown();
                    let _ = self.activate.ShutdownObject();
                }
                Err(start_error(
                    "MFCreateSourceReaderFromMediaSource",
                    &self.link,
                    &e,
                ))
            }
        }
    }
}

impl Opened {
    /// The native media types of the first video stream, as read.
    pub(crate) fn raw_types(&self) -> Result<Vec<(RawType, IMFMediaType)>, PlatformError> {
        let mut out = Vec::new();
        for index in 0.. {
            // SAFETY: plain COM call on a live reader.
            let t = match unsafe { self.reader.GetNativeMediaType(VIDEO_STREAM, index) } {
                Ok(t) => t,
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                Err(e) => return Err(os_error("IMFSourceReader::GetNativeMediaType", &e)),
            };
            // SAFETY: plain attribute reads on a live media type.
            let raw = unsafe {
                RawType {
                    index,
                    subtype: t.GetGUID(&MF_MT_SUBTYPE).map(|g| g.to_u128()).unwrap_or(0),
                    size: t.GetUINT64(&MF_MT_FRAME_SIZE).ok(),
                    rate: t.GetUINT64(&MF_MT_FRAME_RATE).ok(),
                }
            };
            out.push((raw, t));
        }
        Ok(out)
    }

    /// Whether the reader's current type still has this subtype and frame size.
    pub(crate) fn current_is(
        &self,
        subtype: u128,
        size: jstream_platform::media::FrameSize,
    ) -> bool {
        let packed = (u64::from(size.width()) << 32) | u64::from(size.height());
        // SAFETY: plain COM calls on a live reader and the type it returns.
        unsafe {
            self.reader
                .GetCurrentMediaType(VIDEO_STREAM)
                .is_ok_and(|t| {
                    t.GetGUID(&MF_MT_SUBTYPE)
                        .is_ok_and(|g| g.to_u128() == subtype)
                        && t.GetUINT64(&MF_MT_FRAME_SIZE) == Ok(packed)
                })
        }
    }

    /// The formats the backend can deliver, with each one's native type index.
    pub(crate) fn natives(&self) -> Result<Vec<Native>, PlatformError> {
        Ok(natives(self.raw_types()?.into_iter().map(|(raw, _)| raw)))
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: plain COM calls on live objects; errors at teardown change nothing.
        unsafe {
            let _ = self.source.Shutdown();
            let _ = self.activate.ShutdownObject();
        }
    }
}

/// What a media type says about its frames' colour and layout.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TypeInfo {
    /// `MF_MT_YUV_MATRIX`.
    pub(crate) matrix: Option<u32>,
    /// `MF_MT_VIDEO_NOMINAL_RANGE`.
    pub(crate) range: Option<u32>,
    /// `MF_MT_DEFAULT_STRIDE`, signed as Media Foundation stores it.
    pub(crate) stride: Option<i32>,
}

pub(crate) fn type_info(t: &IMFMediaType) -> TypeInfo {
    // SAFETY: plain attribute reads on a live media type.
    unsafe {
        TypeInfo {
            matrix: t.GetUINT32(&MF_MT_YUV_MATRIX).ok(),
            range: t.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE).ok(),
            stride: t
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .ok()
                .map(u32::cast_signed),
        }
    }
}

/// The container ID of the device behind `link` (a device interface path), through the
/// configuration manager: interface, then device instance, then its container. `None`
/// when any step fails.
pub(crate) fn container_id(link: &str) -> Option<u128> {
    let link: Vec<u16> = link.encode_utf16().chain(iter::once(0)).collect();
    // MAX_DEVICE_ID_LEN is 200 characters; the buffer is zeroed, so it stays terminated.
    let mut instance = [0u16; 256];
    let mut ty = DEVPROPTYPE::default();
    let mut size = size_of_val(&instance) as u32;
    // SAFETY: `link` is null-terminated; the property buffer is `size` bytes long and the
    // other out-pointers point to valid locals.
    let cr = unsafe {
        CM_Get_Device_Interface_PropertyW(
            PCWSTR(link.as_ptr()),
            &DEVPKEY_Device_InstanceId,
            &mut ty,
            Some(instance.as_mut_ptr().cast()),
            &mut size,
            0,
        )
    };
    if cr != CR_SUCCESS || ty != DEVPROP_TYPE_STRING {
        return None;
    }
    let mut node = 0u32;
    // SAFETY: `instance` is a null-terminated string the call above wrote.
    let cr = unsafe {
        CM_Locate_DevNodeW(
            &mut node,
            PCWSTR(instance.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        )
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let mut guid = GUID::zeroed();
    let mut size = size_of::<GUID>() as u32;
    // SAFETY: the property buffer is a GUID, `size` bytes long; the other out-pointers
    // point to valid locals.
    let cr = unsafe {
        CM_Get_DevNode_PropertyW(
            node,
            &DEVPKEY_Device_ContainerId,
            &mut ty,
            Some((&raw mut guid).cast()),
            &mut size,
            0,
        )
    };
    (cr == CR_SUCCESS && ty == DEVPROP_TYPE_GUID).then(|| guid.to_u128())
}

/// The active audio input endpoints, in WASAPI's order, each with its container ID.
/// Their IDs are the ones `WasapiAudio` lists. Best effort: empty if WASAPI can't be
/// reached, so a video device then has no paired audio.
pub(crate) fn audio_inputs() -> Vec<(DeviceId, Option<u128>)> {
    let read = || -> windows::core::Result<Vec<(DeviceId, Option<u128>)>> {
        // SAFETY: plain COM activation; the caller holds the MTA alive.
        let e: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        // SAFETY: plain COM calls.
        let all = unsafe { e.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }?;
        // SAFETY: as above.
        let count = unsafe { all.GetCount() }?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            // SAFETY: `i` is below the collection's count; the ID string is taken and
            // freed by take_pwstr.
            let device = unsafe { all.Item(i) }?;
            // SAFETY: as above.
            let id = take_pwstr(unsafe { device.GetId() }?);
            // SAFETY: plain COM calls; the PROPVARIANT is cleared by its Drop.
            let container = unsafe {
                device
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|store| store.GetValue(&PKEY_Device_ContainerId))
                    .and_then(|value| PropVariantToGUID(&value))
            };
            out.push((DeviceId::new(id), container.ok().map(|g| g.to_u128())));
        }
        Ok(out)
    };
    read().unwrap_or_default()
}

/// Calls `f` with a sample's bytes, locked for the duration of the call (and unlocked
/// even if `f` panics).
pub(crate) fn with_bytes<R>(
    sample: &IMFSample,
    f: impl FnOnce(&[u8]) -> R,
) -> Result<R, PlatformError> {
    /// A locked buffer, unlocked on drop.
    struct Locked(IMFMediaBuffer);
    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: balances the successful Lock below, once.
            let _ = unsafe { self.0.Unlock() };
        }
    }
    // SAFETY: plain COM calls on live objects.
    let buffer =
        unsafe { sample.ConvertToContiguousBuffer() }.os("IMFSample::ConvertToContiguousBuffer")?;
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: valid out-pointers.
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len)) }.os("IMFMediaBuffer::Lock")?;
    let locked = Locked(buffer);
    let bytes: &[u8] = if ptr.is_null() {
        &[]
    } else {
        // SAFETY: the locked pointer is valid for `len` bytes until Unlock, which runs
        // when `locked` drops, after the last use of `bytes`.
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    };
    let out = f(bytes);
    drop(locked);
    Ok(out)
}
