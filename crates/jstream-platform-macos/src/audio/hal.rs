//! Reading Core Audio HAL properties (ADR-0063 §4): the device list, the default
//! devices, a device's UID, name, channels and nominal rate, and its input streams'
//! virtual formats.
//!
//! Every read is `AudioObjectGetPropertyData` on a property address with the main
//! element, into a value or buffer this module owns, so the `unsafe` stays here.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};

use jstream_platform::{AudioDeviceInfo, AudioDirection, DeviceId, PlatformError};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyPreferredChannelsForStereo, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyStreams, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioStreamPropertyVirtualFormat,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamBasicDescription};
use objc2_core_foundation::{CFRetained, CFString};

use super::layout::{StreamPlan, is_native_f32, plan_stream, rate_hz};
use crate::fourcc::os_status;

/// The system object, which owns the device list and the defaults.
pub(super) const SYSTEM: AudioObjectID = kAudioObjectSystemObject.cast_unsigned();

/// `noErr`.
const NO_ERR: i32 = 0;

/// A property address on the main element.
pub(super) fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// The scope a direction's streams and channels are in.
pub(super) fn scope(direction: AudioDirection) -> AudioObjectPropertyScope {
    match direction {
        AudioDirection::Input => kAudioObjectPropertyScopeInput,
        AudioDirection::Output => kAudioObjectPropertyScopeOutput,
    }
}

fn check(status: i32, call: &'static str, what: &str) -> Result<(), PlatformError> {
    if status == NO_ERR {
        Ok(())
    } else {
        Err(os_status(call, status, what))
    }
}

/// The size in bytes of a property's value.
fn data_size(
    object: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
) -> Result<u32, PlatformError> {
    let mut size = 0u32;
    // SAFETY: `addr` and `size` are valid for the call; no qualifier is passed.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
        )
    };
    check(
        status,
        "AudioObjectGetPropertyDataSize",
        "reading an audio property's size",
    )?;
    Ok(size)
}

/// Reads a property into `out`, which is `capacity` bytes, and returns the bytes written.
///
/// # Safety
/// `out` must be valid for writes of `capacity` bytes, and suitably aligned for the
/// property's value.
unsafe fn read_raw(
    object: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    out: NonNull<c_void>,
    capacity: u32,
) -> Result<u32, PlatformError> {
    let mut size = capacity;
    // SAFETY: the caller guarantees `out` holds `capacity` bytes; `size` tells the HAL so,
    // and the HAL writes at most that many and returns how many it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(addr),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            out,
        )
    };
    check(
        status,
        "AudioObjectGetPropertyData",
        "reading an audio property",
    )?;
    Ok(size)
}

/// Marks the plain-old-data values a property can be read into: every bit pattern of
/// the type's size is a valid value.
///
/// # Safety
/// Implement only for types with no invalid bit patterns, padding the HAL doesn't care
/// about, and no pointers the HAL would hand ownership through.
unsafe trait Pod: Copy {}
// SAFETY: integers and floats have no invalid bit patterns.
unsafe impl Pod for u32 {}
// SAFETY: as above.
unsafe impl Pod for f64 {}
// SAFETY: as above, for two of them.
unsafe impl Pod for [u32; 2] {}
// SAFETY: `AudioStreamBasicDescription` is a C struct of a `Float64` and `UInt32`s.
unsafe impl Pod for AudioStreamBasicDescription {}

/// Reads a fixed-size property into a `T`, starting from `init`.
fn read_value<T: Pod>(
    object: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    init: T,
) -> Result<T, PlatformError> {
    let mut value = init;
    let capacity = u32::try_from(size_of::<T>()).unwrap_or(u32::MAX);
    // SAFETY: `value` is a live, aligned `T` of `capacity` bytes, and `T: Pod`, so whatever
    // the HAL writes into it is a valid `T`.
    let written = unsafe { read_raw(object, addr, NonNull::from(&mut value).cast(), capacity) }?;
    if written as usize != size_of::<T>() {
        return Err(PlatformError::Os {
            call: "AudioObjectGetPropertyData",
            code: None,
            message: format!(
                "an audio property was {written} bytes, not {}",
                size_of::<T>()
            ),
        });
    }
    Ok(value)
}

/// Reads a property holding an array of `T`.
fn read_list<T: Pod>(
    object: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    zero: T,
) -> Result<Vec<T>, PlatformError> {
    let bytes = data_size(object, addr)?;
    let mut items = vec![zero; bytes as usize / size_of::<T>()];
    if items.is_empty() {
        return Ok(items);
    }
    let capacity = u32::try_from(items.len() * size_of::<T>()).unwrap_or(u32::MAX);
    // SAFETY: `items` holds `capacity` bytes of aligned `T`s, and `T: Pod`.
    let written = unsafe { read_raw(object, addr, NonNull::from(&mut items[0]).cast(), capacity) }?;
    // The list can shrink between the two calls.
    items.truncate(written as usize / size_of::<T>());
    Ok(items)
}

/// Reads a property holding a `CFStringRef` the caller owns (the HAL's name and UID
/// properties are documented as returning a retained string).
fn read_string(
    object: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
) -> Result<String, PlatformError> {
    let mut raw: *const CFString = ptr::null();
    let capacity = u32::try_from(size_of::<*const CFString>()).unwrap_or(u32::MAX);
    // SAFETY: `raw` is a live, aligned pointer-sized slot; the HAL writes a CFStringRef
    // into it (or nothing, leaving it null).
    unsafe { read_raw(object, addr, NonNull::from(&mut raw).cast(), capacity) }?;
    let raw = NonNull::new(raw.cast_mut()).ok_or_else(|| PlatformError::Os {
        call: "AudioObjectGetPropertyData",
        code: None,
        message: "an audio string property was null".into(),
    })?;
    // SAFETY: the string is retained for us (+1), so `CFRetained` takes over that reference
    // and releases it when dropped.
    let string = unsafe { CFRetained::from_raw(raw) };
    Ok(string.to_string())
}

/// The devices present now.
fn device_ids() -> Result<Vec<AudioObjectID>, PlatformError> {
    read_list(
        SYSTEM,
        &address(
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
        ),
        0,
    )
}

/// The system default device for `direction`, if there is one.
pub(super) fn default_device(
    direction: AudioDirection,
) -> Result<Option<AudioObjectID>, PlatformError> {
    let selector = match direction {
        AudioDirection::Input => kAudioHardwarePropertyDefaultInputDevice,
        AudioDirection::Output => kAudioHardwarePropertyDefaultOutputDevice,
    };
    let id = read_value(
        SYSTEM,
        &address(selector, kAudioObjectPropertyScopeGlobal),
        kAudioObjectUnknown,
    )?;
    Ok((id != kAudioObjectUnknown).then_some(id))
}

/// A device's persistent UID, the `DeviceId` this backend lists it under.
pub(super) fn uid(device: AudioObjectID) -> Result<String, PlatformError> {
    read_string(
        device,
        &address(
            kAudioDevicePropertyDeviceUID,
            kAudioObjectPropertyScopeGlobal,
        ),
    )
}

/// The channel count of each buffer the device delivers in `direction`
/// (`kAudioDevicePropertyStreamConfiguration`, an `AudioBufferList` with no data).
pub(super) fn buffer_channels(
    device: AudioObjectID,
    direction: AudioDirection,
) -> Result<Vec<u32>, PlatformError> {
    let addr = address(kAudioDevicePropertyStreamConfiguration, scope(direction));
    let bytes = data_size(device, &addr)? as usize;
    if bytes < size_of::<u32>() {
        return Ok(Vec::new());
    }
    // `u64`s, so the list is aligned for its pointers.
    let mut storage = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    let capacity = u32::try_from(storage.len() * size_of::<u64>()).unwrap_or(u32::MAX);
    // SAFETY: `storage` is `capacity` bytes, 8-byte aligned (as `AudioBufferList` needs),
    // and zeroed; an `AudioBufferList` is plain data whose pointers are only read below as
    // values, never dereferenced.
    let written = unsafe {
        read_raw(
            device,
            &addr,
            NonNull::from(&mut storage[0]).cast(),
            capacity,
        )
    }? as usize;
    // SAFETY: at least four bytes were checked above, in `storage`; `mNumberBuffers`, a
    // `UInt32`, is the list's first field.
    let count = unsafe { storage.as_ptr().cast::<u32>().read() } as usize;
    let header = std::mem::offset_of!(AudioBufferList, mBuffers);
    let fits = written.min(bytes).saturating_sub(header) / size_of::<AudioBuffer>();
    let first = storage
        .as_ptr()
        .cast::<u8>()
        .wrapping_add(header)
        .cast::<AudioBuffer>();
    Ok((0..count.min(fits))
        .map(|i| {
            // SAFETY: buffer `i` lies inside the bytes the HAL wrote (`fits`), which are in
            // `storage`; `AudioBuffer` is plain data.
            unsafe { first.add(i).read() }.mNumberChannels
        })
        .collect())
}

/// The device's IO buffer size in frames, if it reports one: how many frames an IOProc
/// cycle usually carries.
pub(super) fn buffer_frames(device: AudioObjectID) -> Option<u32> {
    read_value(
        device,
        &address(
            kAudioDevicePropertyBufferFrameSize,
            kAudioObjectPropertyScopeGlobal,
        ),
        0u32,
    )
    .ok()
}

/// Whether the device is still present and working.
pub(super) fn is_alive(device: AudioObjectID) -> bool {
    read_value(
        device,
        &address(
            kAudioDevicePropertyDeviceIsAlive,
            kAudioObjectPropertyScopeGlobal,
        ),
        0u32,
    )
    .is_ok_and(|alive| alive != 0)
}

/// The input devices' and output devices' entries for one device, if it has channels in
/// that direction.
fn entries(
    device: AudioObjectID,
    defaults: [Option<AudioObjectID>; 2],
) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
    let id = uid(device)?;
    let name = read_string(
        device,
        &address(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal),
    )
    .unwrap_or_else(|_| id.clone());
    let mut out = Vec::new();
    for (direction, default) in [AudioDirection::Input, AudioDirection::Output]
        .into_iter()
        .zip(defaults)
    {
        if buffer_channels(device, direction)?.iter().any(|&c| c > 0) {
            out.push(AudioDeviceInfo {
                id: DeviceId::new(id.clone()),
                name: name.clone(),
                direction,
                is_default: default == Some(device),
            });
        }
    }
    Ok(out)
}

/// Every input and output, each device listed once per direction it has channels in,
/// under its UID. A device that disappears while being read is left out.
pub(super) fn list() -> Result<Vec<AudioDeviceInfo>, PlatformError> {
    let defaults = [
        default_device(AudioDirection::Input)?,
        default_device(AudioDirection::Output)?,
    ];
    Ok(device_ids()?
        .into_iter()
        .filter_map(|d| entries(d, defaults).ok())
        .flatten()
        .collect())
}

/// The device with UID `id` that has channels in `direction`.
///
/// # Errors
/// [`PlatformError::NotFound`] when no such device is present.
pub(super) fn find(
    id: &DeviceId,
    direction: AudioDirection,
) -> Result<AudioObjectID, PlatformError> {
    device_ids()?
        .into_iter()
        .find(|&d| {
            uid(d).is_ok_and(|u| u == id.as_str())
                && buffer_channels(d, direction).is_ok_and(|b| b.iter().any(|&c| c > 0))
        })
        .ok_or_else(|| PlatformError::NotFound {
            kind: "audio device",
            id: id.as_str().to_owned(),
        })
}

/// How to record or play `device` in `direction`: its streams in that direction must be
/// native `f32`, at a whole nominal rate.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another virtual format, a fractional rate or no
/// channels in that direction, and [`PlatformError::Os`] when a property can't be read.
pub(super) fn plan(
    device: AudioObjectID,
    direction: AudioDirection,
) -> Result<StreamPlan, PlatformError> {
    let side = scope(direction);
    let nominal = read_value(
        device,
        &address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        0f64,
    )?;
    let rate = rate_hz(nominal).ok_or_else(|| PlatformError::Unsupported {
        what: format!("an audio device running at {nominal} Hz"),
    })?;
    let streams = read_list(device, &address(kAudioDevicePropertyStreams, side), 0)?;
    for stream in streams {
        let zero = AudioStreamBasicDescription {
            mSampleRate: 0.0,
            mFormatID: 0,
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: 0,
            mBytesPerFrame: 0,
            mChannelsPerFrame: 0,
            mBitsPerChannel: 0,
            mReserved: 0,
        };
        let f = read_value(
            stream,
            &address(
                kAudioStreamPropertyVirtualFormat,
                kAudioObjectPropertyScopeGlobal,
            ),
            zero,
        )?;
        if !is_native_f32(f.mFormatID, f.mFormatFlags, f.mBitsPerChannel) {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "an audio {} stream in format {} with flags {:#x} and {} bits",
                    match direction {
                        AudioDirection::Input => "input",
                        AudioDirection::Output => "output",
                    },
                    crate::fourcc_str(f.mFormatID).unwrap_or_else(|| f.mFormatID.to_string()),
                    f.mFormatFlags,
                    f.mBitsPerChannel
                ),
            });
        }
    }
    let buffers = buffer_channels(device, direction)?;
    let pair = read_value(
        device,
        &address(kAudioDevicePropertyPreferredChannelsForStereo, side),
        [0u32; 2],
    )
    .ok();
    plan_stream(rate, &buffers, pair)
}
