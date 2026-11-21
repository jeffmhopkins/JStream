//! What a transition draws at a moment (ADR-0029 §5): each item's placement, crop and
//! opacity, interpolated between two scenes.
//!
//! The engine pairs items and plans the transition on its control thread, as a list of
//! [`Track`]s in drawing order. The render thread only samples them: [`Track::sample`]
//! is a pure function of the time since the transition started, so every frame, and a
//! `video.snapshot` at the same moment, draws the same thing.
//!
//! - A **pair** interpolates position, size, crop, opacity, rotation, tilt and turn,
//!   depth and perspective. Angles take the shortest way round: one angle that changes
//!   on its own, and more than one as one rotation along the shortest arc between the
//!   two orientations (ADR-0166 decision 1). The rest are linear.
//! - A pair with **stops on the way** (`via`, ADR-0166 decision 2) follows one smooth
//!   curve through all of them instead: Catmull-Rom for numbers, its spherical form for
//!   orientations, each stop at an equal share of the eased progress.
//! - An item **without a partner** appears or disappears with its effect: `fade`
//!   (opacity only), `scale` (toward 65% about its centre while fading), `slide` (toward
//!   the nearest canvas edge until it is off the canvas, while fading) or `none` (at
//!   once).
//! - An item that **passes through** (a `stinger` transition's overlay) appears with
//!   one effect, stays, and disappears with another: it covers the switch and then
//!   uncovers it.
//! - **Clamping:** a spring overshoots by design, so position, size, rotation and depth
//!   may pass their end values. Values with a hard range are clamped after easing:
//!   opacity to `[0, 1]`, crop to `[0, source size]`, size to `≥ 0` (an empty box draws
//!   nothing) and perspective to `> 0`.
//!
//! Docs: docs/adr/0029-transitions.md

use crate::scene::Crop;
use jstream_protocol::{
    BlendMode, Easing, Finite32, ItemGeometry, MAX_BLUR_PX, MAX_DRAWN_PX, MAX_FOV_DEG, MIN_FOV_DEG,
    SceneCamera, Settings, Size, Vec2, Vec3,
};
use serde_json::Value;

/// How much of its size a `scale` effect leaves at its end: 65%.
const SCALE_TO: f32 = 0.65;

/// The smallest perspective distance a sample keeps, in pixels: the compositor needs a
/// positive one.
const MIN_PERSPECTIVE: f32 = 1.0;

/// One end of an item's movement: where it is, how it is cropped, and how opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Endpoint {
    /// Its box on the canvas.
    pub geometry: ItemGeometry,
    /// Source pixels cut off each edge.
    pub crop: Crop,
    /// Straight-alpha multiplier in `0..=1`.
    pub opacity: f32,
    /// How it composites (ADR-0082). A pair that changes mode switches halfway.
    pub blend: BlendMode,
}

/// How an item without a partner appears or disappears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Opacity only.
    Fade,
    /// Toward 65% of its size about its centre, while fading.
    Scale,
    /// Toward the nearest canvas edge until off the canvas, while fading.
    Slide,
    /// At once: fully there, or gone, from the start.
    None,
}

/// When an item moves within the transition, and along which curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// Milliseconds from the start of the transition until the item starts.
    pub delay_ms: f32,
    /// How long the item takes, in milliseconds. Zero or less jumps to the end.
    pub duration_ms: f32,
    /// The curve its progress follows.
    pub easing: Easing,
}

impl Timing {
    /// The eased progress `elapsed_ms` after the transition started: 0 before the
    /// item's start, 1 after its end, and in between the curve's value, which a spring
    /// takes past 1.
    pub fn progress(&self, elapsed_ms: f32) -> f32 {
        if self.duration_ms <= 0.0 {
            return if elapsed_ms >= self.delay_ms {
                1.0
            } else {
                0.0
            };
        }
        self.easing
            .apply((elapsed_ms - self.delay_ms) / self.duration_ms)
    }

    /// When the item is done, in milliseconds from the start of the transition.
    pub fn end_ms(&self) -> f32 {
        self.delay_ms + self.duration_ms.max(0.0)
    }

    /// 0 before the item's start and 1 from it on: the progress of an item with no
    /// animation (`Effect::None`), which still waits for its delay.
    pub fn step(&self, elapsed_ms: f32) -> f32 {
        if elapsed_ms >= self.delay_ms {
            1.0
        } else {
            0.0
        }
    }
}

/// The most stops a path has: its two ends and [`jstream_protocol::MAX_VIA`] between.
pub const MAX_STOPS: usize = jstream_protocol::MAX_VIA + 2;

/// The stops a Move passes through between its two ends, in order
/// ([ADR-0166](../../../docs/adr/0166-moves-curve-through-keyframes-turn-as-one-and-may-orbit.md)
/// decision 2): at most [`jstream_protocol::MAX_VIA`], held inline so a [`Track`] stays
/// `Copy` and sampling allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Via<T> {
    at: [Option<T>; jstream_protocol::MAX_VIA],
}

impl<T: Copy> Via<T> {
    /// No stops: the Move goes straight from one end to the other.
    pub const NONE: Self = Self {
        at: [None; jstream_protocol::MAX_VIA],
    };

    /// The first [`jstream_protocol::MAX_VIA`] of `stops`, in order.
    pub fn new(stops: impl IntoIterator<Item = T>) -> Self {
        let mut via = Self::NONE;
        for (slot, stop) in via.at.iter_mut().zip(stops) {
            *slot = Some(stop);
        }
        via
    }

    /// Whether there are no stops.
    pub fn is_empty(&self) -> bool {
        self.at[0].is_none()
    }

    /// The stops, in order.
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.at.iter().map_while(|stop| *stop)
    }

    /// `from`, the stops and `to`, in order, in `out`; how many there are.
    fn path(&self, from: T, to: T, out: &mut [T; MAX_STOPS]) -> usize {
        out[0] = from;
        let mut n = 1;
        for stop in self.iter() {
            out[n] = stop;
            n += 1;
        }
        out[n] = to;
        n + 1
    }
}

impl<T: Copy> Default for Via<T> {
    fn default() -> Self {
        Self::NONE
    }
}

/// The stops a [`Track::Pair`] passes through between its two ends, in order
/// (ADR-0166 decision 2): none for a straight Move. Shared, so a track clones cheaply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stops(Option<std::sync::Arc<[Endpoint]>>);

impl Stops {
    /// No stops.
    pub const NONE: Self = Self(None);

    /// The first [`jstream_protocol::MAX_VIA`] of `stops`, in order.
    pub fn new(stops: impl IntoIterator<Item = Endpoint>) -> Self {
        let stops: Vec<Endpoint> = stops.into_iter().take(jstream_protocol::MAX_VIA).collect();
        if stops.is_empty() {
            Self::NONE
        } else {
            Self(Some(stops.into()))
        }
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// `from`, the stops and `to`, in order, in `out`; how many there are.
    fn path(&self, from: Endpoint, to: Endpoint, out: &mut [Endpoint; MAX_STOPS]) -> usize {
        out[0] = from;
        let mut n = 1;
        for stop in self.0.iter().flat_map(|stops| stops.iter()) {
            out[n] = *stop;
            n += 1;
        }
        out[n] = to;
        n + 1
    }
}

/// The uniform Catmull-Rom curve through `points` at `k` of the way, each point at an
/// equal share of it, in the pyramidal form (Barry and Goldman), whose every step is a
/// `mix` of two values: so a `mix` that is a slerp gives the spherical curve
/// (ADR-0166 decision 2). The ends take the one-sided tangent (a phantom point mirrored
/// past each end), so two points give `mix` itself. `k` outside `[0, 1]` carries on
/// along the first or last leg. `points` is not empty.
pub(crate) fn catmull<T: Copy>(points: &[T], k: f32, mix: impl Fn(T, T, f32) -> T) -> T {
    let legs = points.len().saturating_sub(1);
    if legs == 0 {
        return points[0];
    }
    let u = k * legs as f32;
    let i = (u.floor().max(0.0) as usize).min(legs - 1);
    let t = u - i as f32;
    let (p1, p2) = (points[i], points[i + 1]);
    let p0 = if i == 0 {
        mix(p1, p2, -1.0)
    } else {
        points[i - 1]
    };
    let p3 = if i + 2 > legs {
        mix(p2, p1, -1.0)
    } else {
        points[i + 2]
    };
    let a1 = mix(p0, p1, t + 1.0);
    let a2 = mix(p1, p2, t);
    let a3 = mix(p2, p3, t - 1.0);
    let b1 = mix(a1, a2, (t + 1.0) / 2.0);
    let b2 = mix(a2, a3, t / 2.0);
    mix(b1, b2, t)
}

/// One item's movement over a transition.
#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    /// An item that is on screen at both ends: it moves from `from` to `to`.
    Pair {
        /// Where it starts.
        from: Endpoint,
        /// Where it ends.
        to: Endpoint,
        /// When and how it moves.
        timing: Timing,
        /// The source's size in pixels, which bounds the crop, when known.
        source: Option<[u32; 2]>,
        /// Where it passes on the way, in order (ADR-0166 decision 2): with any, it
        /// follows one smooth curve through every stop.
        via: Stops,
    },
    /// An item that comes in without a partner.
    Appear {
        /// Where it ends.
        to: Endpoint,
        /// How it comes in.
        effect: Effect,
        /// When and how it comes in.
        timing: Timing,
    },
    /// An item that goes without a partner.
    Disappear {
        /// Where it starts.
        from: Endpoint,
        /// How it goes.
        effect: Effect,
        /// When and how it goes.
        timing: Timing,
    },
    /// An item that comes in, stays, and goes again within the transition: a `stinger`
    /// transition's overlay. Until `leave` starts it samples as [`Track::Appear`], and
    /// from then on as [`Track::Disappear`].
    Through {
        /// Where it is while it shows.
        at: Endpoint,
        /// How it comes in.
        appear: Effect,
        /// When and how it comes in.
        enter: Timing,
        /// How it goes.
        disappear: Effect,
        /// When and how it goes; its delay is when the item starts to go.
        leave: Timing,
    },
}

impl Track {
    /// When this track is done, in milliseconds from the start of the transition.
    pub fn end_ms(&self) -> f32 {
        match self {
            Track::Appear {
                effect: Effect::None,
                timing,
                ..
            }
            | Track::Disappear {
                effect: Effect::None,
                timing,
                ..
            } => timing.delay_ms,
            Track::Pair { timing, .. }
            | Track::Appear { timing, .. }
            | Track::Disappear { timing, .. } => timing.end_ms(),
            Track::Through {
                disappear: Effect::None,
                leave,
                ..
            } => leave.delay_ms,
            Track::Through { leave, .. } => leave.end_ms(),
        }
    }

    /// The track as it is `elapsed_ms` after the transition started: a
    /// [`Track::Through`] is its appearing half until it starts to leave, and its
    /// disappearing half from then on; every other track is itself.
    fn phase(&self, elapsed_ms: f32) -> Track {
        match self.clone() {
            Track::Through {
                at,
                appear,
                enter,
                disappear,
                leave,
            } => {
                if elapsed_ms < leave.delay_ms {
                    Track::Appear {
                        to: at,
                        effect: appear,
                        timing: enter,
                    }
                } else {
                    Track::Disappear {
                        from: at,
                        effect: disappear,
                        timing: leave,
                    }
                }
            }
            track => track,
        }
    }

    /// The item `elapsed_ms` after the transition started, on a canvas of `canvas`
    /// pixels, or `None` when it draws nothing then (fully transparent, or an empty
    /// box).
    pub fn sample(&self, elapsed_ms: f32, canvas: [f32; 2]) -> Option<Endpoint> {
        let sampled = match self.phase(elapsed_ms) {
            Track::Pair {
                from,
                to,
                timing,
                source,
                via,
            } if !via.is_empty() => {
                let mut stops = [from; MAX_STOPS];
                let n = via.path(from, to, &mut stops);
                path_endpoint(&stops[..n], timing.progress(elapsed_ms), source)
            }
            Track::Pair {
                from,
                to,
                timing,
                source,
                ..
            } => {
                let k = timing.progress(elapsed_ms);
                Endpoint {
                    geometry: lerp_geometry(&from.geometry, &to.geometry, k),
                    crop: lerp_crop(from.crop, to.crop, k, source),
                    opacity: lerp(from.opacity, to.opacity, k.clamp(0.0, 1.0)),
                    blend: if k < 0.5 { from.blend } else { to.blend },
                }
            }
            Track::Appear { to, effect, timing } => {
                let k = match effect {
                    Effect::None => timing.step(elapsed_ms),
                    _ => timing.progress(elapsed_ms).clamp(0.0, 1.0),
                };
                Endpoint {
                    geometry: away(&to.geometry, 1.0 - k, effect, canvas),
                    crop: to.crop,
                    opacity: to.opacity * k,
                    blend: to.blend,
                }
            }
            Track::Disappear {
                from,
                effect,
                timing,
            } => {
                let k = match effect {
                    Effect::None => timing.step(elapsed_ms),
                    _ => timing.progress(elapsed_ms),
                };
                Endpoint {
                    geometry: away(&from.geometry, k, effect, canvas),
                    crop: from.crop,
                    opacity: from.opacity * (1.0 - k.clamp(0.0, 1.0)),
                    blend: from.blend,
                }
            }
            // `phase` never returns one.
            Track::Through { .. } => return None,
        };
        let opacity = sampled.opacity.clamp(0.0, jstream_protocol::MAX_OPACITY);
        let size = sampled.geometry.size;
        let drawn = opacity > 0.0 && size.width.get() > 0.0 && size.height.get() > 0.0;
        drawn.then_some(Endpoint { opacity, ..sampled })
    }
}

/// A scene camera `k` of the way from `from` to `to` (ADR-0094 decision 7): position,
/// target and field of view linearly, roll the shortest way, and `depth_sort` from `to`.
/// `k` may pass 1 under a spring; the field of view is then clamped to what a camera may
/// have. Where the way passes a camera the collection would refuse (its position on its
/// target), it is `to`.
pub fn lerp_camera(from: &SceneCamera, to: &SceneCamera, k: f32) -> SceneCamera {
    let point = |a: Vec3, b: Vec3| Vec3 {
        x: finite(lerp(a.x.get(), b.x.get(), k)),
        y: finite(lerp(a.y.get(), b.y.get(), k)),
        z: finite(lerp(a.z.get(), b.z.get(), k)),
    };
    // Depth of field (ADR-0108 decision 6): an end with no focus is focused where the
    // other is, with no blur, so the blur fades in or out.
    let blur = |c: &SceneCamera| {
        if c.focus_px.is_some() {
            c.blur_px.get()
        } else {
            0.0
        }
    };
    let (focus_px, blur_px) = match (from.focus_px, to.focus_px) {
        (Some(a), Some(b)) => (
            Some(finite(
                lerp(a.get(), b.get(), k).clamp(f32::MIN_POSITIVE, MAX_DRAWN_PX),
            )),
            lerp(from.blur_px.get(), to.blur_px.get(), k),
        ),
        (Some(at), None) | (None, Some(at)) => (Some(at), lerp(blur(from), blur(to), k)),
        (None, None) => (None, lerp(from.blur_px.get(), to.blur_px.get(), k)),
    };
    let blur_px = finite(blur_px.clamp(0.0, MAX_BLUR_PX));
    let camera = SceneCamera {
        position: point(from.position, to.position),
        target: point(from.target, to.target),
        fov_deg: finite(
            lerp(from.fov_deg.get(), to.fov_deg.get(), k).clamp(MIN_FOV_DEG, MAX_FOV_DEG),
        ),
        roll_deg: finite(lerp_angle(from.roll_deg.get(), to.roll_deg.get(), k)),
        depth_sort: to.depth_sort,
        focus_px,
        blur_px,
    };
    if camera.validate().is_ok() {
        camera
    } else {
        *to
    }
}

/// A camera `k` of the way from `from` to `to` through the stops `via` (ADR-0166
/// decisions 2 and 3), along a straight path or, with `orbit`, circling its target. With
/// no stops and no orbit it is [`lerp_camera`]. Where the way passes a camera the
/// collection would refuse, it is `to`.
pub fn move_camera(
    from: &SceneCamera,
    via: &Via<SceneCamera>,
    to: &SceneCamera,
    k: f32,
    orbit: bool,
) -> SceneCamera {
    if via.is_empty() && !orbit {
        return lerp_camera(from, to, k);
    }
    let mut stops = [*from; MAX_STOPS];
    let n = via.path(*from, *to, &mut stops);
    let stops = &stops[..n];
    let curve = |value: &dyn Fn(&SceneCamera) -> f32| {
        let mut values = [0.0; MAX_STOPS];
        for (slot, camera) in values.iter_mut().zip(stops) {
            *slot = value(camera);
        }
        catmull(&values[..n], k, lerp)
    };
    let target = [
        curve(&|c| c.target.x.get()),
        curve(&|c| c.target.y.get()),
        curve(&|c| c.target.z.get()),
    ];
    let position = if orbit {
        let offset = |c: &SceneCamera| {
            [
                f64::from(c.position.x.get() - c.target.x.get()),
                f64::from(c.position.y.get() - c.target.y.get()),
                f64::from(c.position.z.get() - c.target.z.get()),
            ]
        };
        let length = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let mut directions = [[0.0, 0.0, 1.0]; MAX_STOPS];
        for (slot, camera) in directions.iter_mut().zip(stops) {
            let v = offset(camera);
            let len = length(v);
            if len > 0.0 {
                *slot = v.map(|x| x / len);
            }
        }
        // The picture's up is world −y (ADR-0094 decision 3).
        let up = [0.0, -1.0, 0.0];
        let direction = catmull(&directions[..n], k, |a, b, t| {
            crate::turn::slerp_direction(a, b, f64::from(t), up)
        });
        let distance = curve(&|c| length(offset(c)) as f32).max(MIN_ORBIT_PX);
        [0, 1, 2].map(|i| target[i] + (direction[i] * f64::from(distance)) as f32)
    } else {
        [
            curve(&|c| c.position.x.get()),
            curve(&|c| c.position.y.get()),
            curve(&|c| c.position.z.get()),
        ]
    };
    let point = |[x, y, z]: [f32; 3]| Vec3 {
        x: finite(x),
        y: finite(y),
        z: finite(z),
    };
    // Roll the short way on each leg: each stop's roll moved by whole turns to within
    // half a turn of the one before.
    let mut rolls = [0.0; MAX_STOPS];
    for i in 0..n {
        let roll = stops[i].roll_deg.get();
        rolls[i] = if i == 0 {
            roll
        } else {
            lerp_angle(rolls[i - 1], roll, 1.0)
        };
    }
    let roll = if k == 1.0 {
        to.roll_deg.get()
    } else {
        catmull(&rolls[..n], k, lerp)
    };
    // Depth of field (ADR-0108 decision 6): a stop with no focus is focused where the
    // nearest one with focus is, with no blur, so the blur fades in or out.
    let focused = stops.iter().any(|c| c.focus_px.is_some());
    let (focus_px, blur_px) = if focused {
        let focus_at = |i: usize| {
            stops[i].focus_px.map(Finite32::get).unwrap_or_else(|| {
                let before = stops[..i].iter().rev().find_map(|c| c.focus_px);
                let after = stops[i..].iter().find_map(|c| c.focus_px);
                before.or(after).map_or(0.0, Finite32::get)
            })
        };
        let mut focus = [0.0; MAX_STOPS];
        let mut blur = [0.0; MAX_STOPS];
        for i in 0..n {
            focus[i] = focus_at(i);
            blur[i] = if stops[i].focus_px.is_some() {
                stops[i].blur_px.get()
            } else {
                0.0
            };
        }
        (
            Some(finite(
                catmull(&focus[..n], k, lerp).clamp(f32::MIN_POSITIVE, MAX_DRAWN_PX),
            )),
            catmull(&blur[..n], k, lerp),
        )
    } else {
        (None, curve(&|c| c.blur_px.get()))
    };
    let camera = SceneCamera {
        position: point(position),
        target: point(target),
        fov_deg: finite(curve(&|c| c.fov_deg.get()).clamp(MIN_FOV_DEG, MAX_FOV_DEG)),
        roll_deg: finite(roll),
        depth_sort: to.depth_sort,
        focus_px,
        blur_px: finite(blur_px.clamp(0.0, MAX_BLUR_PX)),
    };
    if camera.validate().is_ok() {
        camera
    } else {
        *to
    }
}

/// The nearest an orbiting camera comes to its target, in pixels: a camera on its
/// target is refused.
const MIN_ORBIT_PX: f32 = 0.01;

/// An item `k` of the way along the path through `stops` (ADR-0166 decision 2): every
/// number on the Catmull-Rom curve through its stops, the orientation on the spherical
/// one, and the parts that switch at the path's midpoint, from the first stop to the
/// last. `stops` holds at least two.
fn path_endpoint(stops: &[Endpoint], k: f32, source: Option<[u32; 2]>) -> Endpoint {
    let n = stops.len();
    let (first, last) = (&stops[0], &stops[n - 1]);
    let curve_at = |k: f32, value: &dyn Fn(&Endpoint) -> f32| {
        let mut values = [0.0; MAX_STOPS];
        for (slot, stop) in values.iter_mut().zip(stops) {
            *slot = value(stop);
        }
        catmull(&values[..n], k, lerp)
    };
    let curve = |value: &dyn Fn(&Endpoint) -> f32| curve_at(k, value);
    let g = |stop: &Endpoint| stop.geometry;
    let later = k >= 0.5;
    let (a, b) = (g(first), g(last));
    let perspective_px = if stops.iter().all(|s| s.geometry.perspective_px.is_some()) {
        Some(finite(
            curve(&|s| s.geometry.perspective_px.map_or(0.0, Finite32::get)).max(MIN_PERSPECTIVE),
        ))
    } else if later {
        b.perspective_px
    } else {
        a.perspective_px
    };
    let surface = if stops.iter().all(|s| s.geometry.surface.is_none()) {
        None
    } else {
        let number = |pick: fn(&jstream_protocol::Surface) -> Finite32| {
            finite(curve(&|s| {
                pick(&s.geometry.surface.unwrap_or_default()).get()
            }))
        };
        let switched = if later { b.surface } else { a.surface }.unwrap_or_default();
        Some(jstream_protocol::Surface {
            roughness: number(|s| s.roughness),
            metalness: number(|s| s.metalness),
            emissive: number(|s| s.emissive),
            ..switched
        })
    };
    // Each angle unwrapped leg by leg, the short way, as the per-axis hint and as the
    // answer when only one of them changes along the path.
    let mut angles = [[0.0; 3]; MAX_STOPS];
    let mut changes = [false; 3];
    for i in 0..n {
        let s = &stops[i].geometry;
        let raw = [s.rotation_deg, s.turn_deg, s.tilt_deg].map(Finite32::get);
        for axis in 0..3 {
            angles[i][axis] = if i == 0 {
                raw[axis]
            } else {
                lerp_angle(angles[i - 1][axis], raw[axis], 1.0)
            };
            if i > 0 && angles[i][axis] != angles[0][axis] {
                changes[axis] = true;
            }
        }
    }
    let hint = [0, 1, 2].map(|axis| {
        let mut values = [0.0; MAX_STOPS];
        for i in 0..n {
            values[i] = angles[i][axis];
        }
        catmull(&values[..n], k, lerp)
    });
    let ends = |e: &ItemGeometry| [e.rotation_deg, e.turn_deg, e.tilt_deg].map(Finite32::get);
    let turned = if k == 0.0 {
        ends(&a)
    } else if k == 1.0 {
        ends(&b)
    } else if changes.iter().filter(|c| **c).count() <= 1 {
        hint
    } else {
        let mut raw = [[0.0; 3]; MAX_STOPS];
        for i in 0..n {
            raw[i] = ends(&stops[i].geometry);
        }
        crate::turn::path_angles(&raw[..n], k, hint)
    };
    let [rotation_deg, turn_deg, tilt_deg] = turned.map(finite);
    let geometry = ItemGeometry {
        center: Vec2 {
            x: finite(curve(&|s| s.geometry.center.x.get())),
            y: finite(curve(&|s| s.geometry.center.y.get())),
        },
        size: Size {
            width: finite(curve(&|s| s.geometry.size.width.get()).max(0.0)),
            height: finite(curve(&|s| s.geometry.size.height.get()).max(0.0)),
        },
        fit_mode: if later { b.fit_mode } else { a.fit_mode },
        flip_h: if later { b.flip_h } else { a.flip_h },
        flip_v: if later { b.flip_v } else { a.flip_v },
        billboard: if later { b.billboard } else { a.billboard },
        surface,
        rotation_deg,
        tilt_deg,
        turn_deg,
        depth_px: finite(curve(&|s| s.geometry.depth_px.get())),
        perspective_px,
    };
    let [width, height] = source.unwrap_or([u32::MAX, u32::MAX]);
    let side = |pick: fn(&Crop) -> u32, limit: u32| {
        let v = curve(&|s| pick(&s.crop) as f32).round();
        if v <= 0.0 {
            0
        } else if v >= limit as f32 {
            limit
        } else {
            v as u32
        }
    };
    Endpoint {
        geometry,
        crop: Crop {
            left: side(|c| c.left, width),
            top: side(|c| c.top, height),
            right: side(|c| c.right, width),
            bottom: side(|c| c.bottom, height),
        },
        opacity: curve_at(k.clamp(0.0, 1.0), &|s| s.opacity),
        blend: if later { last.blend } else { first.blend },
    }
}

/// Whether a Move may blend an effect's settings from `from` to `to`
/// ([ADR-0127](../../../docs/adr/0127-moves-interpolate-effect-settings.md)): every
/// parameter either holds the same value on both sides, or holds a plain value on each
/// (a number, a colour, a string, a boolean). A reference (a scene, a source, a bus or a
/// file) or `null` that differs can't be blended, since each side's effect reads only
/// its own, so the item cross-fades instead.
pub fn settings_blend(from: &Settings, to: &Settings) -> bool {
    let plain = |value: Option<&Value>| {
        matches!(
            value,
            Some(Value::Bool(_) | Value::Number(_) | Value::String(_))
        )
    };
    from.0
        .keys()
        .chain(to.0.keys())
        .all(|id| match (from.get(id), to.get(id)) {
            (a, b) if a == b => true,
            (a, b) => plain(a) && plain(b),
        })
}

/// An effect's settings `k` of the way from `from` to `to`
/// ([ADR-0127](../../../docs/adr/0127-moves-interpolate-effect-settings.md)): each
/// number linearly, rounded for a parameter in `whole` (the kind's `int` parameters, from
/// its descriptor: a float stored as `0` and `1` still blends through `0.5`), each colour
/// per channel, and every other value that differs switching from `from`'s to `to`'s at
/// `k = 0.5`. The parameters are `to`'s. `k` is clamped to `[0, 1]`, so a spring never takes a setting past its
/// ends and out of the range its kind allows.
pub fn blend_settings(from: &Settings, to: &Settings, k: f32, whole: &[String]) -> Settings {
    let k = if k.is_nan() { 0.0 } else { k.clamp(0.0, 1.0) };
    let mut out = to.clone();
    for (id, value) in out.0.iter_mut() {
        // A parameter only `to` has keeps `to`'s value throughout.
        let Some(start) = from.get(id) else {
            continue;
        };
        if start == value {
            continue;
        }
        let whole = whole.iter().any(|w| w == id);
        if let Some(blended) = blend_value(start, value, k, whole) {
            *value = blended;
        } else if k < 0.5 {
            *value = start.clone();
        }
    }
    out
}

/// `a` and `b`, two numbers or two colours, `k` of the way, a number rounded when it is
/// `whole`; `None` for anything else.
fn blend_value(a: &Value, b: &Value, k: f32, whole: bool) -> Option<Value> {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64()?, b.as_f64()?);
            let v = a + (b - a) * f64::from(k);
            if whole {
                return Some(Value::from(v.round() as i64));
            }
            serde_json::Number::from_f64(v).map(Value::Number)
        }
        (Value::String(a), Value::String(b)) => {
            let (a, alpha_a) = parse_color(a)?;
            let (b, alpha_b) = parse_color(b)?;
            let mut out = String::with_capacity(9);
            out.push('#');
            let channels = if alpha_a || alpha_b { 4 } else { 3 };
            for (x, y) in a.iter().zip(b).take(channels) {
                let v = lerp(f32::from(*x), f32::from(y), k)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                out.push_str(&format!("{v:02x}"));
            }
            Some(Value::String(out))
        }
        _ => None,
    }
}

/// A `#rrggbb` or `#rrggbbaa` colour as its four channels (alpha 255 when absent), and
/// whether it carried alpha.
fn parse_color(text: &str) -> Option<([u8; 4], bool)> {
    let hex = text.strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    let alpha = hex.len() == 8;
    Some((
        [
            byte(0)?,
            byte(2)?,
            byte(4)?,
            if alpha { byte(6)? } else { 255 },
        ],
        alpha,
    ))
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

/// The shortest way from angle `a` to `b`, in degrees, `k` of the way.
fn lerp_angle(a: f32, b: f32, k: f32) -> f32 {
    let d = ((b - a) % 360.0 + 540.0) % 360.0 - 180.0;
    a + d * k
}

/// `v` as a protocol number; a non-finite one (an overflow) is 0.
fn finite(v: f32) -> Finite32 {
    Finite32::new(v).unwrap_or(Finite32::ZERO)
}

fn lerp_geometry(a: &ItemGeometry, b: &ItemGeometry, k: f32) -> ItemGeometry {
    let num = |x: Finite32, y: Finite32| finite(lerp(x.get(), y.get(), k));
    let [rotation_deg, turn_deg, tilt_deg] = lerp_orientation(a, b, k).map(finite);
    // The discrete parts switch halfway; a perspective present at both ends moves.
    let later = k >= 0.5;
    let perspective_px = match (a.perspective_px, b.perspective_px) {
        (Some(x), Some(y)) => Some(finite(lerp(x.get(), y.get(), k).max(MIN_PERSPECTIVE))),
        (x, y) => {
            if later {
                y
            } else {
                x
            }
        }
    };
    ItemGeometry {
        center: Vec2 {
            x: num(a.center.x, b.center.x),
            y: num(a.center.y, b.center.y),
        },
        size: Size {
            width: finite(lerp(a.size.width.get(), b.size.width.get(), k).max(0.0)),
            height: finite(lerp(a.size.height.get(), b.size.height.get(), k).max(0.0)),
        },
        fit_mode: if later { b.fit_mode } else { a.fit_mode },
        flip_h: if later { b.flip_h } else { a.flip_h },
        flip_v: if later { b.flip_v } else { a.flip_v },
        billboard: if later { b.billboard } else { a.billboard },
        surface: lerp_surface(a.surface, b.surface, k, later),
        rotation_deg,
        tilt_deg,
        turn_deg,
        depth_px: num(a.depth_px, b.depth_px),
        perspective_px,
    }
}

/// The angles `[rotation, turn, tilt]` `k` of the way from `a`'s to `b`'s
/// ([ADR-0166](../../../docs/adr/0166-moves-curve-through-keyframes-turn-as-one-and-may-orbit.md)
/// decision 1): the shortest way on the one angle that changes, or, when more than one
/// does, one rotation along the shortest arc between the two orientations.
fn lerp_orientation(a: &ItemGeometry, b: &ItemGeometry, k: f32) -> [f32; 3] {
    let from = [a.rotation_deg, a.turn_deg, a.tilt_deg].map(Finite32::get);
    let to = [b.rotation_deg, b.turn_deg, b.tilt_deg].map(Finite32::get);
    let mut per_axis = [0.0; 3];
    let mut changed = 0;
    for i in 0..3 {
        per_axis[i] = lerp_angle(from[i], to[i], k);
        if lerp_angle(from[i], to[i], 1.0) != from[i] {
            changed += 1;
        }
    }
    if changed <= 1 {
        return per_axis;
    }
    crate::turn::slerp_angles(from, to, k, per_axis)
}

/// A card's surface between `a` and `b` (ADR-0151 decision 10): its numbers glide, an
/// absent one gliding from or to the default, and its switches change at the midpoint
/// as the geometry's do.
fn lerp_surface(
    a: Option<jstream_protocol::Surface>,
    b: Option<jstream_protocol::Surface>,
    k: f32,
    later: bool,
) -> Option<jstream_protocol::Surface> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let (sa, sb) = (a.unwrap_or_default(), b.unwrap_or_default());
    let num = |x: Finite32, y: Finite32| finite(lerp(x.get(), y.get(), k));
    let switched = if later { sb } else { sa };
    Some(jstream_protocol::Surface {
        roughness: num(sa.roughness, sb.roughness),
        metalness: num(sa.metalness, sb.metalness),
        emissive: num(sa.emissive, sb.emissive),
        ..switched
    })
}

fn lerp_crop(a: Crop, b: Crop, k: f32, source: Option<[u32; 2]>) -> Crop {
    let [width, height] = source.unwrap_or([u32::MAX, u32::MAX]);
    let side = |x: u32, y: u32, limit: u32| {
        let v = lerp(x as f32, y as f32, k).round();
        if v <= 0.0 {
            0
        } else if v >= limit as f32 {
            limit
        } else {
            v as u32
        }
    };
    Crop {
        left: side(a.left, b.left, width),
        top: side(a.top, b.top, height),
        right: side(a.right, b.right, width),
        bottom: side(a.bottom, b.bottom, height),
    }
}

/// `geometry` moved `k` of the way out by `effect`: 0 is where it is, 1 is the end of
/// the effect. `fade` and `none` don't move it.
fn away(geometry: &ItemGeometry, k: f32, effect: Effect, [cw, ch]: [f32; 2]) -> ItemGeometry {
    let mut out = *geometry;
    match effect {
        Effect::Scale => {
            let s = 1.0 - (1.0 - SCALE_TO) * k;
            out.size = Size {
                width: finite((geometry.size.width.get() * s).max(0.0)),
                height: finite((geometry.size.height.get() * s).max(0.0)),
            };
        }
        Effect::Slide => {
            let (cx, cy) = (geometry.center.x.get(), geometry.center.y.get());
            // Half the extent of the rotated box along each axis, so it ends fully off.
            let (sin, cos) = geometry.rotation_deg.get().to_radians().sin_cos();
            let (w, h) = (geometry.size.width.get(), geometry.size.height.get());
            let hx = (w * cos.abs() + h * sin.abs()) * 0.5;
            let hy = (w * sin.abs() + h * cos.abs()) * 0.5;
            let edges = [cx, cw - cx, cy, ch - cy];
            let nearest = (0..4).fold(0, |best, i| if edges[i] < edges[best] { i } else { best });
            let (dx, dy) = match nearest {
                0 => (-(cx + hx), 0.0),
                1 => (cw - cx + hx, 0.0),
                2 => (0.0, -(cy + hy)),
                _ => (0.0, ch - cy + hy),
            };
            out.center = Vec2 {
                x: finite(cx + dx * k),
                y: finite(cy + dy * k),
            };
        }
        Effect::Fade | Effect::None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Effect, Endpoint, Timing, Track, lerp_angle};
    use crate::scene::Crop;
    use jstream_protocol::{
        BlendMode, Easing, Finite32, ItemGeometry, SceneCamera, Size, Vec2, Vec3,
    };

    const CANVAS: [f32; 2] = [1920.0, 1080.0];

    fn at(x: f32, y: f32, w: f32, h: f32) -> Endpoint {
        Endpoint {
            geometry: ItemGeometry::try_new(
                Vec2::new(x, y).expect("finite"),
                Size::try_new(w, h).expect("positive"),
            )
            .expect("valid"),
            crop: Crop::NONE,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }

    fn linear(delay_ms: f32) -> Timing {
        Timing {
            delay_ms,
            duration_ms: 1000.0,
            easing: Easing::Linear,
        }
    }

    fn centre(e: &Endpoint) -> (f32, f32) {
        (e.geometry.center.x.get(), e.geometry.center.y.get())
    }

    /// A stinger's overlay item covers the switch and then uncovers it.
    #[test]
    fn a_through_track_comes_in_stays_and_goes() {
        let track = Track::Through {
            at: at(960.0, 540.0, 400.0, 1080.0),
            appear: Effect::Slide,
            enter: Timing {
                delay_ms: 100.0,
                duration_ms: 400.0,
                easing: Easing::Linear,
            },
            disappear: Effect::Fade,
            leave: Timing {
                delay_ms: 700.0,
                duration_ms: 300.0,
                easing: Easing::Linear,
            },
        };
        assert!(track.sample(50.0, CANVAS).is_none(), "not yet in");
        let entering = track.sample(300.0, CANVAS).expect("coming in");
        assert!(centre(&entering) != (960.0, 540.0) && entering.opacity < 1.0);
        for t in [500.0, 600.0, 699.0] {
            let held = track.sample(t, CANVAS).expect("there");
            assert_eq!(centre(&held), (960.0, 540.0));
            assert!((held.opacity - 1.0).abs() < 1e-6);
        }
        let leaving = track.sample(850.0, CANVAS).expect("going");
        assert_eq!(centre(&leaving), (960.0, 540.0));
        assert!((leaving.opacity - 0.5).abs() < 1e-3);
        assert!(track.sample(1000.0, CANVAS).is_none(), "gone");
        assert!((track.end_ms() - 1000.0).abs() < 1e-6);
        let mut cut = track;
        if let Track::Through { disappear, .. } = &mut cut {
            *disappear = Effect::None;
        }
        assert!(cut.sample(699.0, CANVAS).is_some());
        assert!(cut.sample(700.0, CANVAS).is_none());
        assert!((cut.end_ms() - 700.0).abs() < 1e-6);
    }

    #[test]
    fn a_pair_interpolates_after_its_delay() {
        let track = Track::Pair {
            from: at(100.0, 100.0, 200.0, 100.0),
            to: Endpoint {
                crop: Crop {
                    left: 10,
                    ..Crop::NONE
                },
                opacity: 0.5,
                ..at(300.0, 500.0, 400.0, 300.0)
            },
            timing: linear(200.0),
            source: None,
            via: super::Stops::NONE,
        };
        let start = track.sample(0.0, CANVAS).expect("drawn");
        assert_eq!(centre(&start), (100.0, 100.0));
        let half = track.sample(700.0, CANVAS).expect("drawn");
        assert_eq!(centre(&half), (200.0, 300.0));
        assert_eq!(half.geometry.size.width.get(), 300.0);
        assert_eq!(half.crop.left, 5);
        assert_eq!(half.opacity, 0.75);
        let end = track.sample(5000.0, CANVAS).expect("drawn");
        assert_eq!(centre(&end), (300.0, 500.0));
        assert_eq!(track.end_ms(), 1200.0);
    }

    #[test]
    fn flips_switch_halfway() {
        let mut to = at(100.0, 100.0, 200.0, 100.0);
        to.geometry.flip_h = true;
        to.geometry.flip_v = true;
        let track = Track::Pair {
            from: at(100.0, 100.0, 200.0, 100.0),
            to,
            timing: linear(0.0),
            source: None,
            via: super::Stops::NONE,
        };
        let before = track.sample(499.0, CANVAS).expect("drawn");
        assert!(!before.geometry.flip_h && !before.geometry.flip_v);
        let after = track.sample(500.0, CANVAS).expect("drawn");
        assert!(after.geometry.flip_h && after.geometry.flip_v);
    }

    /// A camera moves linearly, rolls the shortest way, and keeps its field of view in
    /// range past the end of a spring (ADR-0094 decision 7).
    #[test]
    fn a_camera_moves_between_its_ends() {
        let point = |x: f32, y: f32, z: f32| Vec3::new(x, y, z).expect("finite");
        let camera = |x: f32, fov: f32, roll: f32| SceneCamera {
            position: point(x, 0.0, 1000.0),
            target: point(x, 0.0, 0.0),
            fov_deg: Finite32::new(fov).expect("finite"),
            roll_deg: Finite32::new(roll).expect("finite"),
            depth_sort: true,
            focus_px: None,
            blur_px: Finite32::ZERO,
        };
        let from = camera(0.0, 40.0, 350.0);
        let to = camera(100.0, 60.0, 10.0);
        let half = super::lerp_camera(&from, &to, 0.5);
        assert_eq!(half.position.x.get(), 50.0);
        assert_eq!(half.fov_deg.get(), 50.0);
        assert!(
            (half.roll_deg.get() - 360.0).abs() < 1e-3,
            "{}",
            half.roll_deg.get()
        );
        let wide = super::lerp_camera(&camera(0.0, 160.0, 0.0), &camera(0.0, 170.0, 0.0), 1.4);
        assert_eq!(wide.fov_deg.get(), 170.0);
        // Halfway between a camera and its reverse, position meets target: it is `to`.
        let reversed = SceneCamera {
            position: to.target,
            target: to.position,
            ..to
        };
        assert_eq!(super::lerp_camera(&to, &reversed, 0.5), reversed);
    }

    /// ADR-0108 decision 6: focus and blur move linearly; a camera with no focus is taken
    /// as focused where the other is, with no blur, so the blur fades in or out.
    #[test]
    fn a_cameras_focus_moves_between_its_ends() {
        let point = |x: f32, y: f32, z: f32| Vec3::new(x, y, z).expect("finite");
        let camera = |focus: Option<f32>, blur: f32| SceneCamera {
            position: point(0.0, 0.0, 1000.0),
            target: point(0.0, 0.0, 0.0),
            fov_deg: Finite32::new(45.0).expect("finite"),
            roll_deg: Finite32::ZERO,
            depth_sort: true,
            focus_px: focus.and_then(Finite32::new),
            blur_px: Finite32::new(blur).expect("finite"),
        };
        let pulled = super::lerp_camera(&camera(Some(400.0), 4.0), &camera(Some(800.0), 8.0), 0.25);
        assert_eq!(pulled.focus_px.map(Finite32::get), Some(500.0));
        assert_eq!(pulled.blur_px.get(), 5.0);
        let fading_in = super::lerp_camera(&camera(None, 30.0), &camera(Some(600.0), 10.0), 0.5);
        assert_eq!(fading_in.focus_px.map(Finite32::get), Some(600.0));
        assert_eq!(
            fading_in.blur_px.get(),
            5.0,
            "a camera with no focus has no blur"
        );
        let fading_out = super::lerp_camera(&camera(Some(600.0), 10.0), &camera(None, 0.0), 0.75);
        assert_eq!(fading_out.focus_px.map(Finite32::get), Some(600.0));
        assert_eq!(fading_out.blur_px.get(), 2.5);
        let overshoot =
            super::lerp_camera(&camera(Some(600.0), 60.0), &camera(Some(600.0), 64.0), 1.5);
        assert_eq!(
            overshoot.blur_px.get(),
            64.0,
            "a spring past the end keeps blur in range"
        );
    }

    /// A blend mode can't be interpolated, so a pair switches it halfway, as it does
    /// flips, and an item alone keeps its own.
    #[test]
    fn blend_switches_halfway() {
        let to = Endpoint {
            blend: BlendMode::Screen,
            ..at(100.0, 100.0, 200.0, 100.0)
        };
        let track = Track::Pair {
            from: at(100.0, 100.0, 200.0, 100.0),
            to,
            timing: linear(0.0),
            source: None,
            via: super::Stops::NONE,
        };
        let blend = |ms: f32| track.sample(ms, CANVAS).expect("drawn").blend;
        assert_eq!(blend(499.0), BlendMode::Normal);
        assert_eq!(blend(500.0), BlendMode::Screen);
        let appear = Track::Appear {
            to,
            effect: Effect::Fade,
            timing: linear(0.0),
        };
        assert_eq!(
            appear.sample(10.0, CANVAS).expect("drawn").blend,
            BlendMode::Screen
        );
        let disappear = Track::Disappear {
            from: to,
            effect: Effect::Fade,
            timing: linear(0.0),
        };
        assert_eq!(
            disappear.sample(990.0, CANVAS).expect("drawn").blend,
            BlendMode::Screen
        );
    }

    #[test]
    fn angles_take_the_shortest_way() {
        assert_eq!(lerp_angle(350.0, 10.0, 0.5), 360.0);
        assert_eq!(lerp_angle(10.0, 350.0, 0.5), 0.0);
        assert_eq!(lerp_angle(0.0, 90.0, 0.5), 45.0);
        assert_eq!(lerp_angle(-170.0, 170.0, 0.5), -180.0);
    }

    /// ADR-0166 decision 2: two points give the straight line, and a curve through
    /// three passes through the middle one at its share with one speed either side.
    #[test]
    fn the_curve_is_a_line_through_two_points_and_smooth_through_three() {
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        for k in [-0.2, 0.0, 0.3, 0.5, 1.0, 1.2] {
            let got = super::catmull(&[10.0, 30.0], k, mix);
            assert!((got - (10.0 + 20.0 * k)).abs() < 1e-4, "{k}: {got}");
        }
        let points = [0.0, 100.0, 400.0];
        assert!((super::catmull(&points, 0.5, mix) - 100.0).abs() < 1e-4);
        let h = 1e-3;
        let left = (super::catmull(&points, 0.5, mix) - super::catmull(&points, 0.5 - h, mix)) / h;
        let right = (super::catmull(&points, 0.5 + h, mix) - super::catmull(&points, 0.5, mix)) / h;
        // Tangent (400 - 0) / 2 per leg, a leg being half the path: 400 per unit.
        assert!((left - 400.0).abs() < 1.0, "{left}");
        assert!((right - 400.0).abs() < 1.0, "{right}");
    }

    /// ADR-0166 decision 2: a pair with a stop passes through it, turning on two axes
    /// along the spherical curve, and ends exactly on its end.
    #[test]
    fn a_pair_with_a_stop_passes_through_it() {
        let from = at(0.0, 500.0, 100.0, 100.0);
        let mut stop = at(400.0, 300.0, 200.0, 100.0);
        stop.geometry.turn_deg = Finite32::new(40.0).expect("finite");
        stop.geometry.tilt_deg = Finite32::new(20.0).expect("finite");
        let mut to = at(1000.0, 500.0, 100.0, 100.0);
        to.geometry.turn_deg = Finite32::new(90.0).expect("finite");
        let track = Track::Pair {
            from,
            to,
            timing: linear(0.0),
            source: None,
            via: super::Stops::new([stop]),
        };
        let mid = track.sample(500.0, CANVAS).expect("drawn").geometry;
        assert!((mid.center.x.get() - 400.0).abs() < 1e-3, "{mid:?}");
        assert!((mid.center.y.get() - 300.0).abs() < 1e-3, "{mid:?}");
        assert!((mid.size.width.get() - 200.0).abs() < 1e-3, "{mid:?}");
        assert!((mid.turn_deg.get() - 40.0).abs() < 1e-2, "{mid:?}");
        assert!((mid.tilt_deg.get() - 20.0).abs() < 1e-2, "{mid:?}");
        let end = track.sample(1000.0, CANVAS).expect("drawn").geometry;
        assert_eq!(end, to.geometry);
    }

    /// ADR-0166 decision 3: an orbit keeps the camera's distance from its target and
    /// meets both ends; a straight move cuts across, nearer.
    #[test]
    fn an_orbit_keeps_its_distance_and_a_line_cuts_across() {
        use jstream_protocol::Canvas;
        let a = SceneCamera::framing(Canvas::DEFAULT);
        let d = a.position.z.get() - a.target.z.get();
        let b = SceneCamera {
            position: Vec3 {
                x: Finite32::new(a.target.x.get() + d).expect("finite"),
                y: a.target.y,
                z: a.target.z,
            },
            ..a
        };
        let dist = |c: &SceneCamera| {
            let v = [
                c.position.x.get() - c.target.x.get(),
                c.position.y.get() - c.target.y.get(),
                c.position.z.get() - c.target.z.get(),
            ];
            (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
        };
        let none = super::Via::NONE;
        for k in [0.25, 0.5, 0.75] {
            let orbit = super::move_camera(&a, &none, &b, k, true);
            assert!((dist(&orbit) - d).abs() < 0.05, "{k}: {orbit:?}");
        }
        assert_eq!(
            super::move_camera(&a, &none, &b, 0.0, true).position,
            a.position
        );
        let end = super::move_camera(&a, &none, &b, 1.0, true).position;
        assert!((end.x.get() - b.position.x.get()).abs() < 1e-2);
        assert!((end.z.get() - b.position.z.get()).abs() < 1e-2);
        let line = super::move_camera(&a, &none, &b, 0.5, false);
        assert!(dist(&line) < d * 0.75);
        assert_eq!(line, super::lerp_camera(&a, &b, 0.5));
    }

    /// ADR-0166 decision 1: a pair that turns and tilts at once moves as one rotation,
    /// so halfway it is the direct turn's halfway, not each angle's.
    #[test]
    fn a_pair_turning_on_two_axes_turns_as_one_rotation() {
        let from = at(500.0, 500.0, 100.0, 100.0);
        let mut to = from;
        to.geometry.turn_deg = Finite32::new(90.0).expect("finite");
        to.geometry.tilt_deg = Finite32::new(90.0).expect("finite");
        let track = Track::Pair {
            from,
            to,
            timing: linear(0.0),
            source: None,
            via: super::Stops::NONE,
        };
        let half = track.sample(500.0, CANVAS).expect("drawn").geometry;
        // Per axis it would be (0, 45, 45); one rotation takes another triple. Both
        // reach the end exactly.
        let angles = [half.rotation_deg, half.turn_deg, half.tilt_deg].map(Finite32::get);
        assert!((angles[1] - 45.0).abs() > 1.0, "{angles:?}");
        let end = track.sample(1000.0, CANVAS).expect("drawn").geometry;
        assert_eq!(end.turn_deg.get(), 90.0);
        assert_eq!(end.tilt_deg.get(), 90.0);
        assert_eq!(end.rotation_deg.get(), 0.0);

        // One angle alone moves exactly as before.
        let mut turned = from;
        turned.geometry.turn_deg = Finite32::new(60.0).expect("finite");
        let one = Track::Pair {
            from,
            to: turned,
            timing: linear(0.0),
            source: None,
            via: super::Stops::NONE,
        };
        let half = one.sample(500.0, CANVAS).expect("drawn").geometry;
        assert_eq!(half.turn_deg.get(), 30.0);
        assert_eq!(half.tilt_deg.get(), 0.0);
        assert_eq!(half.rotation_deg.get(), 0.0);
    }

    #[test]
    fn a_spring_overshoots_but_clamped_values_stay_in_range() {
        let mut to = at(1000.0, 500.0, 100.0, 100.0);
        to.crop = Crop {
            right: 30,
            ..Crop::NONE
        };
        let track = Track::Pair {
            from: Endpoint {
                opacity: 0.2,
                ..at(0.0, 500.0, 0.000_1, 100.0)
            },
            to,
            timing: Timing {
                delay_ms: 0.0,
                duration_ms: 1000.0,
                easing: Easing::Spring,
            },
            source: Some([32, 32]),
            via: super::Stops::NONE,
        };
        // At p = 0.25 the spring is 1.206: past the end.
        let over = track.sample(250.0, CANVAS).expect("drawn");
        assert!(centre(&over).0 > 1000.0, "{:?}", centre(&over));
        assert!(over.geometry.size.width.get() > 100.0);
        assert_eq!(over.opacity, 1.0);
        assert_eq!(over.crop.right, 32, "the crop stops at the source's width");
    }

    #[test]
    fn unpaired_items_fade_scale_and_slide() {
        let item = at(100.0, 540.0, 100.0, 100.0);
        let fade = Track::Disappear {
            from: item,
            effect: Effect::Fade,
            timing: linear(0.0),
        };
        assert_eq!(fade.sample(250.0, CANVAS).map(|e| e.opacity), Some(0.75));
        assert_eq!(fade.sample(1000.0, CANVAS), None, "gone at the end");

        let scale = Track::Appear {
            to: item,
            effect: Effect::Scale,
            timing: linear(0.0),
        };
        let start = scale.sample(0.0, CANVAS);
        assert_eq!(start, None, "fully transparent at the start");
        let half = scale.sample(500.0, CANVAS).expect("drawn");
        assert!((half.geometry.size.width.get() - 82.5).abs() < 1e-4);
        assert_eq!(centre(&half), (100.0, 540.0));

        // The left edge is nearest: it slides left until its right edge leaves.
        let slide = Track::Disappear {
            from: item,
            effect: Effect::Slide,
            timing: linear(0.0),
        };
        let almost = slide.sample(999.0, CANVAS).expect("drawn");
        assert!((centre(&almost).0 - (100.0 - 149.85)).abs() < 1e-2);

        // `None` waits for the item's start, then shows (or hides) it at once.
        let delayed = Timing {
            delay_ms: 200.0,
            ..linear(500.0)
        };
        let none = Track::Appear {
            to: item,
            effect: Effect::None,
            timing: delayed,
        };
        assert_eq!(none.sample(199.0, CANVAS), None, "not before its delay");
        assert_eq!(none.sample(200.0, CANVAS).map(|e| e.opacity), Some(1.0));
        assert_eq!(none.end_ms(), 200.0);
        let gone = Track::Disappear {
            from: item,
            effect: Effect::None,
            timing: delayed,
        };
        assert_eq!(gone.sample(199.0, CANVAS).map(|e| e.opacity), Some(1.0));
        assert_eq!(gone.sample(200.0, CANVAS), None, "gone from its delay");
    }

    #[test]
    fn a_rotated_box_slides_until_its_corners_are_off() {
        let mut item = at(1800.0, 540.0, 200.0, 100.0);
        item.geometry.rotation_deg = Finite32::new(90.0).expect("finite");
        // Rotated a quarter turn, its half-width across x is 50, so it ends at 1970.
        let moved = super::away(&item.geometry, 1.0, Effect::Slide, CANVAS);
        assert!((moved.center.x.get() - 1970.0).abs() < 1e-3);
    }

    #[test]
    fn a_zero_duration_jumps_at_its_delay() {
        let timing = Timing {
            delay_ms: 100.0,
            duration_ms: 0.0,
            easing: Easing::EaseInOut,
        };
        assert_eq!(timing.progress(99.0), 0.0);
        assert_eq!(timing.progress(100.0), 1.0);
    }

    /// A Move blends numbers and colours with its progress and switches every other
    /// differing value at the midpoint; a reference that differs can't blend (ADR-0127).
    #[test]
    fn effect_settings_blend_numbers_and_colours_and_switch_the_rest() {
        use super::{blend_settings, settings_blend};
        use jstream_protocol::Settings;
        use serde_json::json;
        let settings = |pairs: &[(&str, serde_json::Value)]| -> Settings {
            pairs.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        let from = settings(&[
            ("radius", json!(0.0)),
            ("steps", json!(2)),
            ("color", json!("#000000")),
            ("tint", json!("#00000000")),
            ("mode", json!("box")),
            ("same", json!(1)),
        ]);
        let to = settings(&[
            ("radius", json!(20.0)),
            ("steps", json!(5)),
            ("color", json!("#ff8000")),
            ("tint", json!("#ffffffff")),
            ("mode", json!("gauss")),
            ("same", json!(1)),
        ]);
        assert!(settings_blend(&from, &to));
        let whole = ["steps".to_owned()];
        let mid = blend_settings(&from, &to, 0.25, &whole);
        assert_eq!(mid.get("radius"), Some(&json!(5.0)));
        assert_eq!(
            mid.get("steps"),
            Some(&json!(3)),
            "an int parameter stays whole"
        );
        // A float parameter stored as whole numbers still blends smoothly (#1302 B1).
        let floats = |v: i64| settings(&[("strength", json!(v))]);
        let half = blend_settings(&floats(0), &floats(1), 0.5, &[]);
        assert_eq!(half.get("strength"), Some(&json!(0.5)));
        assert_eq!(mid.get("color"), Some(&json!("#402000")));
        assert_eq!(mid.get("tint"), Some(&json!("#40404040")));
        assert_eq!(mid.get("mode"), Some(&json!("box")), "before the midpoint");
        assert_eq!(
            blend_settings(&from, &to, 0.5, &whole).get("mode"),
            Some(&json!("gauss"))
        );
        // A spring's overshoot never takes a setting past its end.
        assert_eq!(blend_settings(&from, &to, 1.2, &whole), to);
        assert_eq!(blend_settings(&from, &to, 0.0, &whole), from);
        let scene = |id: &str| json!({"type": "scene", "scene_id": id});
        let a = settings(&[("mask", scene("a"))]);
        let b = settings(&[("mask", scene("b"))]);
        assert!(
            !settings_blend(&a, &b),
            "a reference that differs can't blend"
        );
        assert!(settings_blend(&a, &a));
        let none = settings(&[("mask", serde_json::Value::Null)]);
        assert!(!settings_blend(&none, &a));
    }
}
