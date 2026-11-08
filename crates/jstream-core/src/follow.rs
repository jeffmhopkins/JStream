//! A scene camera's motion and its follow camera (ADR-0113,
//! docs/adr/0113-camera-motion-and-follow-pointer.md).
//!
//! Camera motion ([`CameraMotion`]) is a pure function of time, as an item's is: each
//! modifier's value is added to one camera property as the camera is drawn.
//!
//! A follow camera ([`Follow`]) is not: it glides after the pointer over a capture item
//! through springs it carries from frame to frame, so the render thread keeps one per
//! scene family ([`Cameras`]) and steps it once a frame. Its algorithm is the follow-cursor
//! mock's, on the item's box at its depth:
//! 1. where the pointer is in the current view decides where to look: inside a dead zone
//!    the look point holds, and past it only the excess moves it;
//! 2. a fast pointer pulls the camera back for a moment;
//! 3. the look point slides back by any overshoot, so the frame never shows past the
//!    box, and the camera moves nearer when a corner is still out of view;
//! 4. the look point glides there through two chained critically damped springs;
//! 5. the camera leans a little with the look point's speed;
//! 6. its distance glides in log space toward the zoom's, never past what fills the frame.
//!
//! # Behaviour
//!
//! **Camera moves** (`follow.rs`,
//! ADR-0113): a 3D scene's
//! camera motion and follow camera, planned with the shot as its pass camera's
//! `CameraMoves` from its family (`CameraMotion`, and a `FollowPlan`: the followed item's
//! box at its committed geometry, its crop, fit and flips, and the capture it shows). As a
//! pass draws, the follow camera glides after the pointer the capture reports
//! (`Captures::pointer`, mapped through the crop and the fit onto the box) through the
//! springs of the follow-cursor mock, keeping the box filling the frame, and focuses where
//! it looks when the camera has a blur; camera motion is then added, clamped to the
//! camera's ranges. The springs live in the render context's `Chains` (`Cameras`, one per
//! family), step once per frame time, and are dropped after 10 s undrawn. A transition
//! moves each end's camera the same way, so a Move into or out of a follow scene follows
//! it. `scene.zoom_camera` adds its steps to the family's count in the engine, which the
//! live-value channel carries (`LiveParams::with_zooms`); a follow camera takes the steps
//! asked for since it started. A shot whose camera moves changes with time.

use std::collections::BTreeMap;
use std::sync::Arc;

use jstream_anim::Modifier;
use jstream_media::MediaTime;
use jstream_platform::PointerPosition;
use jstream_protocol::{
    CameraFollow, CameraProperty, Canvas, CanvasPoint, FOLLOW_ZOOM_RANGE, FOLLOW_ZOOM_STEP,
    Finite32, FitMode, ItemGeometry, MAX_BLUR_PX, MAX_FOV_DEG, MIN_FOV_DEG, Placed, SceneCamera,
    SceneId, Vec3,
};

use crate::snapshot::{CaptureKey, Captures};

/// A scene family's camera motion, as `jstream-anim` evaluates it: each modifier with
/// the property it adds to.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CameraMotion(Arc<[(CameraProperty, Modifier)]>);

impl CameraMotion {
    /// `motion`, compiled.
    pub(crate) fn of(motion: &[jstream_protocol::CameraMotionModifier]) -> Self {
        Self(jstream_graph::camera_anim_modifiers(motion).into())
    }

    /// Whether it changes nothing.
    pub(crate) fn is_still(&self) -> bool {
        self.0.iter().all(|(_, m)| !m.enabled)
    }

    /// `camera` with the motion's values at `at` added, the scene first shown on program
    /// at `shown`. The field of view is held to 1 to 170°, focus above 0 and blur to 0 to
    /// 64, and a camera that would reach its target keeps its unmoved position. Allocates
    /// nothing.
    pub(crate) fn apply(
        &self,
        camera: SceneCamera,
        at: MediaTime,
        shown: Option<MediaTime>,
    ) -> SceneCamera {
        if self.0.is_empty() {
            return camera;
        }
        let engine_s = at.as_secs_f64();
        let shown_s = shown.map(|since| (engine_s - since.as_secs_f64()).max(0.0));
        let mut out = camera;
        let mut position = [0.0_f32; 3];
        let mut target = [0.0_f32; 3];
        for (property, modifier) in self.0.iter() {
            let value = jstream_anim::evaluate(std::slice::from_ref(modifier), engine_s, shown_s).x;
            let value = value as f32;
            match property {
                CameraProperty::PositionX => position[0] += value,
                CameraProperty::PositionY => position[1] += value,
                CameraProperty::PositionZ => position[2] += value,
                CameraProperty::TargetX => target[0] += value,
                CameraProperty::TargetY => target[1] += value,
                CameraProperty::TargetZ => target[2] += value,
                CameraProperty::Fov => {
                    out.fov_deg = finite(
                        (out.fov_deg.get() + value).clamp(MIN_FOV_DEG, MAX_FOV_DEG),
                        out.fov_deg,
                    );
                }
                CameraProperty::Roll => {
                    out.roll_deg = finite(out.roll_deg.get() + value, out.roll_deg);
                }
                CameraProperty::Focus => {
                    out.focus_px = out.focus_px.map(|f| finite((f.get() + value).max(1.0), f));
                }
                CameraProperty::Blur => {
                    out.blur_px = finite(
                        (out.blur_px.get() + value).clamp(0.0, MAX_BLUR_PX),
                        out.blur_px,
                    );
                }
                _ => {}
            }
        }
        let moved = |v: Vec3, by: [f32; 3]| {
            Vec3::new(v.x.get() + by[0], v.y.get() + by[1], v.z.get() + by[2]).unwrap_or(v)
        };
        let moved_camera = SceneCamera {
            position: moved(out.position, position),
            target: moved(out.target, target),
            ..out
        };
        if moved_camera.validate().is_ok() {
            moved_camera
        } else {
            out
        }
    }
}

fn finite(value: f32, otherwise: Finite32) -> Finite32 {
    Finite32::new(value).unwrap_or(otherwise)
}

/// What moves a 3D scene's camera as it is drawn, beside the camera itself (ADR-0113):
/// its family's motion and follow camera, planned with the shot.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CameraMoves {
    /// The scene family it belongs to: whose follow camera state it steps.
    pub(crate) family: SceneId,
    pub(crate) motion: CameraMotion,
    pub(crate) follow: Option<FollowPlan>,
}

impl CameraMoves {
    /// `base`, the camera the scene draws through, as it is at `at`: glided after the
    /// pointer by the follow camera, then moved by the motion. `shown` is when the scene
    /// started being drawn on program, `zooms` the zoom steps asked for its family.
    pub(crate) fn apply(
        &self,
        base: SceneCamera,
        (at, shown): (MediaTime, Option<MediaTime>),
        canvas: Canvas,
        captures: &dyn Captures,
        cameras: &mut Cameras,
        zooms: i64,
    ) -> SceneCamera {
        let followed = match &self.follow {
            Some(plan) => {
                let pointer = plan.pointer(captures);
                cameras.follow(
                    &self.family,
                    &plan.follow,
                    base,
                    canvas,
                    plan.bx,
                    pointer,
                    (at, zooms),
                )
            }
            None => base,
        };
        self.motion.apply(followed, at, shown)
    }

    /// Whether it changes the camera with time.
    pub(crate) fn moves(&self) -> bool {
        self.follow.is_some() || !self.motion.is_still()
    }
}

/// A follow camera as a shot plans it: what to follow, and how the followed item's
/// capture maps onto its box.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FollowPlan {
    pub(crate) follow: CameraFollow,
    pub(crate) bx: FollowBox,
    /// The item's crop, in the capture's pixels: left, top, right, bottom.
    pub(crate) crop: [u32; 4],
    pub(crate) fit: FitMode,
    pub(crate) flip: (bool, bool),
    /// The capture the item shows; `None` when it shows none, and the camera rests.
    pub(crate) capture: Option<CaptureKey>,
}

impl FollowPlan {
    /// Following `follow` over an item at `geometry`, cropped by `crop`, showing
    /// `capture`. The item is followed as its unrotated box (ADR-0113 §2).
    pub(crate) fn new(
        follow: CameraFollow,
        geometry: &ItemGeometry,
        crop: [u32; 4],
        capture: Option<CaptureKey>,
    ) -> Self {
        Self {
            follow,
            bx: FollowBox {
                center: [geometry.center.x.get(), geometry.center.y.get()],
                size: [geometry.size.width.get(), geometry.size.height.get()],
                depth: geometry.depth_px.get(),
            },
            crop,
            fit: geometry.fit_mode,
            flip: (geometry.flip_h, geometry.flip_v),
            capture,
        }
    }

    /// Where the pointer is on the box, in its pixels, from the capture: `None` when it
    /// gives none, before its first frame, or while the pointer is off what the item
    /// shows.
    fn pointer(&self, captures: &dyn Captures) -> Option<(f32, f32)> {
        let capture = self.capture.as_ref()?;
        let frame = captures.frame(capture)?.size();
        self.on_box(captures.pointer(capture)?, frame)
    }

    /// `pointer`, on a frame `frame` big, through the crop and the fit onto the box.
    fn on_box(&self, pointer: PointerPosition, frame: (f32, f32)) -> Option<(f32, f32)> {
        let [l, t, r, b] = self.crop.map(|v| v as f32);
        let (cw, ch) = (frame.0 - l - r, frame.1 - t - b);
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }
        let (x, y) = (pointer.x - l, pointer.y - t);
        if !(0.0..=cw).contains(&x) || !(0.0..=ch).contains(&y) {
            return None;
        }
        let [bw, bh] = self.bx.size;
        let (sx, sy) = match self.fit {
            FitMode::Stretch => (bw / cw, bh / ch),
            FitMode::Fill => {
                let s = (bw / cw).max(bh / ch);
                (s, s)
            }
            _ => {
                let s = (bw / cw).min(bh / ch);
                (s, s)
            }
        };
        let (ox, oy) = ((bw - cw * sx) * 0.5, (bh - ch * sy) * 0.5);
        let (mut px, mut py) = (x * sx + ox, y * sy + oy);
        if self.flip.0 {
            px = bw - px;
        }
        if self.flip.1 {
            py = bh - py;
        }
        Some((px.clamp(0.0, bw), py.clamp(0.0, bh)))
    }
}

/// Critically damped smoothing toward a target (Game Programming Gems 4, 1.10): no
/// overshoot from rest, the same at any frame rate; `time` is roughly how long it takes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Smooth {
    x: f32,
    v: f32,
}

impl Smooth {
    fn new(x: f32) -> Self {
        Self { x, v: 0.0 }
    }

    fn step(&mut self, target: f32, time: f32, dt: f32) -> f32 {
        let omega = 2.0 / time.max(1e-4);
        let k = omega * dt;
        let e = 1.0 / (1.0 + k + 0.48 * k * k + 0.235 * k * k * k);
        let change = self.x - target;
        let temp = (self.v + omega * change) * dt;
        self.v = (self.v - omega * temp) * e;
        self.x = target + (change + temp) * e;
        self.x
    }
}

/// The followed item's box: its centre on the canvas, its size, and its depth, in scene
/// pixels. The follow camera sees it as this unrotated rectangle on the plane
/// `z = depth`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FollowBox {
    pub(crate) center: [f32; 2],
    pub(crate) size: [f32; 2],
    pub(crate) depth: f32,
}

impl FollowBox {
    /// A point on the box, in its own pixels from its top-left corner, in the scene.
    fn world(&self, p: (f32, f32)) -> Vec3 {
        let x = self.center[0] - self.size[0] * 0.5 + p.0;
        let y = self.center[1] - self.size[1] * 0.5 + p.1;
        Vec3::new(x, y, self.depth).unwrap_or(Vec3 {
            x: Finite32::ZERO,
            y: Finite32::ZERO,
            z: Finite32::ZERO,
        })
    }

    /// A point on the box's plane, in the box's own pixels.
    fn unworld(&self, p: (f32, f32)) -> (f32, f32) {
        (
            p.0 - self.center[0] + self.size[0] * 0.5,
            p.1 - self.center[1] + self.size[1] * 0.5,
        )
    }

    fn middle(&self) -> (f32, f32) {
        (self.size[0] * 0.5, self.size[1] * 0.5)
    }

    /// The box's height relative to the mock's 900-pixel screen: what its distances and
    /// speeds scale by.
    fn scale(&self) -> f32 {
        (self.size[1] / 900.0).max(1e-3)
    }
}

/// How the follow camera is tuned, from a [`CameraFollow`] and its base camera.
#[derive(Clone, Copy, Debug)]
struct Tuning {
    yaw: f32,
    pitch: f32,
    /// The distance at zoom 1: two thirds of the box's height fills the frame.
    dist: f32,
    follow_time: f32,
    dead_zone: f32,
    lean_per_speed: f32,
    lean_max: f32,
    lean_time: f32,
    zoom_time: f32,
    /// What a fast pointer is, and the nearest the camera moves in for a corner.
    fast: f32,
    nearest: f32,
    base: SceneCamera,
    canvas: Canvas,
    bx: FollowBox,
}

impl Tuning {
    fn new(follow: &CameraFollow, base: SceneCamera, canvas: Canvas, bx: FollowBox) -> Self {
        let half_fov = (base.fov_deg.get().to_radians() * 0.5).tan().max(1e-3);
        let scale = bx.scale();
        Self {
            yaw: follow.yaw_deg.get(),
            pitch: follow.pitch_deg.get(),
            dist: bx.size[1] / (3.0 * half_fov),
            follow_time: follow.smooth_ms as f32 / 1000.0,
            dead_zone: 0.35,
            lean_per_speed: follow.lean_deg.get() / 750.0 / scale,
            lean_max: follow.lean_deg.get(),
            lean_time: 0.7,
            zoom_time: 0.35,
            fast: 1200.0 * scale,
            nearest: 250.0 * scale,
            base,
            canvas,
            bx,
        }
    }

    fn camera(&self, aim: (f32, f32), yaw: f32, pitch: f32, dist: f32) -> SceneCamera {
        let target = self.bx.world(aim);
        let (y, p) = (yaw.to_radians(), pitch.to_radians());
        let d = [y.sin() * p.cos(), p.sin(), y.cos() * p.cos()];
        let position = Vec3::new(
            target.x.get() + d[0] * dist,
            target.y.get() + d[1] * dist,
            target.z.get() + d[2] * dist,
        )
        .unwrap_or(self.base.position);
        let base = self.base;
        SceneCamera {
            position,
            target,
            // A little roll into a sideways lean, as a hand-held camera does.
            roll_deg: finite(base.roll_deg.get() - 0.25 * (yaw - self.yaw), base.roll_deg),
            focus_px: if base.blur_px.get() > 0.0 {
                Finite32::new(dist.max(1.0))
            } else {
                base.focus_px
            },
            ..base
        }
    }

    /// The frame's corners and edge midpoints on the box's plane, in its pixels, or
    /// `None` where one misses the plane.
    fn footprint(&self, camera: SceneCamera) -> Option<[(f32, f32); 8]> {
        let view = camera.view(self.canvas).ok()?;
        let (w, h) = (self.canvas.width as f32, self.canvas.height as f32);
        let corners = [
            (0.0, 0.0),
            (w * 0.5, 0.0),
            (w, 0.0),
            (w, h * 0.5),
            (w, h),
            (w * 0.5, h),
            (0.0, h),
            (0.0, h * 0.5),
        ];
        let mut out = [(0.0, 0.0); 8];
        for (o, (x, y)) in out.iter_mut().zip(corners) {
            *o = self
                .bx
                .unworld(view.on_plane(CanvasPoint { x, y }, self.bx.depth)?);
        }
        Some(out)
    }

    fn covers(&self, camera: SceneCamera) -> bool {
        let [bw, bh] = self.bx.size;
        self.footprint(camera).is_some_and(|ps| {
            ps.iter()
                .all(|p| (0.5..bw - 0.5).contains(&p.0) && (0.5..bh - 0.5).contains(&p.1))
        })
    }

    /// The farthest the camera may be, aimed at `aim` at this angle, with the box still
    /// filling the frame; 0 when nowhere does.
    fn max_dist(&self, aim: (f32, f32), yaw: f32, pitch: f32) -> f32 {
        let near = 30.0 * self.bx.scale();
        if !self.covers(self.camera(aim, yaw, pitch, near)) {
            return 0.0;
        }
        let (mut lo, mut hi) = (near, 6.0 * self.dist.max(near));
        for _ in 0..28 {
            let m = 0.5 * (lo + hi);
            if self.covers(self.camera(aim, yaw, pitch, m)) {
                lo = m;
            } else {
                hi = m;
            }
        }
        lo
    }

    /// `aim` slid by just enough that the view from `dist` at this angle stays on the box.
    fn inside(&self, aim: (f32, f32), yaw: f32, pitch: f32, dist: f32) -> (f32, f32) {
        let [bw, bh] = self.bx.size;
        let mut aim = aim;
        for _ in 0..6 {
            let Some(fp) = self.footprint(self.camera(aim, yaw, pitch, dist)) else {
                break;
            };
            let (x0, x1) = fp
                .iter()
                .fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
            let (y0, y1) = fp
                .iter()
                .fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
            let shift = |lo: f32, hi: f32, size: f32, at: f32| -> f32 {
                let m = 1.0;
                if hi - lo > size - 2.0 * m {
                    size * 0.5 - at
                } else if lo < m {
                    m - lo
                } else if hi > size - m {
                    size - m - hi
                } else {
                    0.0
                }
            };
            let (dx, dy) = (shift(x0, x1, bw, aim.0), shift(y0, y1, bh, aim.1));
            if dx.abs() < 0.05 && dy.abs() < 0.05 {
                break;
            }
            aim = (aim.0 + dx, aim.1 + dy);
        }
        aim
    }

    /// Where `p`, in the box's pixels, is in `camera`'s view: -1..1 across and down,
    /// or far outside when it is behind the camera.
    fn seen(&self, camera: SceneCamera, p: (f32, f32)) -> (f32, f32) {
        let Ok(view) = camera.view(self.canvas) else {
            return (9.0, 9.0);
        };
        match view.place(self.bx.world(p)) {
            Placed::At(q) => (
                (q.x / self.canvas.width as f32) * 2.0 - 1.0,
                (q.y / self.canvas.height as f32) * 2.0 - 1.0,
            ),
            _ => (9.0, 9.0),
        }
    }
}

/// One follow camera's state: everything it keeps is smoothed, so its picture is smooth
/// whatever the pointer does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Follow {
    /// The look point, x and y, each through two springs: `[x1, x2, y1, y2]`.
    aim: [Smooth; 4],
    yaw: Smooth,
    pitch: Smooth,
    log_dist: Smooth,
    /// The zoom asked for (`scene.zoom_camera`), and the follow camera it started from.
    zoom: f32,
    follow: CameraFollow,
    /// The pointer's speed, smoothed, in box pixels a second, and where it was.
    speed: Smooth,
    last: (f32, f32),
    /// When it last stepped.
    at: MediaTime,
    /// The zoom steps asked for its family that it has taken.
    zooms: i64,
}

impl Follow {
    /// At rest, looking at `pointer` (the box's centre without one), at `follow`'s zoom.
    pub(crate) fn new(
        follow: &CameraFollow,
        base: SceneCamera,
        canvas: Canvas,
        bx: FollowBox,
        pointer: Option<(f32, f32)>,
        (at, zooms): (MediaTime, i64),
    ) -> Self {
        let t = Tuning::new(follow, base, canvas, bx);
        let zoom = follow.zoom.get();
        // It starts where it would come to rest: the box filling the frame.
        let dist = (t.dist / zoom).min(0.97 * t.max_dist(bx.middle(), t.yaw, t.pitch));
        let dist = dist.max(1.0);
        let last = pointer.unwrap_or_else(|| bx.middle());
        let p = t.inside(last, t.yaw, t.pitch, dist);
        Self {
            aim: [
                Smooth::new(p.0),
                Smooth::new(p.0),
                Smooth::new(p.1),
                Smooth::new(p.1),
            ],
            yaw: Smooth::new(t.yaw),
            pitch: Smooth::new(t.pitch),
            log_dist: Smooth::new(dist.ln()),
            zoom,
            follow: follow.clone(),
            speed: Smooth::new(0.0),
            last,
            at,
            zooms,
        }
    }

    /// Whether it was started from `follow`: a changed follow camera starts again.
    pub(crate) fn is_for(&self, follow: &CameraFollow) -> bool {
        self.follow == *follow
    }

    /// Takes the zoom steps asked for its family up to `zooms`, in (negative: out), each
    /// held to the zoom range.
    fn zoom_to(&mut self, zooms: i64) {
        let (min, max) = FOLLOW_ZOOM_RANGE;
        let steps = (zooms - self.zooms).clamp(-64, 64) as i32;
        self.zooms = zooms;
        self.zoom = (self.zoom * FOLLOW_ZOOM_STEP.powi(steps)).clamp(min, max);
    }

    /// The zoom asked for.
    #[cfg(test)]
    fn zoom(&self) -> f32 {
        self.zoom
    }

    /// The camera at `at`, stepped from the last one by the time between, from where the
    /// pointer is now (in the box's pixels; `None` holds the last place). A second call
    /// for the same `at` steps nothing and gives the same camera.
    pub(crate) fn step(
        &mut self,
        base: SceneCamera,
        canvas: Canvas,
        bx: FollowBox,
        pointer: Option<(f32, f32)>,
        at: MediaTime,
    ) -> SceneCamera {
        let t = Tuning::new(&self.follow, base, canvas, bx);
        let dt = ((at.as_secs_f64() - self.at.as_secs_f64()) as f32).clamp(0.0, 0.25);
        self.at = at;
        let aim_now = (self.aim[1].x, self.aim[3].x);
        let (yaw, pitch) = (self.yaw.x, self.pitch.x);
        let dist_now = self.log_dist.x.exp();
        if dt <= 0.0 {
            return t.camera(aim_now, yaw, pitch, dist_now);
        }
        let pointer = pointer.unwrap_or(self.last);
        let (u, v) = t.seen(t.camera(aim_now, yaw, pitch, dist_now), pointer);
        let r = u.hypot(v);
        // 1. Inside the dead zone the look point holds; past it, only the excess moves it.
        let zone = t.dead_zone;
        let (dx, dy) = (pointer.0 - aim_now.0, pointer.1 - aim_now.1);
        let follow = if r > zone {
            let k = 1.0 - zone / r;
            (aim_now.0 + dx * k, aim_now.1 + dy * k)
        } else {
            aim_now
        };
        // 2. A fast pointer pulls the camera back for a moment.
        let raw = (pointer.0 - self.last.0).hypot(pointer.1 - self.last.1) / dt;
        self.last = pointer;
        let speed = self.speed.step(raw, 0.12, dt).max(0.0);
        let pull = 1.0 + (speed / t.fast).clamp(0.0, 1.0);
        let centre = bx.middle();
        let mut want_dist = (t.dist / self.zoom * pull).min(0.97 * t.max_dist(centre, yaw, pitch));
        // 3. Slide the look point back by any overshoot past the box; move in for a corner.
        let mut want = t.inside(follow, yaw, pitch, want_dist);
        let shows = |a: (f32, f32), d: f32| {
            let (u, v) = t.seen(t.camera(a, yaw, pitch, d), pointer);
            u.abs() <= 0.92 && v.abs() <= 0.92
        };
        if want_dist > 0.0 && !shows(want, want_dist) {
            let (mut lo, mut hi) = (t.nearest.min(want_dist), want_dist);
            for _ in 0..14 {
                let m = (lo * hi).sqrt();
                if shows(t.inside(follow, yaw, pitch, m), m) {
                    lo = m;
                } else {
                    hi = m;
                }
            }
            want_dist = lo;
            want = t.inside(follow, yaw, pitch, want_dist);
        }
        // 4. Glide there through two springs, faster the nearer the pointer is to leaving.
        let urgency = ((r - zone) / (0.85 - zone)).clamp(0.0, 1.0);
        let time = 0.5 * t.follow_time * (1.0 - 0.5 * urgency) * if r > 1.0 { 0.6 } else { 1.0 };
        let a1 = (
            self.aim[0].step(want.0, time, dt),
            self.aim[2].step(want.1, time, dt),
        );
        let aim = (
            self.aim[1].step(a1.0, time, dt),
            self.aim[3].step(a1.1, time, dt),
        );
        // 5. Lean with the look point's speed, settling back to the resting angle.
        let (vx, vy) = (self.aim[1].v, self.aim[3].v);
        let lean = |v: f32| (v * t.lean_per_speed).clamp(-t.lean_max, t.lean_max);
        let yaw = self.yaw.step(t.yaw + lean(vx), t.lean_time, dt);
        let pitch = self.pitch.step(t.pitch - lean(vy), t.lean_time, dt);
        // 6. The distance glides in log space, never past what fills the frame.
        let limit = t.max_dist(aim, yaw, pitch);
        let goal = want_dist.min(limit * 0.97).max(1.0);
        let mut dist = self.log_dist.step(goal.ln(), t.zoom_time, dt).exp();
        if limit > 0.0 && dist > limit {
            dist = limit;
            self.log_dist = Smooth {
                x: limit.ln(),
                v: self.log_dist.v.min(0.0),
            };
        }
        t.camera(aim, yaw, pitch, dist)
    }
}

/// How long a scene's follow camera is kept after the last frame that drew it.
const KEEP_S: f64 = 10.0;

/// The follow cameras of a render context, one per scene family (ADR-0113 §3).
#[derive(Debug, Default)]
pub(crate) struct Cameras {
    follows: BTreeMap<SceneId, Follow>,
}

impl Cameras {
    /// The follow camera of `family`, started (or started again, for a changed follow)
    /// from `follow` when it has none, stepped to `at` with the zoom steps asked for it up
    /// to `zooms`.
    #[allow(clippy::too_many_arguments, reason = "each is one input of the step")]
    pub(crate) fn follow(
        &mut self,
        family: &SceneId,
        follow: &CameraFollow,
        base: SceneCamera,
        canvas: Canvas,
        bx: FollowBox,
        pointer: Option<(f32, f32)>,
        (at, zooms): (MediaTime, i64),
    ) -> SceneCamera {
        let start = || Follow::new(follow, base, canvas, bx, pointer, (at, zooms));
        // The state nearly always exists: clone the family only to insert it.
        if let Some(state) = self.follows.get_mut(family) {
            if !state.is_for(follow) {
                *state = start();
            }
            state.zoom_to(zooms);
            return state.step(base, canvas, bx, pointer, at);
        }
        let state = self.follows.entry(family.clone()).or_insert_with(start);
        state.zoom_to(zooms);
        state.step(base, canvas, bx, pointer, at)
    }

    /// Forgets the follow cameras no frame has drawn for [`KEEP_S`] before `at`.
    pub(crate) fn sweep(&mut self, at: MediaTime) {
        let now = at.as_secs_f64();
        self.follows
            .retain(|_, state| now - state.at.as_secs_f64() <= KEEP_S);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_media::Timebase;
    use jstream_protocol::{ItemId, MotionClock, MotionKind};

    const CANVAS: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };
    const SCREEN: FollowBox = FollowBox {
        center: [960.0, 540.0],
        size: [1440.0, 900.0],
        depth: 0.0,
    };

    fn ms(ms: i64) -> MediaTime {
        MediaTime::new(ms, Timebase::new(1, 1000).expect("timebase"))
    }

    fn base() -> SceneCamera {
        SceneCamera {
            fov_deg: Finite32::new(40.0).expect("finite"),
            blur_px: Finite32::new(44.0).expect("finite"),
            focus_px: Finite32::new(800.0),
            ..SceneCamera::framing(CANVAS)
        }
    }

    fn follow() -> CameraFollow {
        CameraFollow::of(ItemId::new("screen"))
    }

    /// The box pixels the frame shows, as the follow camera's checks see them.
    fn on_box(camera: SceneCamera) -> Option<[(f32, f32); 8]> {
        Tuning::new(&follow(), base(), CANVAS, SCREEN).footprint(camera)
    }

    #[test]
    fn a_follow_camera_keeps_the_box_filling_the_frame_and_the_pointer_in_view() {
        let mut cameras = Cameras::default();
        let family = SceneId::new("a");
        // A pointer that sweeps the screen at 30 fps: across, into a corner and back.
        let path = |i: i64| -> (f32, f32) {
            let s = i as f32 / 300.0;
            let x = 720.0 + 700.0 * (s * 6.0).sin();
            let y = 450.0 + 440.0 * (s * 4.0).cos();
            (x, y)
        };
        let mut uncovered = 0;
        let mut out_of_view = 0;
        let mut last: Option<SceneCamera> = None;
        for i in 0..300 {
            let at = ms(i * 33);
            let pointer = path(i);
            let camera = cameras.follow(
                &family,
                &follow(),
                base(),
                CANVAS,
                SCREEN,
                Some(pointer),
                (at, 0),
            );
            assert!(camera.validate().is_ok(), "frame {i}: {camera:?}");
            let fp = on_box(camera).expect("the frame meets the screen");
            if !fp
                .iter()
                .all(|p| (-2.0..1442.0).contains(&p.0) && (-2.0..902.0).contains(&p.1))
            {
                uncovered += 1;
            }
            let t = Tuning::new(&follow(), base(), CANVAS, SCREEN);
            let (u, v) = t.seen(camera, pointer);
            // Near an edge the frame stays on the screen first: at an angle its footprint
            // is a skewed quad, which can't reach every point along the edges.
            let clear = (150.0..1290.0).contains(&pointer.0) && (150.0..750.0).contains(&pointer.1);
            if clear && (u.abs() > 1.0 || v.abs() > 1.0) {
                out_of_view += 1;
            }
            // Focus follows the distance to where it looks.
            let focus = camera.focus_px.expect("focus").get();
            assert!((focus - camera.target_distance()).abs() < 1.0, "frame {i}");
            if let Some(last) = last {
                // Smooth: the look point moves less than 60 px a frame.
                let moved = (camera.target.x.get() - last.target.x.get())
                    .hypot(camera.target.y.get() - last.target.y.get());
                assert!(moved < 60.0, "frame {i} jumped {moved}");
            }
            last = Some(camera);
        }
        assert_eq!(uncovered, 0, "frames showing past the screen");
        assert!(out_of_view < 10, "{out_of_view} frames lost the pointer");
    }

    #[test]
    fn zoom_moves_the_camera_in_within_its_range() {
        let mut cameras = Cameras::default();
        let family = SceneId::new("a");
        let at_rest = |cameras: &mut Cameras, from: i64, zooms: i64| {
            let mut camera = base();
            for i in 0..60 {
                camera = cameras.follow(
                    &family,
                    &follow(),
                    base(),
                    CANVAS,
                    SCREEN,
                    Some((720.0, 450.0)),
                    (ms(from + i * 33), zooms),
                );
            }
            camera.target_distance()
        };
        // Steps asked for before it started don't count.
        let far = at_rest(&mut cameras, 0, 5);
        let near = at_rest(&mut cameras, 2000, 8);
        assert!(near < far * 0.6, "{near} vs {far}");
        at_rest(&mut cameras, 4000, 108);
        assert_eq!(cameras.follows[&family].zoom(), FOLLOW_ZOOM_RANGE.1);
        at_rest(&mut cameras, 6000, -100);
        assert_eq!(cameras.follows[&family].zoom(), FOLLOW_ZOOM_RANGE.0);
        cameras.sweep(ms(8000 + 11_000));
        assert!(cameras.follows.is_empty());
    }

    #[test]
    fn the_pointer_maps_through_the_crop_and_fit_onto_the_box() {
        let geometry = |fit_mode, flip_h| ItemGeometry {
            fit_mode,
            flip_h,
            ..ItemGeometry::try_new(
                jstream_protocol::Vec2::new(960.0, 540.0).expect("finite"),
                jstream_protocol::Size::try_new(1600.0, 900.0).expect("size"),
            )
            .expect("geometry")
        };
        let plan = |fit, flip, crop| FollowPlan::new(follow(), &geometry(fit, flip), crop, None);
        let at = |x, y| PointerPosition { x, y };
        // A 1920x1080 capture fills a 16:9 box: scaled by 5/6.
        let fit = plan(FitMode::Fit, false, [0; 4]);
        assert_eq!(
            fit.on_box(at(960.0, 540.0), (1920.0, 1080.0)),
            Some((800.0, 450.0))
        );
        assert_eq!(fit.on_box(at(-5.0, 540.0), (1920.0, 1080.0)), None);
        // Cropped to its left half, fitted: 960x1080 in 1600x900, scaled 5/6, centred.
        let cropped = plan(FitMode::Fit, false, [0, 0, 960, 0]);
        assert_eq!(
            cropped.on_box(at(0.0, 0.0), (1920.0, 1080.0)),
            Some((400.0, 0.0))
        );
        assert_eq!(cropped.on_box(at(1000.0, 0.0), (1920.0, 1080.0)), None);
        let flipped = plan(FitMode::Stretch, true, [0; 4]);
        assert_eq!(
            flipped.on_box(at(0.0, 1080.0), (1920.0, 1080.0)),
            Some((1600.0, 900.0))
        );
    }

    #[test]
    fn without_a_pointer_it_rests_at_the_box_centre() {
        let mut cameras = Cameras::default();
        let family = SceneId::new("a");
        let mut camera = base();
        for i in 0..30 {
            camera = cameras.follow(
                &family,
                &follow(),
                base(),
                CANVAS,
                SCREEN,
                None,
                (ms(i * 33), 0),
            );
        }
        assert!((camera.target.x.get() - 960.0).abs() < 1.0, "{camera:?}");
        assert!((camera.target.y.get() - 540.0).abs() < 1.0, "{camera:?}");
    }

    #[test]
    fn camera_motion_adds_to_the_camera_and_holds_its_ranges() {
        let f = |v: f32| Finite32::new(v).expect("finite");
        let drift = |id: &str, property, rate| jstream_protocol::CameraMotionModifier {
            modifier_id: id.into(),
            property,
            kind: MotionKind::Drift {
                rate_per_s: f(rate),
                wrap: None,
            },
            clock: MotionClock::Engine,
            enabled: true,
        };
        let motion = CameraMotion::of(&[
            drift("x", CameraProperty::PositionX, 10.0),
            drift("fov", CameraProperty::Fov, 100.0),
            drift("roll", CameraProperty::Roll, -2.0),
        ]);
        assert!(!motion.is_still());
        let camera = motion.apply(base(), ms(2000), None);
        assert!((camera.position.x.get() - (base().position.x.get() + 20.0)).abs() < 1e-3);
        assert_eq!(camera.fov_deg.get(), MAX_FOV_DEG);
        assert!((camera.roll_deg.get() + 4.0).abs() < 1e-3);
        // A move onto its target keeps the unmoved position.
        let onto = CameraMotion::of(&[drift(
            "z",
            CameraProperty::PositionZ,
            -base().target_distance(),
        )]);
        assert_eq!(onto.apply(base(), ms(1000), None).position, base().position);
        assert_eq!(CameraMotion::default().apply(base(), ms(5), None), base());
    }
}
