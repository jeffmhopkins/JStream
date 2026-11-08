//! Camera controls on V4L2 (#1196): which V4L2 control each [`CameraControlId`] is, and
//! the control that switches its automatic mode, if any. The `ioctl`s are
//! [`Device::query_control`](super::device::Device::query_control) and its siblings; this
//! module only maps and reads, so it is tested without a device.

use jstream_platform::{CameraControlId, CameraControlInfo};

use super::abi::{
    CID_AUTO_WHITE_BALANCE, CID_AUTOGAIN, CID_BACKLIGHT_COMPENSATION, CID_BRIGHTNESS, CID_CONTRAST,
    CID_EXPOSURE_ABSOLUTE, CID_EXPOSURE_AUTO, CID_FOCUS_ABSOLUTE, CID_FOCUS_AUTO, CID_GAIN,
    CID_GAMMA, CID_HUE, CID_HUE_AUTO, CID_IRIS_ABSOLUTE, CID_PAN_ABSOLUTE,
    CID_POWER_LINE_FREQUENCY, CID_SATURATION, CID_SHARPNESS, CID_TILT_ABSOLUTE,
    CID_WHITE_BALANCE_TEMPERATURE, CID_ZOOM_ABSOLUTE, CTRL_FLAG_DISABLED, CTRL_FLAG_READ_ONLY,
    CTRL_TYPE_BOOLEAN, CTRL_TYPE_INTEGER, CTRL_TYPE_MENU, QueryCtrl,
};

/// The control that turns a control's automatic mode on and off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutoControl {
    pub(crate) cid: u32,
    /// The values that mean "automatic", in the order to try them when turning it on
    /// (a UVC camera's exposure offers aperture priority, 3, rather than full auto, 0).
    pub(crate) on: &'static [i32],
    /// The value that means "manual".
    pub(crate) off: i32,
}

/// How one [`CameraControlId`] maps onto V4L2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mapping {
    pub(crate) id: CameraControlId,
    pub(crate) cid: u32,
    pub(crate) auto: Option<AutoControl>,
}

const BOOL_AUTO: &[i32] = &[1];

const fn plain(id: CameraControlId, cid: u32) -> Mapping {
    Mapping {
        id,
        cid,
        auto: None,
    }
}

const fn with_auto(
    id: CameraControlId,
    cid: u32,
    auto_cid: u32,
    on: &'static [i32],
    off: i32,
) -> Mapping {
    Mapping {
        id,
        cid,
        auto: Some(AutoControl {
            cid: auto_cid,
            on,
            off,
        }),
    }
}

/// Every control the backend maps, in [`CameraControlId::ALL`]'s order.
pub(crate) const MAPPINGS: [Mapping; 16] = [
    plain(CameraControlId::Brightness, CID_BRIGHTNESS),
    plain(CameraControlId::Contrast, CID_CONTRAST),
    plain(CameraControlId::Saturation, CID_SATURATION),
    with_auto(CameraControlId::Hue, CID_HUE, CID_HUE_AUTO, BOOL_AUTO, 0),
    plain(CameraControlId::Sharpness, CID_SHARPNESS),
    plain(CameraControlId::Gamma, CID_GAMMA),
    with_auto(CameraControlId::Gain, CID_GAIN, CID_AUTOGAIN, BOOL_AUTO, 0),
    with_auto(
        CameraControlId::WhiteBalance,
        CID_WHITE_BALANCE_TEMPERATURE,
        CID_AUTO_WHITE_BALANCE,
        BOOL_AUTO,
        0,
    ),
    plain(
        CameraControlId::BacklightCompensation,
        CID_BACKLIGHT_COMPENSATION,
    ),
    plain(
        CameraControlId::PowerLineFrequency,
        CID_POWER_LINE_FREQUENCY,
    ),
    with_auto(
        CameraControlId::Exposure,
        CID_EXPOSURE_ABSOLUTE,
        CID_EXPOSURE_AUTO,
        &[3, 0],
        1,
    ),
    with_auto(
        CameraControlId::Focus,
        CID_FOCUS_ABSOLUTE,
        CID_FOCUS_AUTO,
        BOOL_AUTO,
        0,
    ),
    plain(CameraControlId::Zoom, CID_ZOOM_ABSOLUTE),
    plain(CameraControlId::Pan, CID_PAN_ABSOLUTE),
    plain(CameraControlId::Tilt, CID_TILT_ABSOLUTE),
    plain(CameraControlId::Iris, CID_IRIS_ABSOLUTE),
];

/// The mapping of `id`.
pub(crate) fn mapping(id: CameraControlId) -> Option<Mapping> {
    MAPPINGS.into_iter().find(|m| m.id == id)
}

/// Whether a queried control is one the engine can set: implemented, writable, and an
/// integer, a boolean or a menu.
pub(crate) fn usable(q: &QueryCtrl) -> bool {
    q.flags & (CTRL_FLAG_DISABLED | CTRL_FLAG_READ_ONLY) == 0
        && matches!(
            q.type_,
            CTRL_TYPE_INTEGER | CTRL_TYPE_BOOLEAN | CTRL_TYPE_MENU
        )
}

/// The control `m` describes, from its query and value and its automatic mode's value
/// (`None` when the device has no automatic mode for it).
pub(crate) fn info(m: &Mapping, q: &QueryCtrl, value: i32, auto: Option<i32>) -> CameraControlInfo {
    CameraControlInfo {
        id: m.id,
        min: q.minimum,
        max: q.maximum.max(q.minimum),
        step: q.step.max(1),
        default: q.default_value,
        value,
        auto: m.auto.zip(auto).map(|(a, v)| v != a.off),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(type_: u32, flags: u32) -> QueryCtrl {
        QueryCtrl {
            id: CID_WHITE_BALANCE_TEMPERATURE,
            type_,
            name: [0; 32],
            minimum: 2800,
            maximum: 6500,
            step: 0,
            default_value: 4600,
            flags,
            reserved: [0; 2],
        }
    }

    #[test]
    fn every_control_is_mapped_once_in_order() {
        let ids: Vec<_> = MAPPINGS.iter().map(|m| m.id).collect();
        assert_eq!(ids, CameraControlId::ALL);
    }

    #[test]
    fn only_writable_numeric_controls_are_usable() {
        assert!(usable(&query(CTRL_TYPE_INTEGER, 0)));
        assert!(usable(&query(CTRL_TYPE_MENU, 0)));
        assert!(!usable(&query(CTRL_TYPE_INTEGER, CTRL_FLAG_DISABLED)));
        assert!(!usable(&query(CTRL_TYPE_INTEGER, CTRL_FLAG_READ_ONLY)));
        // A string or a button control.
        assert!(!usable(&query(7, 0)));
        assert!(!usable(&query(4, 0)));
    }

    #[test]
    fn a_control_reads_its_range_and_whether_auto_is_on() {
        let wb = mapping(CameraControlId::WhiteBalance).unwrap();
        let read = info(&wb, &query(CTRL_TYPE_INTEGER, 0), 5000, Some(1));
        assert_eq!(
            (
                read.min,
                read.max,
                read.step,
                read.default,
                read.value,
                read.auto
            ),
            (2800, 6500, 1, 4600, 5000, Some(true))
        );
        assert_eq!(
            info(&wb, &query(CTRL_TYPE_INTEGER, 0), 5000, Some(0)).auto,
            Some(false)
        );
        assert_eq!(
            info(&wb, &query(CTRL_TYPE_INTEGER, 0), 5000, None).auto,
            None
        );
        // Exposure's menu: anything but manual (1) is automatic.
        let exposure = mapping(CameraControlId::Exposure).unwrap();
        assert_eq!(
            info(&exposure, &query(CTRL_TYPE_INTEGER, 0), 156, Some(3)).auto,
            Some(true)
        );
        assert_eq!(
            info(&exposure, &query(CTRL_TYPE_INTEGER, 0), 156, Some(1)).auto,
            Some(false)
        );
        // A control with no automatic mode ignores an auto value.
        let brightness = mapping(CameraControlId::Brightness).unwrap();
        assert_eq!(
            info(&brightness, &query(CTRL_TYPE_INTEGER, 0), 1, Some(1)).auto,
            None
        );
    }
}
