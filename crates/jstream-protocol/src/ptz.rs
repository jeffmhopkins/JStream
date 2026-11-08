//! PTZ cameras (ADR-0138, #1182): the shots a scene frames its physical heads with, and
//! the types the `ptz.*` actions and `ptz.list` carry.
//!
//! A head is named by the name the host's PTZ camera list gives it: the list, with each
//! head's address, belongs to the machine (ADR-0123 §4), so the protocol never carries
//! an address in. Positions and speeds are the head's own VISCA counts and steps
//! (ADR-0138 §5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most shots one scene holds.
pub const MAX_PTZ_SHOTS: usize = 16;
/// The highest preset number a shot or a preset action names (VISCA's `pp`, 0 to 127).
pub const MAX_PTZ_PRESET: u8 = 127;
/// The fastest pan step, VISCA's `18h`.
pub const MAX_PTZ_PAN_SPEED: u8 = 24;
/// The fastest tilt step, VISCA's `17h`.
pub const MAX_PTZ_TILT_SPEED: u8 = 23;
/// The fastest zoom or focus step: VISCA's variable speeds 0 to 7, as 1 to 8.
pub const MAX_PTZ_ZOOM_SPEED: u8 = 8;
/// The longest camera name, in bytes.
pub const MAX_PTZ_CAMERA_NAME: usize = 64;
/// The largest pan count: five signed nibbles (Sony's width).
pub const MAX_PTZ_PAN: i32 = 0x7_FFFF;
/// The largest tilt count: four signed nibbles.
pub const MAX_PTZ_TILT: i32 = 0x7FFF;

/// A head's pose, or part of one, in the head's own counts. Pan and tilt go together,
/// since VISCA moves them in one command; zoom and focus are each optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtzPose {
    /// The pan position, signed, -524287 to 524287. Set together with `tilt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<i32>,
    /// The tilt position, signed, -32767 to 32767. Set together with `pan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<i32>,
    /// The zoom position, 0 the widest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<u16>,
    /// The focus position. Setting it puts the head's focus in manual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<u16>,
}

impl PtzPose {
    /// Whether it sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == PtzPose::default()
    }

    /// The first field that isn't valid, and why: pan without tilt or tilt without pan,
    /// or one out of its range. An empty pose is valid here; a shot and `ptz.aim` refuse
    /// one themselves.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        match (self.pan, self.tilt) {
            (Some(_), None) => return Some(("tilt", "is set together with `pan`".into())),
            (None, Some(_)) => return Some(("pan", "is set together with `tilt`".into())),
            _ => {}
        }
        if self
            .pan
            .is_some_and(|p| !(-MAX_PTZ_PAN..=MAX_PTZ_PAN).contains(&p))
        {
            return Some((
                "pan",
                format!("must be within -{MAX_PTZ_PAN}..={MAX_PTZ_PAN}"),
            ));
        }
        if self
            .tilt
            .is_some_and(|t| !(-MAX_PTZ_TILT..=MAX_PTZ_TILT).contains(&t))
        {
            return Some((
                "tilt",
                format!("must be within -{MAX_PTZ_TILT}..={MAX_PTZ_TILT}"),
            ));
        }
        None
    }
}

/// One shot of a scene: a camera, and the preset it recalls or the pose it moves to
/// (ADR-0138 §6). Exactly one of `preset` and a pose is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtzShot {
    /// The camera's name on the machine's PTZ camera list.
    pub camera: String,
    /// The preset to recall, 0 to 127.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<u8>,
    /// The pose to move to, when there is no preset.
    #[serde(flatten)]
    pub pose: PtzPose,
}

impl PtzShot {
    /// The first field that isn't valid, and why.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        if let Some(reason) = camera_name_problem(&self.camera) {
            return Some(("camera", reason));
        }
        match (self.preset, self.pose.is_empty()) {
            (Some(_), false) => Some((
                "preset",
                "a shot recalls a preset or moves to a pose, not both".into(),
            )),
            (None, true) => Some((
                "preset",
                "a shot needs a preset, or a pose (pan and tilt, zoom, focus)".into(),
            )),
            (Some(p), true) if p > MAX_PTZ_PRESET => {
                Some(("preset", format!("must be within 0..={MAX_PTZ_PRESET}")))
            }
            _ => self.pose.problem(),
        }
    }
}

/// Why `name` can't name a PTZ camera, or `None` when it can: 1 to 64 bytes, with no
/// control characters.
pub fn camera_name_problem(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > MAX_PTZ_CAMERA_NAME {
        Some(format!("must be 1 to {MAX_PTZ_CAMERA_NAME} bytes"))
    } else if name.chars().any(char::is_control) {
        Some("must not hold a control character".into())
    } else {
        None
    }
}

/// Why `shots` can't be a scene's shots, as a field path and a reason: more than
/// [`MAX_PTZ_SHOTS`], one camera twice, or a shot that isn't valid.
pub fn check_ptz_shots(shots: &[PtzShot]) -> Result<(), (String, String)> {
    if shots.len() > MAX_PTZ_SHOTS {
        return Err((
            "shots".into(),
            format!("a scene holds at most {MAX_PTZ_SHOTS} shots"),
        ));
    }
    for (i, shot) in shots.iter().enumerate() {
        if let Some((field, reason)) = shot.problem() {
            return Err((format!("shots[{i}].{field}"), reason));
        }
        if shots[..i].iter().any(|s| s.camera == shot.camera) {
            return Err((
                format!("shots[{i}].camera"),
                format!("camera `{}` has a shot already", shot.camera),
            ));
        }
    }
    Ok(())
}

/// The PTZ camera a `ptz.*` action names, and the first of its fields that isn't
/// valid, with why. `None` for any other action. The engine and the mock check in this
/// order: these fields, then whether the machine lists any camera (`action_failed`),
/// then whether it lists this one (`invalid_argument` on `camera`).
pub fn ptz_action_check(action: &crate::Action) -> Option<(&str, Option<(&'static str, String)>)> {
    use crate::Action;
    let speed = |field, v: i8, max: u8| {
        (v.unsigned_abs() > max).then(|| (field, format!("must be within -{max}..={max}")))
    };
    let (camera, problem) = match action {
        Action::PtzDrive {
            camera,
            pan,
            tilt,
            zoom,
        } => (
            camera,
            speed("pan", *pan, MAX_PTZ_PAN_SPEED)
                .or_else(|| speed("tilt", *tilt, MAX_PTZ_TILT_SPEED))
                .or_else(|| speed("zoom", *zoom, MAX_PTZ_ZOOM_SPEED)),
        ),
        Action::PtzAim {
            camera,
            pose,
            speed,
        } => (
            camera,
            if pose.is_empty() {
                Some(("pan", "an aim needs pan and tilt, zoom or focus".into()))
            } else {
                pose.problem().or_else(|| {
                    speed
                        .is_some_and(|s| !(1..=MAX_PTZ_PAN_SPEED).contains(&s))
                        .then(|| ("speed", format!("must be within 1..={MAX_PTZ_PAN_SPEED}")))
                })
            },
        ),
        Action::PtzHome { camera } => (camera, None),
        Action::PtzRecall { camera, preset } | Action::PtzStore { camera, preset } => (
            camera,
            (*preset > MAX_PTZ_PRESET)
                .then(|| ("preset", format!("must be within 0..={MAX_PTZ_PRESET}"))),
        ),
        Action::PtzFocus { camera, speed, .. } => (
            camera,
            speed
                .is_some_and(|s| !(1..=MAX_PTZ_ZOOM_SPEED).contains(&s))
                .then(|| ("speed", format!("must be within 1..={MAX_PTZ_ZOOM_SPEED}"))),
        ),
        _ => return None,
    };
    let problem = camera_name_problem(camera)
        .map(|reason| ("camera", reason))
        .or(problem);
    Some((camera.as_str(), problem))
}

/// What `ptz.focus` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PtzFocus {
    /// Autofocus on.
    Auto,
    /// Autofocus off: the focus stays where it is.
    Manual,
    /// Focus once, automatically, then hold.
    OnePush,
    /// Drive the focus nearer at `speed`, until `stop`.
    Near,
    /// Drive the focus farther at `speed`, until `stop`.
    Far,
    /// Stop a focus drive.
    Stop,
}

/// How a PTZ camera on the machine's list is reached (ADR-0138 §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PtzTransport {
    /// Sony-style VISCA over IP: UDP with the 8-byte header (port 52381 by default).
    ViscaIp,
    /// Raw VISCA messages over UDP.
    Udp,
    /// Raw VISCA messages over TCP, which also reaches a serial head through an
    /// IP-to-serial bridge.
    Tcp,
}

impl PtzTransport {
    /// Its name on the wire and on the daemon's command line.
    pub fn name(self) -> &'static str {
        match self {
            PtzTransport::ViscaIp => "visca_ip",
            PtzTransport::Udp => "udp",
            PtzTransport::Tcp => "tcp",
        }
    }
}

/// One PTZ camera in a `ptz.list` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtzCameraInfo {
    /// Its name, which `ptz.*` actions and shots take.
    pub camera: String,
    /// The LAN address and port it is reached at, as `ip:port`.
    pub address: String,
    /// How it is reached.
    pub transport: PtzTransport,
    /// Its VISCA address on that link, 1 to 7.
    pub visca_address: u8,
    /// Whether it answered in the last three seconds.
    pub reachable: bool,
    /// Its pose when it last answered the engine's position inquiries: the fields it
    /// answered. Omitted until it has answered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<PtzPose>,
    /// Why its last command failed, as the head answered or the engine saw it. Omitted
    /// while the last one succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Messages sent to it.
    pub sent: u64,
    /// Replies read from it.
    pub replies: u64,
    /// Datagrams dropped: from another address or port, or not a VISCA reply
    /// (ADR-0123 §4).
    pub dropped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(camera: &str, preset: Option<u8>, pose: PtzPose) -> PtzShot {
        PtzShot {
            camera: camera.into(),
            preset,
            pose,
        }
    }

    #[test]
    fn a_shot_takes_a_preset_or_a_pose() {
        let pose = PtzPose {
            pan: Some(10),
            tilt: Some(-4),
            ..PtzPose::default()
        };
        assert_eq!(shot("wide", Some(3), PtzPose::default()).problem(), None);
        assert_eq!(shot("wide", None, pose).problem(), None);
        assert_eq!(
            shot("wide", Some(3), pose).problem().map(|p| p.0),
            Some("preset")
        );
        assert_eq!(
            shot("wide", None, PtzPose::default())
                .problem()
                .map(|p| p.0),
            Some("preset")
        );
        assert_eq!(
            shot("wide", Some(128), PtzPose::default())
                .problem()
                .map(|p| p.0),
            Some("preset")
        );
        assert_eq!(
            shot("", Some(1), PtzPose::default()).problem().map(|p| p.0),
            Some("camera")
        );
        let half = PtzPose {
            pan: Some(1),
            ..PtzPose::default()
        };
        assert_eq!(shot("a", None, half).problem().map(|p| p.0), Some("tilt"));
        let far = PtzPose {
            pan: Some(0),
            tilt: Some(0x8000),
            ..PtzPose::default()
        };
        assert_eq!(shot("a", None, far).problem().map(|p| p.0), Some("tilt"));
    }

    #[test]
    fn a_scene_names_each_camera_once() {
        let one = shot("wide", Some(1), PtzPose::default());
        assert!(check_ptz_shots(std::slice::from_ref(&one)).is_ok());
        assert_eq!(
            check_ptz_shots(&[one.clone(), one]).unwrap_err().0,
            "shots[1].camera"
        );
        let many: Vec<_> = (0..=MAX_PTZ_SHOTS)
            .map(|i| shot(&format!("c{i}"), Some(1), PtzPose::default()))
            .collect();
        assert_eq!(check_ptz_shots(&many).unwrap_err().0, "shots");
    }

    #[test]
    fn a_shot_reads_flat() {
        let parsed: PtzShot =
            serde_json::from_str(r#"{"camera":"wide","pan":5,"tilt":-2,"zoom":100}"#).unwrap();
        assert_eq!(parsed.pose.zoom, Some(100));
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            r#"{"camera":"wide","pan":5,"tilt":-2,"zoom":100}"#
        );
    }
}
