//! What a transition draws at a moment (ADR-0029 §5): each item's placement, crop and
//! opacity, interpolated between two scenes.
//!
//! The engine pairs items and plans the transition on its control thread, as a list of
//! [`Track`]s in drawing order. The render thread only samples them: [`Track::sample`]
//! is a pure function of the time since the transition started, so every frame, and a
//! `video.snapshot` at the same moment, draws the same thing.
//!
//! - A **pair** interpolates position, size, crop, opacity, rotation, tilt and turn,
//!   depth and perspective. Angles take the shortest way round; the rest are linear.
//! - An item **without a partner** appears or disappears with its effect: `fade`
//!   (opacity only), `scale` (toward 65% about its centre while fading), `slide` (toward
//!   the nearest canvas edge until it is off the canvas, while fading) or `none` (at
//!   once).
//! - **Clamping:** a spring overshoots by design, so position, size, rotation and depth
//!   may pass their end values. Values with a hard range are clamped after easing:
//!   opacity to `[0, 1]`, crop to `[0, source size]`, size to `≥ 0` (an empty box draws
//!   nothing) and perspective to `> 0`.
//!
//! Docs: docs/adr/0029-transitions.md

use crate::scene::Crop;
use jstream_protocol::{Easing, Finite32, ItemGeometry, Size, Vec2};

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

/// One item's movement over a transition.
#[derive(Clone, Copy, Debug, PartialEq)]
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
        }
    }

    /// The item `elapsed_ms` after the transition started, on a canvas of `canvas`
    /// pixels, or `None` when it draws nothing then (fully transparent, or an empty
    /// box).
    pub fn sample(&self, elapsed_ms: f32, canvas: [f32; 2]) -> Option<Endpoint> {
        let sampled = match self {
            Track::Pair {
                from,
                to,
                timing,
                source,
            } => {
                let k = timing.progress(elapsed_ms);
                Endpoint {
                    geometry: lerp_geometry(&from.geometry, &to.geometry, k),
                    crop: lerp_crop(from.crop, to.crop, k, *source),
                    opacity: lerp(from.opacity, to.opacity, k.clamp(0.0, 1.0)),
                }
            }
            Track::Appear { to, effect, timing } => {
                let k = match effect {
                    Effect::None => timing.step(elapsed_ms),
                    _ => timing.progress(elapsed_ms).clamp(0.0, 1.0),
                };
                Endpoint {
                    geometry: away(&to.geometry, 1.0 - k, *effect, canvas),
                    crop: to.crop,
                    opacity: to.opacity * k,
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
                    geometry: away(&from.geometry, k, *effect, canvas),
                    crop: from.crop,
                    opacity: from.opacity * (1.0 - k.clamp(0.0, 1.0)),
                }
            }
        };
        let opacity = sampled.opacity.clamp(0.0, 1.0);
        let size = sampled.geometry.size;
        let drawn = opacity > 0.0 && size.width.get() > 0.0 && size.height.get() > 0.0;
        drawn.then_some(Endpoint { opacity, ..sampled })
    }
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
    let angle = |x: Finite32, y: Finite32| finite(lerp_angle(x.get(), y.get(), k));
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
        rotation_deg: angle(a.rotation_deg, b.rotation_deg),
        tilt_deg: angle(a.tilt_deg, b.tilt_deg),
        turn_deg: angle(a.turn_deg, b.turn_deg),
        depth_px: num(a.depth_px, b.depth_px),
        perspective_px,
    }
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
    use jstream_protocol::{Easing, Finite32, ItemGeometry, Size, Vec2};

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
        };
        let before = track.sample(499.0, CANVAS).expect("drawn");
        assert!(!before.geometry.flip_h && !before.geometry.flip_v);
        let after = track.sample(500.0, CANVAS).expect("drawn");
        assert!(after.geometry.flip_h && after.geometry.flip_v);
    }

    #[test]
    fn angles_take_the_shortest_way() {
        assert_eq!(lerp_angle(350.0, 10.0, 0.5), 360.0);
        assert_eq!(lerp_angle(10.0, 350.0, 0.5), 0.0);
        assert_eq!(lerp_angle(0.0, 90.0, 0.5), 45.0);
        assert_eq!(lerp_angle(-170.0, 170.0, 0.5), -180.0);
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
}
