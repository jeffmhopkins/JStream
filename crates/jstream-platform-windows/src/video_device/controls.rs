//! Camera controls on Media Foundation (#1196): a device's media source answers
//! DirectShow's `IAMVideoProcAmp` (image settings) and `IAMCameraControl` (lens and
//! exposure), which the kernel-streaming proxy forwards to the driver. Setting one on
//! the media source a running capture reads applies at once, with no reopen.
//!
//! The mapping and the flag logic are plain and tested anywhere; the COM calls and the
//! registry of open sources are Windows-only.

use jstream_platform::{CameraControlId, CameraControlInfo, CameraControlValue};

/// Which interface a control is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Iface {
    /// `IAMVideoProcAmp`.
    ProcAmp,
    /// `IAMCameraControl`.
    Camera,
}

/// Each control's interface and property: `VideoProcAmpProperty` and
/// `CameraControlProperty` values. Power-line frequency is the kernel-streaming
/// property `KSPROPERTY_VIDEOPROCAMP_POWERLINE_FREQUENCY` (13), which the proxy takes
/// through `IAMVideoProcAmp` though the enum stops before it.
pub(crate) const MAP: [(CameraControlId, Iface, i32); 16] = [
    (CameraControlId::Brightness, Iface::ProcAmp, 0),
    (CameraControlId::Contrast, Iface::ProcAmp, 1),
    (CameraControlId::Saturation, Iface::ProcAmp, 3),
    (CameraControlId::Hue, Iface::ProcAmp, 2),
    (CameraControlId::Sharpness, Iface::ProcAmp, 4),
    (CameraControlId::Gamma, Iface::ProcAmp, 5),
    (CameraControlId::Gain, Iface::ProcAmp, 9),
    (CameraControlId::WhiteBalance, Iface::ProcAmp, 7),
    (CameraControlId::BacklightCompensation, Iface::ProcAmp, 8),
    (CameraControlId::PowerLineFrequency, Iface::ProcAmp, 13),
    (CameraControlId::Exposure, Iface::Camera, 4),
    (CameraControlId::Focus, Iface::Camera, 6),
    (CameraControlId::Zoom, Iface::Camera, 3),
    (CameraControlId::Pan, Iface::Camera, 0),
    (CameraControlId::Tilt, Iface::Camera, 1),
    (CameraControlId::Iris, Iface::Camera, 5),
];

/// `VideoProcAmp_Flags_Auto` and `CameraControl_Flags_Auto`.
pub(crate) const FLAG_AUTO: i32 = 1;
/// `VideoProcAmp_Flags_Manual` and `CameraControl_Flags_Manual`.
pub(crate) const FLAG_MANUAL: i32 = 2;

/// What `GetRange` returns: min, max, step, default and the capability flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Range {
    pub(crate) min: i32,
    pub(crate) max: i32,
    pub(crate) step: i32,
    pub(crate) default: i32,
    pub(crate) caps: i32,
}

/// The control from its range and `Get`'s value and flags.
pub(crate) fn info(id: CameraControlId, range: Range, value: i32, flags: i32) -> CameraControlInfo {
    CameraControlInfo {
        id,
        min: range.min,
        max: range.max.max(range.min),
        step: range.step.max(1),
        default: range.default,
        value,
        auto: (range.caps & FLAG_AUTO != 0).then_some(flags & FLAG_AUTO != 0),
    }
}

/// The value and flags to pass to `Set`, or `None` when the control has no automatic
/// mode and `value` asks for it. Auto keeps the current value, which the device ignores.
pub(crate) fn set_args(
    control: &CameraControlInfo,
    value: CameraControlValue,
) -> Option<(i32, i32)> {
    match value {
        CameraControlValue::Manual(v) => Some((control.snap(v), FLAG_MANUAL)),
        CameraControlValue::Auto => control.auto.map(|_| (control.value, FLAG_AUTO)),
    }
}

#[cfg(windows)]
pub(crate) use com::{Controls, Registered, open_source, register};

#[cfg(windows)]
mod com {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, PoisonError};

    use jstream_platform::{CameraControlId, CameraControlInfo, CameraControlValue, PlatformError};
    use windows::Win32::Media::DirectShow::{IAMCameraControl, IAMVideoProcAmp};
    use windows::Win32::Media::MediaFoundation::IMFMediaSource;
    use windows::core::Interface;

    use super::{Iface, MAP, Range, info, set_args};
    use crate::com::OsResult;

    /// A device's control interfaces, from its media source. Either is `None` when the
    /// source doesn't answer it (a capture card often answers neither).
    pub(crate) struct Controls {
        proc_amp: Option<IAMVideoProcAmp>,
        camera: Option<IAMCameraControl>,
    }

    impl Controls {
        pub(crate) fn of(source: &IMFMediaSource) -> Self {
            Self {
                proc_amp: source.cast().ok(),
                camera: source.cast().ok(),
            }
        }

        fn range(&self, iface: Iface, property: i32) -> Option<Range> {
            let (mut min, mut max, mut step, mut default, mut caps) = (0, 0, 0, 0, 0);
            // SAFETY: plain COM calls with valid out-pointers on live interfaces.
            let got = unsafe {
                match iface {
                    Iface::ProcAmp => self.proc_amp.as_ref()?.GetRange(
                        property,
                        &mut min,
                        &mut max,
                        &mut step,
                        &mut default,
                        &mut caps,
                    ),
                    Iface::Camera => self.camera.as_ref()?.GetRange(
                        property,
                        &mut min,
                        &mut max,
                        &mut step,
                        &mut default,
                        &mut caps,
                    ),
                }
            };
            got.ok().map(|()| Range {
                min,
                max,
                step,
                default,
                caps,
            })
        }

        fn get(&self, iface: Iface, property: i32) -> Option<(i32, i32)> {
            let (mut value, mut flags) = (0, 0);
            // SAFETY: plain COM calls with valid out-pointers on live interfaces.
            let got = unsafe {
                match iface {
                    Iface::ProcAmp => self
                        .proc_amp
                        .as_ref()?
                        .Get(property, &mut value, &mut flags),
                    Iface::Camera => self.camera.as_ref()?.Get(property, &mut value, &mut flags),
                }
            };
            got.ok().map(|()| (value, flags))
        }

        fn read(
            &self,
            id: CameraControlId,
            iface: Iface,
            property: i32,
        ) -> Option<CameraControlInfo> {
            let range = self.range(iface, property)?;
            let (value, flags) = self.get(iface, property)?;
            Some(info(id, range, value, flags))
        }

        /// The controls the device answers, in [`MAP`]'s order.
        pub(crate) fn list(&self) -> Vec<CameraControlInfo> {
            MAP.iter()
                .filter_map(|&(id, iface, property)| self.read(id, iface, property))
                .collect()
        }

        /// Sets `id`.
        pub(crate) fn apply(
            &self,
            id: CameraControlId,
            value: CameraControlValue,
            device: &str,
        ) -> Result<(), PlatformError> {
            let unsupported = |what: &str| PlatformError::Unsupported {
                what: format!("{what} {} on video capture device {device:?}", id.name()),
            };
            let &(_, iface, property) = MAP
                .iter()
                .find(|(c, _, _)| *c == id)
                .ok_or_else(|| unsupported("the control"))?;
            let current = self
                .read(id, iface, property)
                .ok_or_else(|| unsupported("the control"))?;
            let (value, flags) =
                set_args(&current, value).ok_or_else(|| unsupported("automatic"))?;
            // SAFETY: plain COM calls on live interfaces; `read` found the one used.
            unsafe {
                match iface {
                    Iface::ProcAmp => self
                        .proc_amp
                        .as_ref()
                        .ok_or_else(|| unsupported("the control"))?
                        .Set(property, value, flags)
                        .os("IAMVideoProcAmp::Set"),
                    Iface::Camera => self
                        .camera
                        .as_ref()
                        .ok_or_else(|| unsupported("the control"))?
                        .Set(property, value, flags)
                        .os("IAMCameraControl::Set"),
                }
            }
        }
    }

    /// A running capture's media source, kept so its controls are set on the source the
    /// capture reads rather than on a second activation of the device, which a camera
    /// that is streaming may refuse.
    struct Source(IMFMediaSource);

    // SAFETY: Media Foundation media sources are free-threaded and were created in the
    // MTA; every thread that touches one here joins the MTA first (`Com::init`).
    unsafe impl Send for Source {}

    /// The media sources of running captures, by device link, each with the token of
    /// the registration that put it there.
    static OPEN: Mutex<BTreeMap<String, (u64, Source)>> = Mutex::new(BTreeMap::new());
    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A running capture's entry in the registry, removed when dropped.
    pub(crate) struct Registered {
        link: String,
        token: u64,
    }

    /// Registers `source` as the running capture of `link`, replacing an earlier one.
    pub(crate) fn register(link: &str, source: &IMFMediaSource) -> Registered {
        let token = NEXT.fetch_add(1, Ordering::Relaxed);
        OPEN.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(link.to_owned(), (token, Source(source.clone())));
        Registered {
            link: link.to_owned(),
            token,
        }
    }

    impl Drop for Registered {
        fn drop(&mut self) {
            let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
            if open.get(&self.link).is_some_and(|(t, _)| *t == self.token) {
                open.remove(&self.link);
            }
        }
    }

    /// The media source a running capture of `link` reads, if any.
    pub(crate) fn open_source(link: &str) -> Option<IMFMediaSource> {
        OPEN.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(link)
            .map(|(_, s)| s.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(caps: i32) -> Range {
        Range {
            min: -11,
            max: -2,
            step: 1,
            default: -6,
            caps,
        }
    }

    #[test]
    fn every_control_is_mapped_once_in_order() {
        let ids: Vec<_> = MAP.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, CameraControlId::ALL);
    }

    #[test]
    fn auto_is_offered_only_when_the_range_says_so() {
        let both = info(
            CameraControlId::Exposure,
            range(FLAG_AUTO | FLAG_MANUAL),
            -5,
            FLAG_AUTO,
        );
        assert_eq!((both.value, both.auto), (-5, Some(true)));
        let manual_now = info(
            CameraControlId::Exposure,
            range(FLAG_AUTO | FLAG_MANUAL),
            -5,
            FLAG_MANUAL,
        );
        assert_eq!(manual_now.auto, Some(false));
        let manual_only = info(
            CameraControlId::Exposure,
            range(FLAG_MANUAL),
            -5,
            FLAG_MANUAL,
        );
        assert_eq!(manual_only.auto, None);
    }

    #[test]
    fn set_snaps_a_value_and_refuses_auto_without_it() {
        let with_auto = info(
            CameraControlId::Exposure,
            range(FLAG_AUTO | FLAG_MANUAL),
            -5,
            FLAG_AUTO,
        );
        assert_eq!(
            set_args(&with_auto, CameraControlValue::Manual(-20)),
            Some((-11, FLAG_MANUAL))
        );
        assert_eq!(
            set_args(&with_auto, CameraControlValue::Auto),
            Some((-5, FLAG_AUTO))
        );
        let manual_only = info(
            CameraControlId::Exposure,
            range(FLAG_MANUAL),
            -5,
            FLAG_MANUAL,
        );
        assert_eq!(set_args(&manual_only, CameraControlValue::Auto), None);
    }
}
