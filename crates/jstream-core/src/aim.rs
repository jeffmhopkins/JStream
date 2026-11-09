//! A scene camera's live aim (ADR-0151, #1181): the pan, tilt and zoom that
//! `scene.drive_camera` and `scene.aim_camera` give a scene family's camera at runtime,
//! drawn on top of the camera the scene stores and whatever moves it (ADR-0113).
//!
//! The engine keeps one [`Aim`] per scene family and publishes them on the render
//! parameter channel ([`LiveParams`](crate::snapshot)). An aim holds no velocity the
//! render thread integrates: each axis is a value at a time and how it moves from there
//! (still, a drive at a fixed rate, or a glide to a value by a time), so the render
//! thread, the snapshot thread and the engine all compute the same aim for any time,
//! and `scene.camera_aim` answers without asking the render thread.

use jstream_media::MediaTime;
use jstream_protocol::{
    CameraAim, CameraAimInfo, ClientId, Finite32, MAX_AIM_PAN_DEG, MAX_AIM_TILT_DEG, MAX_AIM_ZOOM,
    MAX_PTZ_PAN_SPEED, MAX_PTZ_TILT_SPEED, MAX_PTZ_ZOOM_SPEED, SceneCamera, Vec3,
};

/// How far a view may point from level, in degrees: never quite straight up or down.
const MAX_PITCH_DEG: f64 = 89.0;

/// The slowest and fastest pan or tilt rate, in degrees a second, at step 1 and the top
/// step. Between them the rate grows with the square of the step, so the low steps are
/// fine and the top ones sweep, as a joystick's feel on a real head.
const MIN_TURN_DEG_S: f64 = 1.0;
const MAX_TURN_DEG_S: f64 = 80.0;
/// The slowest and fastest zoom rate, in natural-log zoom a second: the whole 1 to 8
/// range takes about 21 s at step 1 and 2 s at step 8.
const MIN_ZOOM_RATE: f64 = 0.1;
const MAX_ZOOM_RATE: f64 = 1.0;

/// The rate of step `step` (1 to `max`, sign ignored) on a curve from `lo` to `hi`.
fn curve(step: u8, max: u8, lo: f64, hi: f64) -> f64 {
    let step = f64::from(step.clamp(1, max));
    let k = (step - 1.0) / (f64::from(max) - 1.0);
    lo + (hi - lo) * k * k
}

/// A pan drive step's rate in degrees a second, signed as the step.
pub(crate) fn pan_rate(step: i8) -> f64 {
    signed(
        step,
        curve(
            step.unsigned_abs(),
            MAX_PTZ_PAN_SPEED,
            MIN_TURN_DEG_S,
            MAX_TURN_DEG_S,
        ),
    )
}

/// A tilt drive step's rate in degrees a second, signed as the step.
pub(crate) fn tilt_rate(step: i8) -> f64 {
    signed(
        step,
        curve(
            step.unsigned_abs(),
            MAX_PTZ_TILT_SPEED,
            MIN_TURN_DEG_S,
            MAX_TURN_DEG_S,
        ),
    )
}

/// A zoom drive step's rate in natural-log zoom a second, signed as the step.
pub(crate) fn zoom_rate(step: i8) -> f64 {
    signed(
        step,
        curve(
            step.unsigned_abs(),
            MAX_PTZ_ZOOM_SPEED,
            MIN_ZOOM_RATE,
            MAX_ZOOM_RATE,
        ),
    )
}

fn signed(step: i8, rate: f64) -> f64 {
    match step.signum() {
        0 => 0.0,
        s => f64::from(s) * rate,
    }
}

/// How one axis moves from its value at its time.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
    Still,
    /// At a fixed rate, in the axis's units a second, until it meets its limit.
    Drive(f64),
    /// To a value, eased, over this many seconds.
    Glide {
        to: f64,
        secs: f64,
    },
}

/// One axis of an aim: pan or tilt in degrees from the stored camera's view, or zoom as
/// its natural log, so 0 is the stored camera on every axis.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Axis {
    /// When `value` held, in seconds on the engine's animation clock.
    since: f64,
    value: f64,
    motion: Motion,
    /// The axis's range.
    lo: f64,
    hi: f64,
}

impl Axis {
    fn new(lo: f64, hi: f64) -> Self {
        Self {
            since: 0.0,
            value: 0.0,
            motion: Motion::Still,
            lo,
            hi,
        }
    }

    /// The axis's value at `at` seconds.
    fn at(&self, at: f64) -> f64 {
        let dt = (at - self.since).max(0.0);
        match self.motion {
            Motion::Still => self.value,
            Motion::Drive(rate) => (self.value + rate * dt).clamp(self.lo, self.hi),
            Motion::Glide { to, secs } => {
                if secs <= 0.0 || dt >= secs {
                    to
                } else {
                    let k = dt / secs;
                    let eased = k * k * (3.0 - 2.0 * k);
                    self.value + (to - self.value) * eased
                }
            }
        }
    }

    /// Whether it is still moving at `at`: a glide not yet there, or a drive not yet
    /// held at the limit it drives toward.
    fn moving(&self, at: f64) -> bool {
        let dt = (at - self.since).max(0.0);
        match self.motion {
            Motion::Still => false,
            Motion::Drive(rate) => {
                let v = self.at(at);
                (rate > 0.0 && v < self.hi) || (rate < 0.0 && v > self.lo)
            }
            Motion::Glide { secs, .. } => dt < secs,
        }
    }

    /// Holds the axis where it is at `at`, so a new motion starts from there.
    fn hold(&mut self, at: f64) {
        self.value = self.at(at);
        self.since = at;
        self.motion = Motion::Still;
    }

    fn drive(&mut self, at: f64, rate: f64) {
        self.hold(at);
        if rate != 0.0 {
            self.motion = Motion::Drive(rate);
        }
    }

    fn glide(&mut self, at: f64, to: f64, secs: f64) {
        self.hold(at);
        let to = to.clamp(self.lo, self.hi);
        self.motion = if secs > 0.0 && to != self.value {
            Motion::Glide { to, secs }
        } else {
            self.value = to;
            Motion::Still
        };
    }

    fn driving(&self) -> bool {
        matches!(self.motion, Motion::Drive(_))
    }
}

/// A scene family's live aim.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Aim {
    pan: Axis,
    tilt: Axis,
    zoom: Axis,
    /// The session whose drive moves it, if one does: when that session ends, the aim
    /// stops where it is (ADR-0151, as ADR-0146 §3 for a head).
    driver: Option<ClientId>,
}

impl Default for Aim {
    fn default() -> Self {
        let pan = f64::from(MAX_AIM_PAN_DEG);
        let tilt = f64::from(MAX_AIM_TILT_DEG);
        Self {
            pan: Axis::new(-pan, pan),
            tilt: Axis::new(-tilt, tilt),
            zoom: Axis::new(0.0, f64::from(MAX_AIM_ZOOM).ln()),
            driver: None,
        }
    }
}

fn secs(at: MediaTime) -> f64 {
    at.as_secs_f64()
}

impl Aim {
    /// Drives it from `at` at the signed VISCA steps given, 0 holding an axis and `None`
    /// leaving it as it was, for `client`'s session: the session whose drive last set
    /// an axis moving owns what moves.
    pub(crate) fn drive(
        &mut self,
        at: MediaTime,
        (pan, tilt, zoom): (Option<i8>, Option<i8>, Option<i8>),
        client: &ClientId,
    ) {
        let at = secs(at);
        let mut started = false;
        for (axis, rate) in [
            (&mut self.pan, pan.map(pan_rate)),
            (&mut self.tilt, tilt.map(tilt_rate)),
            (&mut self.zoom, zoom.map(zoom_rate)),
        ] {
            if let Some(rate) = rate {
                axis.drive(at, rate);
                started |= rate != 0.0;
            }
        }
        if started {
            self.driver = Some(client.clone());
        } else if !self.driving() {
            self.driver = None;
        }
    }

    /// Glides it from `at` to `aim`'s axes at `speed` (1 to 24, the fastest when
    /// `None`), all of them arriving together. The axes `aim` leaves out keep moving as
    /// they were.
    pub(crate) fn aim(&mut self, at: MediaTime, aim: &CameraAim, speed: Option<u8>) {
        let at = secs(at);
        let step = speed.unwrap_or(MAX_PTZ_PAN_SPEED);
        let turn = curve(step, MAX_PTZ_PAN_SPEED, MIN_TURN_DEG_S, MAX_TURN_DEG_S);
        // The zoom step that matches: 1 to 24 laid over 1 to 8.
        let zoom_step = 1
            + (u16::from(step.clamp(1, MAX_PTZ_PAN_SPEED)) - 1) * u16::from(MAX_PTZ_ZOOM_SPEED - 1)
                / u16::from(MAX_PTZ_PAN_SPEED - 1);
        let zoom_rate = curve(
            u8::try_from(zoom_step).unwrap_or(MAX_PTZ_ZOOM_SPEED),
            MAX_PTZ_ZOOM_SPEED,
            MIN_ZOOM_RATE,
            MAX_ZOOM_RATE,
        );
        let get = |v: Option<Finite32>| v.map(|v| f64::from(v.get()));
        let to_pan = get(aim.pan_deg);
        let to_tilt = get(aim.tilt_deg);
        let to_zoom = get(aim.zoom).map(|z| z.max(1.0).ln());
        let far = |axis: &Axis, to: Option<f64>| {
            to.map_or(0.0, |to| (to.clamp(axis.lo, axis.hi) - axis.at(at)).abs())
        };
        let secs = (far(&self.pan, to_pan) / turn)
            .max(far(&self.tilt, to_tilt) / turn)
            .max(far(&self.zoom, to_zoom) / zoom_rate);
        for (axis, to) in [
            (&mut self.pan, to_pan),
            (&mut self.tilt, to_tilt),
            (&mut self.zoom, to_zoom),
        ] {
            if let Some(to) = to {
                axis.glide(at, to, secs);
            }
        }
        if !self.driving() {
            self.driver = None;
        }
    }

    /// Stops it where it is at `at` if `client`'s drive moves it: that session ended.
    /// Returns whether it stopped.
    pub(crate) fn forget(&mut self, at: MediaTime, client: &ClientId) -> bool {
        if self.driver.as_ref() != Some(client) {
            return false;
        }
        let at = secs(at);
        for axis in [&mut self.pan, &mut self.tilt, &mut self.zoom] {
            if axis.driving() {
                axis.hold(at);
            }
        }
        self.driver = None;
        true
    }

    fn driving(&self) -> bool {
        self.pan.driving() || self.tilt.driving() || self.zoom.driving()
    }

    /// Whether a drive or a glide was its last motion on any axis, so a picture drawn
    /// through it may change with time. It says so until the next drive or aim, even
    /// once the glide is there or the drive meets its limit, since it reads no clock.
    pub(crate) fn moves(&self) -> bool {
        [&self.pan, &self.tilt, &self.zoom]
            .iter()
            .any(|axis| axis.motion != Motion::Still)
    }

    /// Its pan and tilt in degrees and its zoom factor at `at`.
    pub(crate) fn at(&self, at: MediaTime) -> (f64, f64, f64) {
        let at = secs(at);
        (self.pan.at(at), self.tilt.at(at), self.zoom.at(at).exp())
    }

    /// What `scene.camera_aim` answers at `at`.
    pub(crate) fn info(&self, at: MediaTime) -> CameraAimInfo {
        let (pan, tilt, zoom) = self.at(at);
        let s = secs(at);
        let finite = |v: f64| Finite32::new(v as f32).unwrap_or(Finite32::ZERO);
        CameraAimInfo {
            pan_deg: finite(pan),
            tilt_deg: finite(tilt),
            zoom: Finite32::new(zoom as f32).unwrap_or(Finite32::ONE),
            moving: self.pan.moving(s) || self.tilt.moving(s) || self.zoom.moving(s),
        }
    }

    /// `camera` turned and zoomed by this aim at `at`: its target turned about its
    /// position by the pan (about the vertical) and the tilt (toward up or down, the
    /// view held within 89° of level), and its field of view narrowed by the zoom.
    pub(crate) fn apply(&self, camera: SceneCamera, at: MediaTime) -> SceneCamera {
        let (pan, tilt, zoom) = self.at(at);
        turn(camera, (pan, tilt, zoom))
    }
}

/// `camera` turned by `pan` and `tilt` degrees and zoomed by `zoom`. The home aim
/// (0, 0, 1) leaves it exactly as it is.
pub(crate) fn turn(camera: SceneCamera, (pan, tilt, zoom): (f64, f64, f64)) -> SceneCamera {
    if pan == 0.0 && tilt == 0.0 && zoom == 1.0 {
        return camera;
    }
    let mut out = camera;
    if pan != 0.0 || tilt != 0.0 {
        let p = [
            f64::from(camera.position.x.get()),
            f64::from(camera.position.y.get()),
            f64::from(camera.position.z.get()),
        ];
        let t = [
            f64::from(camera.target.x.get()),
            f64::from(camera.target.y.get()),
            f64::from(camera.target.z.get()),
        ];
        // Scene space is x right, y down, z toward the viewer: a camera looking at the
        // canvas looks along -z, and up is -y.
        let d = [t[0] - p[0], t[1] - p[1], t[2] - p[2]];
        let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let flat = (d[0] * d[0] + d[2] * d[2]).sqrt();
        let yaw = d[0].atan2(-d[2]) + pan.to_radians();
        let pitch = ((-d[1]).atan2(flat).to_degrees() + tilt)
            .clamp(-MAX_PITCH_DEG, MAX_PITCH_DEG)
            .to_radians();
        let dir = [
            pitch.cos() * yaw.sin(),
            -pitch.sin(),
            -pitch.cos() * yaw.cos(),
        ];
        if let Some(target) = Vec3::new(
            (p[0] + dir[0] * length) as f32,
            (p[1] + dir[1] * length) as f32,
            (p[2] + dir[2] * length) as f32,
        ) {
            out.target = target;
        }
    }
    if zoom != 1.0 {
        let half = (f64::from(camera.fov_deg.get()) / 2.0).to_radians();
        let fov = (2.0 * (half.tan() / zoom).atan()).to_degrees();
        let fov = fov.clamp(
            f64::from(jstream_protocol::MIN_FOV_DEG),
            f64::from(jstream_protocol::MAX_FOV_DEG),
        );
        if let Some(fov) = Finite32::new(fov as f32) {
            out.fov_deg = fov;
        }
    }
    out
}

#[cfg(test)]
mod tests;
