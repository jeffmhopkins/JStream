//! Media Foundation plumbing shared by the codecs: starting MF, finding MFTs, and moving
//! bytes in and out of `IMFSample`s.

use std::mem::ManuallyDrop;

use jstream_platform::PlatformError;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFSample, IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_VERSION,
    MFCreateMemoryBuffer, MFCreateSample, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFT_ENUM_FLAG,
    MFT_FRIENDLY_NAME_Attribute, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx,
};
use windows::Win32::System::Com::{
    CO_MTA_USAGE_COOKIE, CoDecrementMTAUsage, CoIncrementMTAUsage, CoTaskMemFree,
};
use windows::core::{GUID, PWSTR};

use crate::com::{OsResult, take_pwstr};

/// Media Foundation started, and the process's multithreaded apartment kept alive, for as
/// long as a codec session lives. Both are reference-counted by the OS, so every session
/// holds its own.
///
/// Keeping the MTA alive lets a session be driven from a thread that never initialised
/// COM (the engine's encode thread): such a thread is an implicit MTA member while the
/// MTA exists.
pub(crate) struct MfRuntime {
    mta: CO_MTA_USAGE_COOKIE,
}

impl MfRuntime {
    pub(crate) fn start() -> Result<Self, PlatformError> {
        // SAFETY: plain call; balanced by CoDecrementMTAUsage in Drop.
        let mta = unsafe { CoIncrementMTAUsage() }.os("CoIncrementMTAUsage")?;
        // SAFETY: plain call; balanced by MFShutdown in Drop.
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) } {
            // SAFETY: balances the increment above.
            let _ = unsafe { CoDecrementMTAUsage(mta) };
            return Err(crate::com::os_error("MFStartup", &e));
        }
        Ok(Self { mta })
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup and CoIncrementMTAUsage in `start`.
        unsafe {
            let _ = MFShutdown();
            let _ = CoDecrementMTAUsage(self.mta);
        }
    }
}

/// An MFT that enumeration found.
pub(crate) struct Found {
    activate: IMFActivate,
    /// Its friendly name.
    pub(crate) name: String,
}

impl Found {
    /// Creates the transform.
    pub(crate) fn activate(&self) -> Result<IMFTransform, PlatformError> {
        // SAFETY: plain COM call on a live activation object.
        unsafe { self.activate.ActivateObject() }.os("IMFActivate::ActivateObject")
    }
}

/// The MFTs in `category` that take `input` and produce `output` (major type, subtype),
/// in the order Media Foundation ranks them.
pub(crate) fn enumerate(
    category: GUID,
    flags: MFT_ENUM_FLAG,
    input: (GUID, GUID),
    output: (GUID, GUID),
) -> Result<Vec<Found>, PlatformError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: input.0,
        guidSubtype: input.1,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: output.0,
        guidSubtype: output.1,
    };
    let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the type infos outlive the call; the out-pointers point to valid locals.
    unsafe {
        MFTEnumEx(
            category,
            flags,
            Some(&input),
            Some(&output),
            &mut array,
            &mut count,
        )
    }
    .os("MFTEnumEx")?;
    if array.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: MFTEnumEx returned `count` initialised entries at `array`. Each reference is
    // moved out exactly once, then the array itself is freed with CoTaskMemFree.
    let activates: Vec<IMFActivate> = unsafe {
        let items = std::slice::from_raw_parts_mut(array, count as usize);
        let out = items.iter_mut().filter_map(Option::take).collect();
        CoTaskMemFree(Some(array as *const _));
        out
    };
    Ok(activates
        .into_iter()
        .map(|activate| Found {
            name: friendly_name(&activate),
            activate,
        })
        .collect())
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut ptr = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: valid out-pointers; the string is CoTaskMem-allocated and take_pwstr frees it.
    match unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len) } {
        Ok(()) => take_pwstr(ptr),
        Err(_) => "(unnamed Media Foundation transform)".into(),
    }
}

/// A sample holding a copy of `bytes`, with a time and duration in 100 ns units.
pub(crate) fn sample_from(
    bytes: &[u8],
    time: i64,
    duration: i64,
) -> Result<IMFSample, PlatformError> {
    let len = u32::try_from(bytes.len()).map_err(|_| PlatformError::Unsupported {
        what: format!("a {}-byte media sample", bytes.len()),
    })?;
    // SAFETY: plain constructor and method calls on live objects. The locked pointer is
    // valid for `len` bytes (the buffer was created with that capacity) until Unlock, and
    // nothing else holds the buffer yet.
    unsafe {
        let buffer = MFCreateMemoryBuffer(len).os("MFCreateMemoryBuffer")?;
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
        sample.SetSampleTime(time).os("IMFSample::SetSampleTime")?;
        sample
            .SetSampleDuration(duration)
            .os("IMFSample::SetSampleDuration")?;
        Ok(sample)
    }
}

/// How a transform's output stream wants its samples.
pub(crate) struct Output {
    /// Whether the MFT allocates its own output samples.
    provides_samples: bool,
    /// The buffer size to allocate otherwise.
    buffer_size: u32,
}

impl Output {
    /// Reads output stream 0's requirements. Call after the media types are set.
    pub(crate) fn new(transform: &IMFTransform) -> Result<Self, PlatformError> {
        // SAFETY: plain COM call on a configured transform.
        let info =
            unsafe { transform.GetOutputStreamInfo(0) }.os("IMFTransform::GetOutputStreamInfo")?;
        Ok(Self {
            provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
            // Some MFTs report 0 until streaming starts; one AAC or small H.264 access
            // unit fits in 64 KiB, and a larger one fails visibly rather than truncating.
            buffer_size: if info.cbSize == 0 {
                65_536
            } else {
                info.cbSize
            },
        })
    }

    /// One `ProcessOutput` call: the bytes of the next output sample, or `None` when the
    /// MFT needs more input.
    pub(crate) fn pull(&self, transform: &IMFTransform) -> Result<Option<Vec<u8>>, PlatformError> {
        let sample = if self.provides_samples {
            None
        } else {
            // SAFETY: plain constructor and method calls on live objects.
            Some(unsafe {
                let s = MFCreateSample().os("MFCreateSample")?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.buffer_size).os("MFCreateMemoryBuffer")?)
                    .os("IMFSample::AddBuffer")?;
                s
            })
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: `buffers` is a valid one-element array and `status` a valid out-pointer.
        let result = unsafe { transform.ProcessOutput(0, &mut buffers, &mut status) };
        // SAFETY: each field is taken exactly once and `buffers` is not used again, so the
        // references in it are released exactly once.
        let (sample, _events) = unsafe {
            (
                ManuallyDrop::take(&mut buffers[0].pSample),
                ManuallyDrop::take(&mut buffers[0].pEvents),
            )
        };
        match result {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
            Err(e) => return Err(crate::com::os_error("IMFTransform::ProcessOutput", &e)),
        }
        let sample = sample.ok_or_else(|| PlatformError::Os {
            call: "IMFTransform::ProcessOutput",
            code: None,
            message: "succeeded without a sample".into(),
        })?;
        // SAFETY: plain COM calls on live objects. The locked pointer is valid for `len`
        // bytes until Unlock, and the bytes are copied out before it.
        unsafe {
            let buffer = sample
                .ConvertToContiguousBuffer()
                .os("IMFSample::ConvertToContiguousBuffer")?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer
                .Lock(&mut ptr, None, Some(&mut len))
                .os("IMFMediaBuffer::Lock")?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock().os("IMFMediaBuffer::Unlock")?;
            Ok(Some(data))
        }
    }
}
