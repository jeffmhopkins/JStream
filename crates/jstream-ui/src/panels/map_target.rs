//! What a control is mapped onto (UI-FEATURES VC-03, VC-04): a setting a value binding
//! drives (ADR-0178), or one of an item's placement properties, which motion modifiers
//! drive (ADR-0099, ADR-0171). The Map control dialog and mapping mode write either
//! through [`MapTarget`], so both kinds map the same way.
//!
//! Either kind takes one mapping per source, and each mapping is an offset or an
//! override (ADR-0203). An offset adds to what is stored, so any number of them stack:
//! its ends are how far it moves the value, which [`ItemUnit`] says how to show for a
//! property. An override is the value itself, and a target keeps one: a setting's is
//! the binding named after it ([`learn_id`]), so the control button and older
//! collections agree on it. A mapping can be bypassed without being removed.
//!
//! Docs: docs/UI-FEATURES.md#automation, docs/adr/0178-value-bindings-drive-any-number.md

use crate::panels::learn::learn_id;
use crate::panels::map_control::{ends, same_source};
use crate::store::Collection;
use jstream_control::protocol::{
    Edit, ItemId, MappingMode, MotionClock, MotionKind, MotionModifier, MotionProperty, SceneId,
    ValueBinding, ValueTarget,
};

/// What a mapping drives.
#[derive(Clone, Debug, PartialEq)]
pub enum MapTarget {
    /// A setting a value binding drives.
    Value(ValueTarget),
    /// A placement property of an item, as a scene shows it.
    Item {
        /// The scene shown: the item's own, or a keyframe of it.
        scene: SceneId,
        /// The item.
        item: ItemId,
        /// The property.
        property: MotionProperty,
    },
}

/// The mark a field carries when a control can be mapped onto it.
#[derive(Clone, Debug)]
pub struct MapMark {
    /// The target's [`MapTarget::id`].
    pub id: String,
    /// How far its mappings move it, as a tag on the field; `None` for no tag.
    pub amount: leptos::prelude::Signal<Option<String>>,
}

/// One mapping onto a target.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    /// The binding's or the modifier's ID.
    pub key: String,
    /// What it reads, with its ends.
    pub input: MotionKind,
    /// The channel that scales it, if any.
    pub amount: Option<String>,
    /// Whether it adds to the value (an offset) or is the value (an override).
    pub mode: MappingMode,
    /// Whether it is in use; `false` while it is bypassed.
    pub enabled: bool,
}

impl Mapping {
    /// Whether it is an override.
    pub fn overrides(&self) -> bool {
        self.mode == MappingMode::Replace
    }
}

/// What a mode is called: "Offset" or "Override".
pub fn mode_name(mode: MappingMode) -> &'static str {
    match mode {
        MappingMode::Replace => "Override",
        _ => "Offset",
    }
}

/// Whether `kind` reads a source that can be mapped: a control, live data, a channel or
/// an audio level.
pub fn reads(kind: &MotionKind) -> bool {
    matches!(
        kind,
        MotionKind::Midi { .. }
            | MotionKind::Data { .. }
            | MotionKind::Channel { .. }
            | MotionKind::Audio { .. }
    )
}

/// How an item property's offset is shown, and how far a drag takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemUnit {
    /// Canvas or source pixels, added.
    Pixels,
    /// Degrees, added.
    Degrees,
    /// A share of the stored value, which is multiplied by one plus it.
    Share,
}

impl ItemUnit {
    /// The unit of `property`, or `None` for one that isn't a single number here.
    pub fn of(property: MotionProperty) -> Option<Self> {
        use MotionProperty as P;
        match property {
            P::X | P::Y | P::Depth | P::CropLeft | P::CropTop | P::CropRight | P::CropBottom => {
                Some(ItemUnit::Pixels)
            }
            P::Rotation | P::Tilt | P::Turn => Some(ItemUnit::Degrees),
            P::Scale | P::Opacity | P::Width | P::Height => Some(ItemUnit::Share),
            _ => None,
        }
    }

    /// The offset a whole drag (an amount of 1) gives, and the farthest a mapping goes
    /// either way.
    pub fn span(self) -> f64 {
        match self {
            ItemUnit::Pixels => 1000.0,
            ItemUnit::Degrees => 360.0,
            ItemUnit::Share => 1.0,
        }
    }

    /// The offset a mapping made without a drag gets (Learn, or a source picked in the
    /// dialog): enough to see, in the direction that makes sense for `property`.
    /// Opacity only goes down from what is stored, so its source fades it in.
    pub fn first_ends(self, property: MotionProperty) -> (f64, f64) {
        match (self, property) {
            (_, MotionProperty::Opacity) => (-1.0, 0.0),
            (ItemUnit::Pixels, _) => (0.0, 200.0),
            (ItemUnit::Degrees, _) => (0.0, 90.0),
            (ItemUnit::Share, _) => (0.0, 0.5),
        }
    }

    /// An offset as the field shows it: `+120 px`, `-15°`, `+20 %`.
    pub fn label(self, offset: f64) -> String {
        let (value, unit, decimals) = match self {
            ItemUnit::Pixels => (offset, " px", 0),
            ItemUnit::Degrees => (offset, "°", 1),
            ItemUnit::Share => (offset * 100.0, " %", 0),
        };
        let scale = 10_f64.powi(decimals);
        let rounded = (value * scale).round() / scale;
        if rounded == 0.0 {
            format!("0{unit}")
        } else if rounded.fract() == 0.0 {
            format!("{rounded:+.0}{unit}")
        } else {
            format!("{rounded:+.1}{unit}")
        }
    }

    /// The ends an override gets at first, in the property's own unit: opacity from
    /// clear to solid, a size from as stored to half as big again, a turn of 90
    /// degrees, a move of 200 pixels from the origin.
    pub fn first_override(self, property: MotionProperty) -> (f64, f64) {
        match (self, property) {
            (_, MotionProperty::Opacity) => (0.0, 1.0),
            (ItemUnit::Pixels, _) => (0.0, 200.0),
            (ItemUnit::Degrees, _) => (0.0, 90.0),
            (ItemUnit::Share, _) => (1.0, 1.5),
        }
    }

    /// The lowest and highest an override's ends may be.
    pub fn override_limits(self, property: MotionProperty) -> (f64, f64) {
        match (self, property) {
            (_, MotionProperty::Opacity) => (0.0, 1.0),
            (ItemUnit::Pixels, _) => (-100_000.0, 100_000.0),
            (ItemUnit::Degrees, _) => (-3600.0, 3600.0),
            (ItemUnit::Share, _) => (0.0, 100.0),
        }
    }
}

/// The offset ends a drag of `amount` leaves: from nothing to the offset `existing`
/// reached plus the drag, no farther than the unit's span either way.
pub fn offset_after(existing: Option<(f32, f32)>, unit: ItemUnit, amount: f64) -> (f32, f32) {
    reach_after(existing, unit.span(), amount)
}

/// The ends a drag of `amount` leaves a mapping whose whole travel is `span`: its start
/// kept (nothing, for a new offset), its far end moved by the drag, no farther than
/// the span from its start either way.
pub fn reach_after(existing: Option<(f32, f32)>, span: f64, amount: f64) -> (f32, f32) {
    let (from, reach) = existing.map_or((0.0, 0.0), |(from, to)| (f64::from(from), f64::from(to)));
    let to = (reach + amount * span).clamp(from - span, from + span);
    (from as f32, to as f32)
}

impl MapTarget {
    /// The ID its control carries in the page (`data-learn` or `data-map`), which a
    /// right-click and a mapping-mode drag name it by. An item's doesn't name the
    /// scene: the fields that carry it show one scene's item at a time.
    pub fn id(&self) -> String {
        match self {
            MapTarget::Value(target) => learn_id(target),
            MapTarget::Item { item, property, .. } => {
                let property = serde_json::to_string(property).unwrap_or_default();
                format!("item:{}:{}", item.as_str(), property.trim_matches('"'))
            }
        }
    }

    /// The mode a mapping made on it without a drag gets (Learn, or a source picked in
    /// the dialog): a setting is taken over, as a knob mapped in a DAW takes one, and a
    /// property is offset, so its keyframes and stored placement still count.
    pub fn first_mode(&self) -> MappingMode {
        match self {
            MapTarget::Value(_) => MappingMode::Replace,
            MapTarget::Item { .. } => MappingMode::Add,
        }
    }

    /// The ends a new mapping of `mode` gets, and the lowest and highest they may be,
    /// given the `range` and `limits` of the target's [`first_mode`](Self::first_mode).
    pub fn shape(
        &self,
        mode: MappingMode,
        range: (f64, f64),
        limits: (f64, f64),
    ) -> ((f64, f64), (f64, f64)) {
        if mode == self.first_mode() {
            return (range, limits);
        }
        match self {
            // An offset on a setting: a quarter of its range up, at most the whole
            // range either way.
            MapTarget::Value(_) => {
                let span = (limits.1 - limits.0).abs();
                ((0.0, span / 4.0), (-span, span))
            }
            MapTarget::Item { property, .. } => match ItemUnit::of(*property) {
                Some(unit) => (
                    unit.first_override(*property),
                    unit.override_limits(*property),
                ),
                None => (range, limits),
            },
        }
    }

    /// The item's motion modifiers, as the scene shows them.
    fn motion(&self, collection: &Collection) -> Vec<MotionModifier> {
        match self {
            MapTarget::Item { scene, item, .. } => collection
                .item_in(Some(scene), item)
                .map(|(_, i)| i.motion.clone())
                .unwrap_or_default(),
            MapTarget::Value(_) => Vec::new(),
        }
    }

    /// The mappings onto it, in the order they are stored.
    pub fn mappings(&self, collection: &Collection) -> Vec<Mapping> {
        match self {
            MapTarget::Value(target) => collection
                .value_bindings
                .iter()
                .filter(|b| &b.target == target)
                .map(|b| Mapping {
                    key: b.binding_id.clone(),
                    input: b.input.clone(),
                    amount: b.amount.clone(),
                    mode: b.mode,
                    enabled: b.enabled,
                })
                .collect(),
            MapTarget::Item { property, .. } => self
                .motion(collection)
                .into_iter()
                .filter(|m| m.property == *property && reads(&m.kind))
                .map(|m| Mapping {
                    key: m.modifier_id,
                    input: m.kind,
                    amount: m.amount,
                    mode: m.mode,
                    enabled: m.enabled,
                })
                .collect(),
        }
    }

    /// Its mapping from the source `input` reads, if it has one.
    pub fn mapping_from(&self, collection: &Collection, input: &MotionKind) -> Option<Mapping> {
        self.mappings(collection)
            .into_iter()
            .find(|m| same_source(&m.input, input))
    }

    /// The edits that map `input` onto it as `mode`. Its mapping from the same source is
    /// rewritten, or the one named `replacing`, keeping whether it is bypassed;
    /// otherwise a mapping is added. A target keeps one override, so a new one takes
    /// the place of the one it had.
    pub fn set(
        &self,
        collection: &Collection,
        input: MotionKind,
        amount: Option<String>,
        mode: MappingMode,
        replacing: Option<&str>,
    ) -> Vec<Edit> {
        let all = self.mappings(collection);
        let existing = all.iter().find(|m| match replacing {
            Some(key) => m.key == key,
            None => same_source(&m.input, &input),
        });
        let enabled = existing.is_none_or(|m| m.enabled);
        let overrides = mode == MappingMode::Replace;
        match self {
            MapTarget::Value(target) => {
                let own = learn_id(target);
                let binding_id = match existing {
                    _ if overrides => own.clone(),
                    Some(m) if m.key != own => m.key.clone(),
                    _ => (1..)
                        .map(|n| format!("map_{}_{n}", own.trim_start_matches("learn_")))
                        .find(|id| {
                            collection
                                .value_bindings
                                .iter()
                                .all(|b| &b.binding_id != id)
                        })
                        .unwrap_or_default(),
                };
                // Whatever the new binding doesn't overwrite: the mapping rewritten,
                // when its name changes with its mode, and any other override.
                let mut edits: Vec<Edit> = all
                    .iter()
                    .filter(|m| m.key != binding_id)
                    .filter(|m| {
                        existing.is_some_and(|e| e.key == m.key) || (overrides && m.overrides())
                    })
                    .map(|m| Edit::BindingClearValue {
                        binding_id: m.key.clone(),
                    })
                    .collect();
                edits.push(Edit::BindingSetValue {
                    binding: ValueBinding {
                        binding_id,
                        input,
                        target: target.clone(),
                        amount,
                        mode,
                        enabled,
                    },
                });
                edits
            }
            MapTarget::Item {
                scene,
                item,
                property,
            } => {
                let mut motion = self.motion(collection);
                let kept = existing.map(|m| m.key.clone());
                if overrides {
                    motion.retain(|m| {
                        !(m.property == *property
                            && reads(&m.kind)
                            && m.mode == MappingMode::Replace
                            && kept.as_deref() != Some(m.modifier_id.as_str()))
                    });
                }
                let at = kept.and_then(|key| motion.iter().position(|m| m.modifier_id == key));
                match at {
                    Some(n) => {
                        motion[n].kind = input;
                        motion[n].amount = amount;
                        motion[n].mode = mode;
                    }
                    None => {
                        let modifier_id = (1..)
                            .map(|n| format!("map_{n}"))
                            .find(|id| motion.iter().all(|m| &m.modifier_id != id))
                            .unwrap_or_default();
                        motion.push(MotionModifier {
                            modifier_id,
                            property: *property,
                            effect: None,
                            source: None,
                            kind: input,
                            clock: MotionClock::default(),
                            enabled: true,
                            amount,
                            mode,
                        });
                    }
                }
                vec![Edit::ItemSetMotion {
                    item_id: item.clone(),
                    scene_id: Some(scene.clone()),
                    motion,
                }]
            }
        }
    }

    /// The edits that bypass its mapping `key`, or bring it back; none when it has no
    /// such mapping or it is that way already.
    pub fn set_enabled(&self, collection: &Collection, key: &str, enabled: bool) -> Vec<Edit> {
        match self {
            MapTarget::Value(target) => collection
                .value_bindings
                .iter()
                .filter(|b| &b.target == target && b.binding_id == key && b.enabled != enabled)
                .map(|b| Edit::BindingSetValue {
                    binding: ValueBinding {
                        enabled,
                        ..b.clone()
                    },
                })
                .collect(),
            MapTarget::Item { scene, item, .. } => {
                let mut motion = self.motion(collection);
                let Some(row) = motion
                    .iter_mut()
                    .find(|m| m.modifier_id == key && m.enabled != enabled)
                else {
                    return Vec::new();
                };
                row.enabled = enabled;
                vec![Edit::ItemSetMotion {
                    item_id: item.clone(),
                    scene_id: Some(scene.clone()),
                    motion,
                }]
            }
        }
    }

    /// The edits that remove its mapping `key`; none when it has no such mapping.
    pub fn clear(&self, collection: &Collection, key: &str) -> Vec<Edit> {
        match self {
            MapTarget::Value(_) => self
                .mappings(collection)
                .iter()
                .filter(|m| m.key == key)
                .map(|m| Edit::BindingClearValue {
                    binding_id: m.key.clone(),
                })
                .collect(),
            MapTarget::Item { scene, item, .. } => {
                let mut motion = self.motion(collection);
                let before = motion.len();
                motion.retain(|m| m.modifier_id != key);
                if motion.len() == before {
                    return Vec::new();
                }
                vec![Edit::ItemSetMotion {
                    item_id: item.clone(),
                    scene_id: Some(scene.clone()),
                    motion,
                }]
            }
        }
    }

    /// How far a mapping moves it, as its control shows it: an item property's offset in
    /// its unit, or a setting's as a share of `range`.
    pub fn amount_label(&self, input: &MotionKind, range: (f64, f64)) -> Option<String> {
        let (from, to) = ends(input)?;
        Some(match self {
            MapTarget::Item { property, .. } => {
                ItemUnit::of(*property)?.label(f64::from(to) - f64::from(from))
            }
            MapTarget::Value(_) => crate::panels::mapping_mode::amount_label((from, to), range),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_sources::mod_reader;
    use crate::panels::map_control::with_ends;

    #[test]
    fn an_offset_reads_in_its_propertys_unit() {
        assert_eq!(ItemUnit::Pixels.label(120.4), "+120 px");
        assert_eq!(ItemUnit::Degrees.label(-15.0), "-15°");
        assert_eq!(ItemUnit::Degrees.label(2.5), "+2.5°");
        assert_eq!(ItemUnit::Share.label(0.2), "+20 %");
        assert_eq!(ItemUnit::Share.label(0.0), "0 %");
        assert_eq!(ItemUnit::of(MotionProperty::Opacity), Some(ItemUnit::Share));
        assert_eq!(ItemUnit::of(MotionProperty::Effect), None);
    }

    #[test]
    fn a_drag_adds_to_the_offset_and_stops_at_the_span() {
        assert_eq!(offset_after(None, ItemUnit::Degrees, 0.25), (0.0, 90.0));
        assert_eq!(offset_after(None, ItemUnit::Pixels, -0.1), (0.0, -100.0));
        assert_eq!(
            offset_after(Some((0.0, 90.0)), ItemUnit::Degrees, -0.5),
            (0.0, -90.0)
        );
        assert_eq!(
            offset_after(Some((0.0, 300.0)), ItemUnit::Degrees, 1.0),
            (0.0, 360.0)
        );
    }

    #[test]
    fn an_item_takes_one_modifier_per_source_and_keeps_its_others() {
        let target = MapTarget::Item {
            scene: SceneId::new("scn_1"),
            item: ItemId::new("itm_1"),
            property: MotionProperty::Rotation,
        };
        assert_eq!(target.id(), "item:itm_1:rotation");
        assert_eq!(target.first_mode(), MappingMode::Add);
        // No such item in an empty collection: the edit still names the one modifier.
        let c = Collection::default();
        let first = with_ends(mod_reader(None, 0), 0.0, 15.0);
        let edits = target.set(&c, first.clone(), None, MappingMode::Add, None);
        let [Edit::ItemSetMotion { motion, .. }] = &edits[..] else {
            panic!("one item.set_motion, got {edits:?}");
        };
        assert_eq!(motion.len(), 1);
        assert_eq!(motion[0].modifier_id, "map_1");
        assert_eq!(motion[0].property, MotionProperty::Rotation);
        assert_eq!(motion[0].kind, first);
        assert!(target.clear(&c, "map_1").is_empty());
    }

    fn gain() -> ValueTarget {
        ValueTarget::SourceGain {
            source_id: jstream_control::protocol::SourceId::new("src_1"),
        }
    }

    /// `collection` after `edits`, for the value bindings only.
    fn after(mut collection: Collection, edits: Vec<Edit>) -> Collection {
        for edit in edits {
            match edit {
                Edit::BindingSetValue { binding } => {
                    collection
                        .value_bindings
                        .retain(|b| b.binding_id != binding.binding_id);
                    collection.value_bindings.push(binding);
                }
                Edit::BindingClearValue { binding_id } => {
                    collection
                        .value_bindings
                        .retain(|b| b.binding_id != binding_id);
                }
                other => panic!("a binding edit, got {other:?}"),
            }
        }
        collection
    }

    #[test]
    fn a_setting_keeps_one_override_and_any_number_of_offsets() {
        let target = MapTarget::Value(gain());
        let own = learn_id(&gain());
        let set = |c: Collection, slot: usize, mode: MappingMode| {
            let edits = target.set(&c, mod_reader(None, slot), None, mode, None);
            after(c, edits)
        };
        let c = set(Collection::default(), 0, MappingMode::Replace);
        let c = set(c, 1, MappingMode::Add);
        let c = set(c, 2, MappingMode::Add);
        let all = target.mappings(&c);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, own);
        assert!(all[0].overrides());
        assert!(all[1].key.starts_with("map_") && all[1].key != all[2].key);
        assert!(!all[1].overrides() && !all[2].overrides());
        // The same source again rewrites its own mapping.
        let c = set(c, 1, MappingMode::Add);
        assert_eq!(target.mappings(&c).len(), 3);
        // A second override takes the place of the first; the offsets stay.
        let c = set(c, 3, MappingMode::Replace);
        let all = target.mappings(&c);
        assert_eq!(all.len(), 3);
        let over: Vec<_> = all.iter().filter(|m| m.overrides()).collect();
        assert_eq!(over.len(), 1);
        assert_eq!(over[0].key, own);
        assert!(same_source(&over[0].input, &mod_reader(None, 3)));
        // An offset made the override moves to the setting's own name, and back.
        let c = set(c, 1, MappingMode::Replace);
        let all = target.mappings(&c);
        assert_eq!(all.len(), 2);
        assert!(same_source(
            &all.iter().find(|m| m.overrides()).unwrap().input,
            &mod_reader(None, 1)
        ));
        let c = set(c, 1, MappingMode::Add);
        let all = target.mappings(&c);
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|m| !m.overrides() && m.key != own));
    }

    #[test]
    fn a_bypassed_mapping_stays_bypassed_when_it_is_rewritten() {
        let target = MapTarget::Value(gain());
        let c = after(
            Collection::default(),
            target.set(
                &Collection::default(),
                mod_reader(None, 0),
                None,
                MappingMode::Add,
                None,
            ),
        );
        let key = target.mappings(&c)[0].key.clone();
        assert!(target.set_enabled(&c, &key, true).is_empty());
        let c2 = after(c.clone(), target.set_enabled(&c, &key, false));
        assert!(!target.mappings(&c2)[0].enabled);
        let again = with_ends(mod_reader(None, 0), 0.0, 3.0);
        let c3 = after(
            c2.clone(),
            target.set(&c2, again, None, MappingMode::Add, None),
        );
        let all = target.mappings(&c3);
        assert_eq!(all.len(), 1);
        assert!(!all[0].enabled);
        assert_eq!(ends(&all[0].input), Some((0.0, 3.0)));
    }

    #[test]
    fn the_other_mode_gets_its_own_ends_and_limits() {
        let value = MapTarget::Value(gain());
        assert_eq!(
            value.shape(MappingMode::Replace, (-60.0, 6.0), (-60.0, 6.0)),
            ((-60.0, 6.0), (-60.0, 6.0))
        );
        assert_eq!(
            value.shape(MappingMode::Add, (-60.0, 6.0), (-60.0, 6.0)),
            ((0.0, 16.5), (-66.0, 66.0))
        );
        let opacity = MapTarget::Item {
            scene: SceneId::new("scn_1"),
            item: ItemId::new("itm_1"),
            property: MotionProperty::Opacity,
        };
        assert_eq!(
            opacity.shape(MappingMode::Replace, (-1.0, 0.0), (-1.0, 1.0)),
            ((0.0, 1.0), (0.0, 1.0))
        );
        assert_eq!(reach_after(None, 66.0, 0.25), (0.0, 16.5));
        assert_eq!(reach_after(Some((0.0, 60.0)), 66.0, 0.5), (0.0, 66.0));
    }

    #[test]
    fn a_setting_has_one_binding_named_after_it() {
        let value = ValueTarget::SourceGain {
            source_id: jstream_control::protocol::SourceId::new("src_1"),
        };
        let target = MapTarget::Value(value.clone());
        assert_eq!(target.id(), learn_id(&value));
        assert_eq!(target.first_mode(), MappingMode::Replace);
        let edits = target.set(
            &Collection::default(),
            mod_reader(None, 3),
            None,
            MappingMode::Replace,
            None,
        );
        let [Edit::BindingSetValue { binding }] = &edits[..] else {
            panic!("one binding.set_value, got {edits:?}");
        };
        assert_eq!(binding.binding_id, learn_id(&value));
        assert_eq!(
            target.amount_label(&with_ends(mod_reader(None, 3), 0.0, 15.0), (0.0, 100.0)),
            Some("+15 %".into())
        );
    }
}
