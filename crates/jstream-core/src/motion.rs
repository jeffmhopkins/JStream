//! Motion modifiers as a shot draws them (ADR-0099 decision 3).
//!
//! Planning copies each drawn item's effective modifiers (its own, or a keyframe
//! scene's override) into the shot as `jstream-anim`'s model. The render thread
//! evaluates them as each frame is drawn, as pure functions of the frame's time on the
//! media clock (decision 2), and moves the item's box, crop and opacity by what they
//! give: positions, angles and crop edges add, scale and opacity multiply by `1 + v`.
//! The collection never changes. During a transition, a track that pairs two items
//! blends both sides' outputs by the track's eased progress, so a sway changes pace
//! across a Move without a jump.
//!
//! The `shown` clock counts from when an item was first drawn in an unbroken run of
//! plans: [`ShownSince`] keeps that time per item on the control side, and each shot
//! carries it.
//!
//! Docs: docs/adr/0099-native-motion-modifiers-and-timelines.md

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jstream_anim::{Modifier, Offsets};
use jstream_graph::Item as GraphItem;
use jstream_media::MediaTime;
use jstream_protocol::{Finite32, ItemId, Size, Vec2};
use jstream_render::transition::{Endpoint, Track};
use jstream_render::{Crop, Placement};

/// An item's modifiers, in order, shared by every place a shot holds them. Empty for
/// an item that doesn't move.
pub(crate) type Motion = Arc<[Modifier]>;

/// `item`'s modifiers, as the shot holds them.
pub(crate) fn of(item: &GraphItem) -> Motion {
    if item.motion().is_empty() {
        return Arc::from([]);
    }
    jstream_graph::anim_modifiers(item.motion()).into()
}

/// When each item a shot draws was first drawn, in the run of shots that have drawn it
/// since: the origin of its `shown` clock. An item that leaves the shot starts again
/// when it comes back.
#[derive(Debug, Default)]
pub(crate) struct ShownSince {
    since: BTreeMap<ItemId, MediaTime>,
}

impl ShownSince {
    /// Notes that `drawn` are the items a shot planned at `now` draws: each one new to
    /// it starts at `now`, and one no longer drawn is forgotten. Returns the times of
    /// the items `drawn` names, for the shot.
    pub(crate) fn update(
        &mut self,
        drawn: &BTreeSet<ItemId>,
        now: MediaTime,
    ) -> BTreeMap<ItemId, MediaTime> {
        self.since.retain(|item, _| drawn.contains(item));
        for item in drawn {
            self.since.entry(item.clone()).or_insert(now);
        }
        self.since.clone()
    }
}

/// What `motion` adds to an item at `at`, which was first shown at `shown`, if it is.
/// Allocates nothing.
pub(crate) fn offsets(motion: &[Modifier], at: MediaTime, shown: Option<MediaTime>) -> Offsets {
    if motion.is_empty() {
        return Offsets::IDENTITY;
    }
    let engine_s = at.as_secs_f64();
    let shown_s = shown.map(|since| (engine_s - since.as_secs_f64()).max(0.0));
    jstream_anim::evaluate(motion, engine_s, shown_s)
}

/// `placement`, `crop` and `opacity` moved by `by`. A legacy transform moves and turns
/// about its top-left corner and scales from it, and has no tilt, turn or depth to add
/// to.
/// A crop edge never goes below 0 and is rounded to whole source pixels.
pub(crate) fn apply(
    by: &Offsets,
    placement: Placement,
    crop: Crop,
    opacity: f32,
) -> (Placement, Crop, f32) {
    if by.is_identity() {
        return (placement, crop, opacity);
    }
    let placement = match placement {
        Placement::Geometry(geometry) => {
            let moved = (|| {
                let size = Size::try_new(
                    by.scale_of(geometry.size.width.get()),
                    by.scale_of(geometry.size.height.get()),
                )
                .ok()?;
                Some(jstream_protocol::ItemGeometry {
                    center: Vec2::new(
                        geometry.center.x.get() + by.x,
                        geometry.center.y.get() + by.y,
                    )?,
                    size,
                    rotation_deg: Finite32::new(geometry.rotation_deg.get() + by.rotation)?,
                    tilt_deg: Finite32::new(geometry.tilt_deg.get() + by.tilt)?,
                    turn_deg: Finite32::new(geometry.turn_deg.get() + by.turn)?,
                    depth_px: Finite32::new(geometry.depth_px.get() + by.depth)?,
                    ..geometry
                })
            })();
            // A scale that shrinks it to nothing, or a value that isn't finite, leaves
            // it where it is.
            Placement::Geometry(moved.unwrap_or(geometry))
        }
        Placement::Legacy(mut transform) => {
            transform.position[0] += by.x;
            transform.position[1] += by.y;
            transform.scale[0] = by.scale_of(transform.scale[0]);
            transform.scale[1] = by.scale_of(transform.scale[1]);
            transform.rotation_deg += by.rotation;
            Placement::Legacy(transform)
        }
        // A model's drawn layer (ADR-0107) is already placed in its pixels, drawn flat
        // from the canvas's top-left; its geometry only sorts it, so motion leaves it.
        layer @ Placement::Layer(_) => layer,
    };
    let edge = |px: u32, add: f32| (px as f32 + add).round().max(0.0) as u32;
    let crop = Crop {
        left: edge(crop.left, by.crop[0]),
        top: edge(crop.top, by.crop[1]),
        right: edge(crop.right, by.crop[2]),
        bottom: edge(crop.bottom, by.crop[3]),
    };
    (placement, crop, by.opacity_of(opacity))
}

/// A track's endpoint moved by `by`: [`apply`] on its box, crop and opacity.
pub(crate) fn apply_endpoint(by: &Offsets, at: Endpoint) -> Endpoint {
    let (placement, crop, opacity) =
        apply(by, Placement::Geometry(at.geometry), at.crop, at.opacity);
    let Placement::Geometry(geometry) = placement else {
        return at;
    };
    Endpoint {
        geometry,
        crop,
        opacity,
        ..at
    }
}

/// The modifiers of the two items a transition's track moves between: the item it
/// leaves and the one it arrives at, each with its ID, for its `shown` clock. An item
/// that isn't moving between two, and a track with only one end, has the same on both
/// sides, and is drawn with `to`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackMotion {
    pub(crate) from: (ItemId, Motion),
    pub(crate) to: (ItemId, Motion),
}

impl TrackMotion {
    /// One item's modifiers on both sides.
    pub(crate) fn same(item: ItemId, motion: Motion) -> Self {
        Self {
            from: (item.clone(), motion.clone()),
            to: (item, motion),
        }
    }

    /// Whether neither side moves.
    pub(crate) fn is_still(&self) -> bool {
        self.from.1.is_empty() && self.to.1.is_empty()
    }

    /// What it adds to `track`'s item `elapsed_ms` into the transition, at `at`: the
    /// leaving side's alone before a pair starts to move and the arriving side's once it
    /// has, blended by the track's eased progress in between. An appearing track takes
    /// the arriving side's and a disappearing one the leaving side's.
    pub(crate) fn offsets(
        &self,
        track: &Track,
        elapsed_ms: f32,
        at: MediaTime,
        shown: impl Fn(&ItemId) -> Option<MediaTime>,
    ) -> Offsets {
        if self.is_still() {
            return Offsets::IDENTITY;
        }
        let side = |(item, motion): &(ItemId, Motion)| offsets(motion, at, shown(item));
        match track {
            Track::Pair { timing, .. } => {
                let k = timing.progress(elapsed_ms).clamp(0.0, 1.0);
                if k <= 0.0 {
                    side(&self.from)
                } else if k >= 1.0 {
                    side(&self.to)
                } else {
                    side(&self.from).blend(&side(&self.to), k)
                }
            }
            Track::Appear { .. } | Track::Through { .. } => side(&self.to),
            Track::Disappear { .. } => side(&self.from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_anim::{Clock, Kind, Property};
    use jstream_protocol::{Easing, ItemGeometry};
    use jstream_render::LegacyTransform;
    use jstream_render::transition::Timing;

    fn drift(property: Property, rate_per_s: f64, clock: Clock) -> Modifier {
        Modifier {
            id: format!("{property:?}"),
            property,
            kind: Kind::Drift {
                rate_per_s,
                wrap: None,
            },
            clock,
            enabled: true,
        }
    }

    fn at(secs: f64) -> MediaTime {
        MediaTime::new((secs * 1e6) as i64, jstream_media::Timebase::MICROS)
    }

    fn geometry() -> ItemGeometry {
        ItemGeometry::try_new(
            Vec2::new(100.0, 50.0).expect("finite"),
            Size::try_new(40.0, 20.0).expect("size"),
        )
        .expect("geometry")
    }

    #[test]
    fn each_property_moves_its_part_of_the_box() {
        let motion = [
            drift(Property::X, 3.0, Clock::Engine),
            drift(Property::Y, -2.0, Clock::Engine),
            drift(Property::Scale, 0.5, Clock::Engine),
            drift(Property::Rotation, 10.0, Clock::Engine),
            drift(Property::Tilt, 1.0, Clock::Engine),
            drift(Property::Turn, -1.0, Clock::Engine),
            drift(Property::Depth, 4.0, Clock::Engine),
            drift(Property::Opacity, -0.25, Clock::Engine),
            drift(Property::CropLeft, 2.4, Clock::Engine),
            drift(Property::CropRight, -9.0, Clock::Engine),
        ];
        let by = offsets(&motion, at(2.0), None);
        let crop = Crop {
            left: 1,
            top: 2,
            right: 3,
            bottom: 4,
        };
        let (placement, crop, opacity) = apply(&by, Placement::Geometry(geometry()), crop, 0.8);
        let Placement::Geometry(moved) = placement else {
            panic!("a geometry stays one");
        };
        assert_eq!((moved.center.x.get(), moved.center.y.get()), (106.0, 46.0));
        assert_eq!(
            (moved.size.width.get(), moved.size.height.get()),
            (80.0, 40.0)
        );
        assert_eq!(
            (
                moved.rotation_deg.get(),
                moved.tilt_deg.get(),
                moved.turn_deg.get()
            ),
            (20.0, 2.0, -2.0)
        );
        assert_eq!(moved.depth_px.get(), 8.0);
        // 1 + 4.8 rounds to 6; 3 - 18 stops at 0.
        assert_eq!(
            crop,
            Crop {
                left: 6,
                top: 2,
                right: 0,
                bottom: 4
            }
        );
        assert!((opacity - 0.4).abs() < 1e-6, "{opacity}");
    }

    #[test]
    fn no_motion_changes_nothing_and_a_collapse_keeps_the_box() {
        let crop = Crop::NONE;
        let still = offsets(&[], at(5.0), None);
        let placement = Placement::Geometry(geometry());
        assert_eq!(apply(&still, placement, crop, 0.5), (placement, crop, 0.5));
        // Scale by 1 + (-1) is nothing: the box stays.
        let gone = offsets(
            &[drift(Property::Scale, -1.0, Clock::Engine)],
            at(1.0),
            None,
        );
        assert_eq!(apply(&gone, placement, crop, 1.0).0, placement);
    }

    #[test]
    fn a_legacy_transform_moves_from_its_corner() {
        let by = offsets(
            &[
                drift(Property::X, 1.0, Clock::Engine),
                drift(Property::Scale, 1.0, Clock::Engine),
                drift(Property::Tilt, 9.0, Clock::Engine),
            ],
            at(1.0),
            None,
        );
        let (placement, _, _) = apply(
            &by,
            Placement::Legacy(LegacyTransform::IDENTITY),
            Crop::NONE,
            1.0,
        );
        let Placement::Legacy(transform) = placement else {
            panic!("a legacy transform stays one");
        };
        assert_eq!(transform.position, [1.0, 0.0]);
        assert_eq!(transform.scale, [2.0, 2.0]);
        assert_eq!(transform.rotation_deg, 0.0);
    }

    #[test]
    fn the_shown_clock_counts_from_when_the_item_was_first_drawn() {
        let motion = [drift(Property::X, 10.0, Clock::Shown)];
        // Never shown: at its start.
        assert_eq!(offsets(&motion, at(30.0), None).x, 0.0);
        assert_eq!(offsets(&motion, at(30.0), Some(at(28.5))).x, 15.0);

        let mut since = ShownSince::default();
        let a = ItemId::new("a");
        let b = ItemId::new("b");
        let both: BTreeSet<_> = [a.clone(), b.clone()].into();
        let shown = since.update(&both, at(1.0));
        assert_eq!(shown.get(&a), Some(&at(1.0)));
        // Still drawn: keeps its time; a newcomer starts now.
        let shown = since.update(&both, at(4.0));
        assert_eq!(shown.get(&b), Some(&at(1.0)));
        // Gone, then back: starts again.
        since.update(&[a.clone()].into(), at(5.0));
        let shown = since.update(&both, at(6.0));
        assert_eq!(
            (shown.get(&a), shown.get(&b)),
            (Some(&at(1.0)), Some(&at(6.0)))
        );
    }

    #[test]
    fn a_pair_blends_both_sides_by_its_eased_progress() {
        let timing = Timing {
            delay_ms: 0.0,
            duration_ms: 1000.0,
            easing: Easing::Linear,
        };
        let end = Endpoint {
            geometry: geometry(),
            crop: Crop::NONE,
            opacity: 1.0,
            blend: jstream_protocol::BlendMode::Normal,
        };
        let pair = Track::Pair {
            from: end,
            to: end,
            timing,
            source: None,
        };
        let motion = TrackMotion {
            from: (
                ItemId::new("i"),
                Arc::from([drift(Property::X, 10.0, Clock::Engine)]),
            ),
            to: (
                ItemId::new("i"),
                Arc::from([drift(Property::X, 30.0, Clock::Engine)]),
            ),
        };
        let x = |elapsed| motion.offsets(&pair, elapsed, at(1.0), |_| None).x;
        assert_eq!(x(0.0), 10.0);
        assert_eq!(x(500.0), 20.0);
        assert_eq!(x(1000.0), 30.0);
        let appear = Track::Appear {
            to: end,
            effect: jstream_render::transition::Effect::Fade,
            timing,
        };
        assert_eq!(motion.offsets(&appear, 0.0, at(1.0), |_| None).x, 30.0);
        assert!(TrackMotion::same(ItemId::new("i"), Arc::from([])).is_still());
    }
}
