//! Desktop and application audio through Core Audio process taps (ADR-0063 §4), from
//! macOS 14.2.
//!
//! A tap ([`CATapDescription`], `AudioHardwareCreateProcessTap`) is a stereo mixdown of
//! what some processes play, whatever the layout of the stream they play on. To record it, the backend makes a private aggregate device
//! clocked by an output device, with the tap in its tap list, and runs the same input
//! IOProc on it as on a microphone ([`super::stream`]). The tap's two channels are the
//! aggregate's last input channels ([`tap_pair`](super::layout::tap_pair)). The aggregate
//! is the backend's own: it is private to this process, its UID starts with
//! [`TAP_UID_PREFIX`](super::layout::TAP_UID_PREFIX), and the device list leaves it out.
//!
//! - **Desktop audio** (`DefaultOutputLoopback`) taps every process except JStream's own,
//!   whichever device it plays on, so JStream's monitoring isn't recorded back.
//! - **An output** (`OutputLoopback`) taps what every process except JStream plays on
//!   that device's first output stream.
//! - **An application** (`ProcessLoopback`) taps that one process. On macOS a tap
//!   targets process objects, so its child processes aren't included, unlike Windows'
//!   process loopback, which records the whole process tree.
//!
//! Without the system audio recording permission a tap records silence, and nothing
//! reports a refusal ([`SYSTEM_AUDIO_HINT`](super::permission::SYSTEM_AUDIO_HINT)).
//!
//! Dropping a [`Tap`] destroys the aggregate device and then the tap.

use jstream_platform::{AudioDirection, DeviceId, PlatformError};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectID,
    CATapDescription, kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioObjectUnknown,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::ffi::CStr;
use std::ptr::NonNull;

use super::hal;
use super::layout::{StreamPlan, TAP_UID_PREFIX};
use crate::fourcc::os_status;

/// `noErr`.
const NO_ERR: i32 = 0;

/// What a tap records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TapKind {
    /// Every process except JStream (desktop audio).
    Desktop,
    /// What every process except JStream plays on this output.
    Output(DeviceId),
    /// The process with this ID.
    Process(u32),
}

impl TapKind {
    /// What the stream records, for errors.
    pub(super) fn what(&self) -> String {
        match self {
            Self::Desktop => "desktop audio".to_owned(),
            Self::Output(id) => format!("the audio played on output {:?}", id.as_str()),
            Self::Process(pid) => format!("the audio of process {pid}"),
        }
    }
}

/// What a tap's source is, to notice it going away.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// Every process: nothing to lose.
    Everything,
    /// An output device.
    Device(AudioObjectID),
    /// A process object.
    Process(AudioObjectID),
}

/// A tap and the private aggregate device that records it.
#[derive(Debug)]
pub(super) struct Tap {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    source: Source,
}

impl Drop for Tap {
    fn drop(&mut self) {
        // SAFETY: destroys the aggregate device and the tap this value made, each once;
        // failures (already gone) leave nothing to undo. Callers stop the IOProc on the
        // aggregate first.
        unsafe {
            if self.aggregate != kAudioObjectUnknown {
                let _ = AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != kAudioObjectUnknown {
                let _ = AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }
}

/// `key` as a `CFString`.
fn cf_key(key: &CStr) -> CFRetained<CFString> {
    CFString::from_str(key.to_str().unwrap_or_default())
}

/// A dictionary from `pairs`.
fn dictionary(pairs: &[(&CStr, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys: Vec<CFRetained<CFString>> = pairs.iter().map(|(k, _)| cf_key(k)).collect();
    let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&CFType> = pairs.iter().map(|(_, v)| *v).collect();
    CFDictionary::from_slices(&keys, &values)
}

/// JStream's own process object, to leave out of a tap, if Core Audio knows it yet.
fn own_process() -> Result<Option<AudioObjectID>, PlatformError> {
    let pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    hal::process_object(pid)
}

/// The tap's description, and what it records from.
fn describe(kind: &TapKind) -> Result<(Retained<CATapDescription>, Source), PlatformError> {
    let objects = |ids: &[AudioObjectID]| {
        let numbers: Vec<Retained<NSNumber>> = ids.iter().map(|&o| NSNumber::new_u32(o)).collect();
        NSArray::from_retained_slice(&numbers)
    };
    let own: Vec<AudioObjectID> = own_process()?.into_iter().collect();
    Ok(match kind {
        TapKind::Desktop => {
            // SAFETY: an `init` method on a fresh allocation, with an array of process
            // object IDs as `NSNumber`s, as the header documents.
            let d = unsafe {
                CATapDescription::initStereoGlobalTapButExcludeProcesses(
                    CATapDescription::alloc(),
                    &objects(&own),
                )
            };
            (d, Source::Everything)
        }
        TapKind::Output(id) => {
            let device = hal::find(id, AudioDirection::Output)?;
            // SAFETY: as above, with the device's UID and its first output stream.
            let d = unsafe {
                CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
                    CATapDescription::alloc(),
                    &objects(&own),
                    &NSString::from_str(id.as_str()),
                    0,
                )
            };
            (d, Source::Device(device))
        }
        TapKind::Process(pid) => {
            let object = i32::try_from(*pid)
                .ok()
                .map(hal::process_object)
                .transpose()?
                .flatten()
                .ok_or_else(|| PlatformError::NotFound {
                    kind: "audio process",
                    id: pid.to_string(),
                })?;
            // SAFETY: as above, with the one process object.
            let d = unsafe {
                CATapDescription::initStereoMixdownOfProcesses(
                    CATapDescription::alloc(),
                    &objects(&[object]),
                )
            };
            (d, Source::Process(object))
        }
    })
}

impl Tap {
    /// Makes the tap and its aggregate device.
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] for an output or process that isn't there, or no
    /// default output to clock the aggregate by, and [`PlatformError::Os`] when Core
    /// Audio refuses the tap or the aggregate device.
    pub(super) fn create(kind: &TapKind) -> Result<Self, PlatformError> {
        let (description, source) = describe(kind)?;
        // SAFETY: plain setters on the description this function owns. A stereo mixdown
        // whatever the tapped stream's layout, so the tap's channels are always two.
        unsafe {
            description.setPrivate(true);
            description.setMixdown(true);
            description.setMono(false);
            description.setName(&NSString::from_str("JStream"));
        }
        let mut tap = Self {
            tap: kAudioObjectUnknown,
            aggregate: kAudioObjectUnknown,
            source,
        };
        // SAFETY: `description` is a valid tap description; `tap.tap` is a valid out-slot.
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &raw mut tap.tap) };
        if status != NO_ERR {
            return Err(os_status(
                "AudioHardwareCreateProcessTap",
                status,
                &format!("tapping {}", kind.what()),
            ));
        }
        // SAFETY: a getter on the description.
        let tap_uid = unsafe { description.UUID() }.UUIDString().to_string();
        let main_uid = match kind {
            TapKind::Output(id) => id.as_str().to_owned(),
            TapKind::Desktop | TapKind::Process(_) => {
                let output = hal::default_device(AudioDirection::Output)?.ok_or_else(|| {
                    PlatformError::NotFound {
                        kind: "audio device",
                        id: "the default output, which a tap is clocked by".into(),
                    }
                })?;
                hal::uid(output)?
            }
        };
        let spec = aggregate_spec(&tap_uid, &main_uid);
        let mut aggregate = kAudioObjectUnknown;
        // SAFETY: `spec` is a dictionary of the documented aggregate-device keys, with
        // `CFString` keys; `aggregate` is a valid out-slot.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(spec.as_ref(), NonNull::from(&mut aggregate))
        };
        if status != NO_ERR {
            // `tap` drops here, destroying the tap.
            return Err(os_status(
                "AudioHardwareCreateAggregateDevice",
                status,
                &format!("making the device that records {}", kind.what()),
            ));
        }
        tap.aggregate = aggregate;
        Ok(tap)
    }

    /// The aggregate device to run the IOProc on.
    pub(super) fn device(&self) -> AudioObjectID {
        self.aggregate
    }

    /// How to record the aggregate device now.
    pub(super) fn plan(&self) -> Result<StreamPlan, PlatformError> {
        hal::plan_tap(self.aggregate)
    }

    /// Why the stream should end, if what it taps has gone: the output device was
    /// removed, or the process exited.
    pub(super) fn gone(&self) -> Option<&'static str> {
        match self.source {
            Source::Everything => None,
            Source::Device(d) => {
                (!hal::is_alive(d)).then_some("the output device was removed or stopped working")
            }
            Source::Process(p) => hal::process_pid(p)
                .is_none()
                .then_some("the process exited"),
        }
    }
}

/// The aggregate device's description: private, not stacked, clocked by `main_uid`, with
/// the tap `tap_uid` (drift-compensated, started with the device).
fn aggregate_spec(tap_uid: &str, main_uid: &str) -> CFRetained<CFDictionary<CFString, CFType>> {
    let yes = CFNumber::new_i32(1);
    let no = CFNumber::new_i32(0);
    let name = CFString::from_str("JStream tap");
    let uid = CFString::from_str(&format!("{TAP_UID_PREFIX}{tap_uid}"));
    let main = CFString::from_str(main_uid);
    let tap = CFString::from_str(tap_uid);
    let sub_device = dictionary(&[(kAudioSubDeviceUIDKey, main.as_ref())]);
    let sub_tap = dictionary(&[
        (kAudioSubTapUIDKey, tap.as_ref()),
        (kAudioSubTapDriftCompensationKey, yes.as_ref()),
    ]);
    let sub_devices = CFArray::from_retained_objects(&[sub_device]);
    let sub_taps = CFArray::from_retained_objects(&[sub_tap]);
    dictionary(&[
        (kAudioAggregateDeviceNameKey, name.as_ref()),
        (kAudioAggregateDeviceUIDKey, uid.as_ref()),
        (kAudioAggregateDeviceMainSubDeviceKey, main.as_ref()),
        (kAudioAggregateDeviceIsPrivateKey, yes.as_ref()),
        (kAudioAggregateDeviceIsStackedKey, no.as_ref()),
        (kAudioAggregateDeviceTapAutoStartKey, yes.as_ref()),
        (kAudioAggregateDeviceSubDeviceListKey, sub_devices.as_ref()),
        (kAudioAggregateDeviceTapListKey, sub_taps.as_ref()),
    ])
}
