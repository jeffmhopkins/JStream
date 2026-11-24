//! What the Sequence panel's Add lane search offers beside the channels (A13, SQ-01):
//! "Settings (makes a channel)", every live number setting of the scene's sources and
//! their filters and each audio source's fader, and "Switches", every on/off setting of
//! the same. Picking one makes a channel the setting follows ([ADR-0183] decision 5),
//! or reuses the channel it follows already, and gives that channel a lane.
//!
//! A switch lane is an ordinary lane whose points all use the `hold` curve at 0 or 1
//! ([ADR-0184] decision 1): it is drawn as bars where it is on, off before the first,
//! and edited as bars (`insert_bar`, `drag_bar`) written back as points (`bar_points`),
//! so editing one never makes it a curve.
//!
//! [ADR-0183]: ../../../../docs/adr/0183-virtual-control-channels.md
//! [ADR-0184]: ../../../../docs/adr/0184-sequences-lanes-and-triggers.md

use crate::panels::learn::{FADER_THROW, fraction, param_range};
use crate::store::Collection;
use jstream_control::protocol::{
    FilterOwner, Finite32, KindDescriptor, LaneCurve, LanePoint, MotionKind, ParamType, SceneId,
    SourceKind, TimelineLane, ValueTarget,
};
use std::collections::HashMap;

/// One setting Add lane offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    /// What the list shows: "Title › Blur › Radius".
    pub label: String,
    /// The setting.
    pub target: ValueTarget,
    /// The range the channel's 0 to 1 maps onto.
    pub range: (f64, f64),
    /// Whether it is on or off, so it goes under Switches and gets a switch lane.
    pub switch: bool,
    /// Where the setting is now in `range`, 0 to 1: the new channel's default, so the
    /// setting doesn't move when it starts following it.
    pub at: f32,
}

/// The kinds whose descriptors [`offers`] reads for `scene`: its sources' and their
/// filters', each once.
pub fn kinds_needed(collection: &Collection, scene: &SceneId) -> Vec<String> {
    let mut kinds = Vec::new();
    for source in sources_of(collection, scene) {
        if let SourceKind::Builtin { kind } = &source.kind {
            kinds.push(kind.clone());
        }
        for f in source.filters.video.iter().chain(&source.filters.audio) {
            kinds.push(f.kind.clone());
        }
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

/// The sources placed in `scene`, each once, in stacking order.
fn sources_of<'a>(
    collection: &'a Collection,
    scene: &SceneId,
) -> Vec<&'a jstream_control::protocol::SourceInfo> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for item in collection.scene(scene).map_or(&[][..], |s| &s.items[..]) {
        if seen.contains(&item.source_id) {
            continue;
        }
        seen.push(item.source_id.clone());
        if let Some(source) = collection.source(&item.source_id) {
            out.push(source);
        }
    }
    out
}

/// Every setting of `scene`'s sources Add lane can give a lane, from the descriptors
/// read so far: each live number or on/off setting of a source and of its filters, and
/// each audio source's fader. A kind not yet described offers nothing yet.
pub fn offers(
    collection: &Collection,
    scene: &SceneId,
    descriptors: &HashMap<String, KindDescriptor>,
) -> Vec<Offer> {
    let mut out = Vec::new();
    for source in sources_of(collection, scene) {
        let name = &source.name;
        if let SourceKind::Builtin { kind } = &source.kind
            && let Some(d) = descriptors.get(kind)
        {
            for p in &d.params {
                let Some(range) = param_range(p) else {
                    continue;
                };
                let value = source.settings.get(&p.id).unwrap_or(&p.default);
                out.push(Offer {
                    label: format!("{name} › {}", p.label),
                    target: ValueTarget::SourceSetting {
                        source_id: source.source_id.clone(),
                        param: p.id.clone(),
                    },
                    range,
                    switch: p.ty == ParamType::Bool,
                    at: at_of(value, range),
                });
            }
        }
        for f in source.filters.video.iter().chain(&source.filters.audio) {
            let Some(d) = descriptors.get(&f.kind) else {
                continue;
            };
            for p in &d.params {
                let Some(range) = param_range(p) else {
                    continue;
                };
                let value = f.settings.get(&p.id).unwrap_or(&p.default);
                out.push(Offer {
                    label: format!("{name} › {} › {}", d.name, p.label),
                    target: ValueTarget::FilterSetting {
                        owner: FilterOwner::Source {
                            source_id: source.source_id.clone(),
                        },
                        filter_id: f.filter_id.clone(),
                        param: p.id.clone(),
                    },
                    range,
                    switch: p.ty == ParamType::Bool,
                    at: at_of(value, range),
                });
            }
        }
        if let Some(audio) = source.audio.as_ref().filter(|a| a.enabled) {
            out.push(Offer {
                label: format!("{name} › Fader"),
                target: ValueTarget::SourceGain {
                    source_id: source.source_id.clone(),
                },
                range: FADER_THROW,
                switch: false,
                at: fraction(f64::from(audio.gain_db.get()), FADER_THROW),
            });
        }
    }
    out
}

/// A setting's value as a fraction of `range`: a number, or a bool as 0 or 1.
fn at_of(value: &serde_json::Value, range: (f64, f64)) -> f32 {
    match value {
        serde_json::Value::Bool(on) => f32::from(u8::from(*on)),
        other => other.as_f64().map_or(0.0, |v| fraction(v, range)),
    }
}

/// Whether `offer` matches the search `query`, by any part of its label.
pub fn matches(offer: &Offer, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || offer.label.to_lowercase().contains(&q)
}

/// The channel `target` follows already through a value binding, if it does.
pub fn followed_channel(collection: &Collection, target: &ValueTarget) -> Option<String> {
    collection
        .value_bindings
        .iter()
        .filter(|b| b.target == *target)
        .find_map(|b| match &b.input {
            MotionKind::Channel { channel_id, .. } => Some(channel_id.clone()),
            _ => None,
        })
}

/// Whether `lane` is a switch lane: it has points, and every one jumps (`hold`) to 0 or
/// 1.
pub fn is_switch(lane: &TimelineLane) -> bool {
    !lane.points.is_empty()
        && lane
            .points
            .iter()
            .all(|p| p.curve == LaneCurve::Hold && (p.value.get() == 0.0 || p.value.get() == 1.0))
}

/// A switch lane's points for a setting that is `on` now: one point at the end that
/// holds it there, so the lane changes nothing until a bar is added.
pub fn switch_points(on: bool, duration_ms: u32) -> Vec<LanePoint> {
    vec![hold(duration_ms, on)]
}

fn hold(at_ms: u32, on: bool) -> LanePoint {
    LanePoint {
        at_ms,
        value: if on { Finite32::ONE } else { Finite32::ZERO },
        curve: LaneCurve::Hold,
    }
}

/// Whether a switch lane is on at `at_ms`: the last point at or before it, or `from`
/// (the channel's state as the sequence starts) before the first.
pub fn on_at(points: &[LanePoint], at_ms: u32, from: bool) -> bool {
    points
        .iter()
        .rev()
        .find(|p| p.at_ms <= at_ms)
        .map_or(from, |p| p.value.get() >= 0.5)
}

/// The stretches a switch lane is on, as `(from_ms, to_ms)`, over `0..duration_ms`.
pub fn switch_bars(points: &[LanePoint], duration_ms: u32, from: bool) -> Vec<(u32, u32)> {
    let mut bars = Vec::new();
    let mut start = from.then_some(0);
    for p in points.iter().filter(|p| p.at_ms <= duration_ms) {
        let on = p.value.get() >= 0.5;
        match (start, on) {
            (None, true) => start = Some(p.at_ms),
            (Some(s), false) => {
                if p.at_ms > s {
                    bars.push((s, p.at_ms));
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start.filter(|s| *s < duration_ms) {
        bars.push((s, duration_ms));
    }
    bars
}

/// Turns a switch lane on from `at_ms` for `len_ms` (kept within the length), then
/// back to whatever it was there before.
pub fn add_bar(points: &mut Vec<LanePoint>, at_ms: u32, len_ms: u32, duration_ms: u32, from: bool) {
    let at_ms = at_ms.min(duration_ms.saturating_sub(1));
    let end = at_ms.saturating_add(len_ms.max(1)).min(duration_ms);
    let after = on_at(points, end, from);
    points.retain(|p| p.at_ms < at_ms || p.at_ms > end);
    let n = points.partition_point(|p| p.at_ms < at_ms);
    points.insert(n, hold(at_ms, true));
    if !after {
        points.insert(n + 1, hold(end, false));
    }
}

/// The shortest bar a switch lane keeps: a tenth of a second.
pub const MIN_BAR_MS: u32 = 100;

/// Which part of a bar a drag holds: the whole bar, or one of its ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    /// The bar, moved without changing its length.
    Body,
    /// Its start, where it switches on.
    Start,
    /// Its end, where it switches off.
    End,
}

/// A switch lane's points for `bars` (in time order, apart or touching): off from 0 up
/// to the first bar, on at each bar's start and off at its end, so the lane is the same
/// whatever its channel was when the sequence starts. Touching bars join. No bars: one
/// point at the end that switches it off, as a new lane changes nothing before it.
pub fn bar_points(bars: &[(u32, u32)], duration_ms: u32) -> Vec<LanePoint> {
    let mut joined: Vec<(u32, u32)> = Vec::new();
    for &(a, b) in bars.iter().filter(|(a, b)| b > a && *a < duration_ms) {
        match joined.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => joined.push((a, b.min(duration_ms))),
        }
    }
    if joined.is_empty() {
        return vec![hold(duration_ms, false)];
    }
    let mut points = Vec::new();
    if joined[0].0 > 0 {
        points.push(hold(0, false));
    }
    for (a, b) in joined {
        points.push(hold(a, true));
        if b < duration_ms {
            points.push(hold(b, false));
        }
    }
    points
}

/// The room bar `n` (or a new bar at `n`) has: from [`MIN_BAR_MS`] after the bar before
/// it, or 0, to [`MIN_BAR_MS`] before the bar after it, or the end. Bars never touch,
/// so two never join into one by being dragged together.
fn room(bars: &[(u32, u32)], n: usize, next: usize, duration_ms: u32) -> (u32, u32) {
    let lo = n
        .checked_sub(1)
        .and_then(|p| bars.get(p))
        .map_or(0, |p| p.1 + MIN_BAR_MS);
    let hi = bars
        .get(next)
        .map_or(duration_ms, |p| p.0.saturating_sub(MIN_BAR_MS));
    (lo, hi.max(lo))
}

/// Where bar `n` of `bars` goes when `grip` is dragged to `at_ms` (for the body: its
/// start goes there). It stays in its [room](room) and in `0..duration_ms`, and keeps
/// [`MIN_BAR_MS`]; moved whole it keeps its length.
pub fn drag_bar(
    bars: &[(u32, u32)],
    n: usize,
    grip: Grip,
    at_ms: u32,
    duration_ms: u32,
) -> (u32, u32) {
    let Some(&(a, b)) = bars.get(n) else {
        return (0, 0);
    };
    let (lo, hi) = room(bars, n, n + 1, duration_ms);
    match grip {
        Grip::Body => {
            let len = b - a;
            let start = at_ms.clamp(lo, hi.saturating_sub(len).max(lo));
            (start, (start + len).min(hi))
        }
        Grip::Start => (at_ms.clamp(lo, b.saturating_sub(MIN_BAR_MS).max(lo)), b),
        Grip::End => (a, at_ms.clamp((a + MIN_BAR_MS).min(hi), hi)),
    }
}

/// `bars` with a new bar `len_ms` long from `at_ms`, kept in the gap's [room](room)
/// (so cut short before the next bar and the end), and its place among them; `None`
/// when `at_ms` is in a bar or the gap has no room for [`MIN_BAR_MS`].
pub fn insert_bar(
    bars: &[(u32, u32)],
    at_ms: u32,
    len_ms: u32,
    duration_ms: u32,
) -> Option<(Vec<(u32, u32)>, usize)> {
    if bars.iter().any(|&(a, b)| at_ms >= a && at_ms < b) {
        return None;
    }
    let n = bars.partition_point(|&(a, _)| a < at_ms);
    let (lo, hi) = room(bars, n, n, duration_ms);
    let start = at_ms.max(lo);
    let end = start.saturating_add(len_ms).min(hi);
    if end < start + MIN_BAR_MS {
        return None;
    }
    let mut out = bars.to_vec();
    out.insert(n, (start, end));
    Some((out, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{
        ChannelInfo, Crop, FilterChains, FilterId, FilterInfo, ItemId, ItemInfo, ItemTransform,
        KindCategory, ParamDescriptor, PlacementForm, SceneInfo, SourceId, SourceInfo,
        ValueBinding,
    };
    use serde_json::json;

    fn param(id: &str, ty: ParamType, live: bool) -> ParamDescriptor {
        ParamDescriptor {
            warning: None,
            id: id.into(),
            ty,
            default: json!(0),
            label: id.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live,
        }
    }

    fn kind(id: &str, params: Vec<ParamDescriptor>) -> KindDescriptor {
        KindDescriptor {
            kind_id: id.into(),
            category: KindCategory::Source,
            name: id.into(),
            version: 1,
            video: true,
            audio: false,
            max_outset_px: None,
            params,
        }
    }

    #[test]
    fn a_switch_lane_is_bars_where_it_is_on() {
        let mut points = switch_points(false, 6000);
        assert_eq!(switch_bars(&points, 6000, false), vec![]);
        add_bar(&mut points, 2000, 1000, 6000, false);
        assert_eq!(switch_bars(&points, 6000, false), vec![(2000, 3000)]);
        let lane = TimelineLane {
            target: jstream_control::protocol::LaneTarget {
                channel: "c".into(),
            },
            points: points.clone(),
            on: true,
        };
        assert!(is_switch(&lane));
        // A bar over the end stops at it, and joins a bar it reaches.
        add_bar(&mut points, 5500, 1000, 6000, false);
        assert_eq!(
            switch_bars(&points, 6000, false),
            vec![(2000, 3000), (5500, 6000)]
        );
        add_bar(&mut points, 2500, 1000, 6000, false);
        assert_eq!(
            switch_bars(&points, 6000, false),
            vec![(2000, 3500), (5500, 6000)]
        );
        // Starting on, it is on until its first point turns it off.
        assert_eq!(
            switch_bars(&switch_points(false, 6000), 6000, true),
            vec![(0, 6000)]
        );
    }

    #[test]
    fn bars_are_dragged_between_their_neighbours() {
        let bars = vec![(1000, 2000), (4000, 5000)];
        assert_eq!(drag_bar(&bars, 0, Grip::Body, 2500, 6000), (2500, 3500));
        assert_eq!(
            drag_bar(&bars, 0, Grip::Body, 3500, 6000),
            (2900, 3900),
            "stops a tenth before the next bar"
        );
        assert_eq!(
            drag_bar(&bars, 1, Grip::Body, 5800, 6000),
            (5000, 6000),
            "and at the end"
        );
        assert_eq!(drag_bar(&bars, 1, Grip::Body, 100, 6000), (2100, 3100));
        assert_eq!(
            drag_bar(&bars, 0, Grip::Start, 1950, 6000),
            (1900, 2000),
            "keeps a tenth"
        );
        assert_eq!(drag_bar(&bars, 1, Grip::Start, 500, 6000), (2100, 5000));
        assert_eq!(drag_bar(&bars, 0, Grip::End, 4500, 6000), (1000, 3900));
        assert_eq!(drag_bar(&bars, 1, Grip::End, 9000, 6000), (4000, 6000));
        assert_eq!(drag_bar(&bars, 1, Grip::End, 0, 6000), (4000, 4100));
    }

    #[test]
    fn a_bar_is_added_only_in_room_for_it() {
        let bars = vec![(1000, 2000)];
        assert_eq!(insert_bar(&bars, 1500, 1000, 6000), None, "in a bar");
        assert_eq!(
            insert_bar(&bars, 0, 1000, 6000),
            Some((vec![(0, 900), (1000, 2000)], 0)),
            "cut short"
        );
        assert_eq!(insert_bar(&bars, 850, 1000, 6000), None, "no room");
        assert_eq!(
            insert_bar(&bars, 2000, 1000, 6000),
            Some((vec![(1000, 2000), (2100, 3100)], 1)),
            "kept apart"
        );
        assert_eq!(
            insert_bar(&bars, 5500, 1000, 6000),
            Some((vec![(1000, 2000), (5500, 6000)], 1))
        );
        assert_eq!(insert_bar(&bars, 5950, 1000, 6000), None);
    }

    #[test]
    fn bar_points_are_the_bars_whatever_the_channel_was() {
        let bars = vec![(1000, 2000), (2000, 3000), (5000, 6000)];
        let points = bar_points(&bars, 6000);
        let lane = TimelineLane {
            target: jstream_control::protocol::LaneTarget {
                channel: "c".into(),
            },
            points: points.clone(),
            on: true,
        };
        assert!(is_switch(&lane));
        assert_eq!(
            points
                .iter()
                .map(|p| (p.at_ms, p.value.get()))
                .collect::<Vec<_>>(),
            vec![(0, 0.0), (1000, 1.0), (3000, 0.0), (5000, 1.0)],
            "touching bars join"
        );
        for from in [false, true] {
            assert_eq!(
                switch_bars(&points, 6000, from),
                vec![(1000, 3000), (5000, 6000)]
            );
        }
        assert_eq!(
            switch_bars(&bar_points(&[(0, 500)], 6000), 6000, true),
            vec![(0, 500)]
        );
        let none = bar_points(&[], 6000);
        assert_eq!(none.len(), 1);
        assert_eq!(switch_bars(&none, 6000, false), vec![]);
    }

    #[test]
    fn a_curve_is_not_a_switch() {
        let lane = TimelineLane {
            target: jstream_control::protocol::LaneTarget {
                channel: "c".into(),
            },
            points: vec![LanePoint {
                at_ms: 100,
                value: Finite32::ONE,
                curve: LaneCurve::Linear,
            }],
            on: true,
        };
        assert!(!is_switch(&lane));
    }

    #[test]
    fn the_scenes_settings_are_offered_by_kind() {
        let mut c = Collection::default();
        let mut scene: SceneInfo =
            serde_json::from_value(json!({"scene_id": "s", "name": "S", "items": []}))
                .unwrap_or_else(|e| panic!("a scene: {e}"));
        let item = ItemInfo {
            motion: Default::default(),
            item_id: ItemId::new("i"),
            source_id: SourceId::new("title"),
            placement: PlacementForm::Legacy,
            transform: ItemTransform::IDENTITY,
            geometry: None,
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: Default::default(),
            locked: false,
            effects_space: jstream_protocol::EffectsSpace::Box,
        };
        scene.items.push(item);
        c.scenes.push(scene);
        let mut source = SourceInfo {
            source_id: SourceId::new("title"),
            name: "Title".into(),
            kind: SourceKind::Builtin {
                kind: "text".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
            audio: None,
            filters: Default::default(),
            meta: Default::default(),
        };
        source.settings.insert("size", json!(50));
        source.filters = FilterChains {
            video: vec![FilterInfo {
                filter_id: FilterId::new("blur"),
                kind: "blur".into(),
                enabled: true,
                settings: Default::default(),
            }],
            audio: Vec::new(),
        };
        c.sources.push(source);
        let mut ds = HashMap::new();
        ds.insert(
            "text".to_owned(),
            kind(
                "text",
                vec![
                    param(
                        "size",
                        ParamType::Int {
                            min: Some(0),
                            max: Some(100),
                            step: None,
                            unit: None,
                        },
                        true,
                    ),
                    param("bold", ParamType::Bool, true),
                    param("font", ParamType::Bool, false),
                ],
            ),
        );
        let scene_id = SceneId::new("s");
        assert_eq!(kinds_needed(&c, &scene_id), vec!["blur", "text"]);
        let found = offers(&c, &scene_id, &ds);
        let labels: Vec<_> = found.iter().map(|o| (o.label.as_str(), o.switch)).collect();
        assert_eq!(
            labels,
            vec![("Title › size", false), ("Title › bold", true)]
        );
        assert_eq!(found[0].at, 0.5, "where the size is now");
        assert!(matches(&found[0], "TITLE"));
        assert!(!matches(&found[0], "blur"));

        assert_eq!(followed_channel(&c, &found[0].target), None);
        c.channels.push(ChannelInfo::new("glow"));
        c.value_bindings.push(ValueBinding {
            binding_id: "b".into(),
            input: MotionKind::Channel {
                channel_id: "glow".into(),
                smoothing_ms: 0,
                curve: jstream_control::protocol::MotionCurve::Linear,
                from: Finite32::ZERO,
                to: Finite32::ONE,
            },
            target: found[0].target.clone(),
            amount: None,
        });
        assert_eq!(
            followed_channel(&c, &found[0].target).as_deref(),
            Some("glow")
        );
    }
}
