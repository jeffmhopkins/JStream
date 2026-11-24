//! What a control is mapped onto (UI-FEATURES VC-03, VC-04): a setting a value binding
//! drives (ADR-0178), or one of an item's placement properties, which motion modifiers
//! drive (ADR-0099, ADR-0171). The Map control dialog and mapping mode write either
//! through [`MapTarget`], so both kinds map the same way.
//!
//! A value binding takes its setting over, and a setting has one. A motion modifier
//! adds to its property, and a property takes any number of them, one per source: its
//! ends are an offset, which [`ItemUnit`] says how to show and how far a drag goes.
//!
//! Docs: docs/UI-FEATURES.md#automation, docs/adr/0178-value-bindings-drive-any-number.md

use crate::panels::learn::learn_id;
use crate::panels::map_control::{binding_for, ends, same_source};
use crate::store::Collection;
use jstream_control::protocol::{
    Edit, ItemId, MotionClock, MotionKind, MotionModifier, MotionProperty, SceneId, ValueTarget,
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
}

/// Whether `kind` reads a source that can be mapped: a control, live data or a channel.
pub fn reads(kind: &MotionKind) -> bool {
    matches!(
        kind,
        MotionKind::Midi { .. } | MotionKind::Data { .. } | MotionKind::Channel { .. }
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
}

/// The offset ends a drag of `amount` leaves: from nothing to the offset `existing`
/// reached plus the drag, no farther than the unit's span either way.
pub fn offset_after(existing: Option<(f32, f32)>, unit: ItemUnit, amount: f64) -> (f32, f32) {
    let span = unit.span();
    let (from, reach) = existing.map_or((0.0, 0.0), |(from, to)| (f64::from(from), f64::from(to)));
    let to = (reach + amount * span).clamp(from - span, from + span);
    (from as f32, to as f32)
}

impl MapTarget {
    /// The ID its control carries in the page (`data-learn` or `data-map`), which a
    /// right-click and a mapping-mode drag name it by.
    pub fn id(&self) -> String {
        match self {
            MapTarget::Value(target) => learn_id(target),
            MapTarget::Item { item, property, .. } => {
                let property = serde_json::to_string(property).unwrap_or_default();
                format!("item:{}:{}", item.as_str(), property.trim_matches('"'))
            }
        }
    }

    /// Whether its mappings add to what is stored, rather than take it over.
    pub fn adds(&self) -> bool {
        matches!(self, MapTarget::Item { .. })
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

    /// The edits that map `input` onto it. A setting's one binding is replaced. A
    /// property's modifier from the same source is replaced, or the one named
    /// `replacing`; otherwise a modifier is added after the item's others.
    pub fn set(
        &self,
        collection: &Collection,
        input: MotionKind,
        amount: Option<String>,
        replacing: Option<&str>,
    ) -> Vec<Edit> {
        match self {
            MapTarget::Value(target) => vec![Edit::BindingSetValue {
                binding: binding_for(target, input, amount),
            }],
            MapTarget::Item {
                scene,
                item,
                property,
            } => {
                let mut motion = self.motion(collection);
                let at = motion.iter().position(|m| {
                    m.property == *property
                        && reads(&m.kind)
                        && (replacing == Some(m.modifier_id.as_str())
                            || (replacing.is_none() && same_source(&m.kind, &input)))
                });
                match at {
                    Some(n) => {
                        motion[n].kind = input;
                        motion[n].amount = amount;
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
                            mode: jstream_control::protocol::MappingMode::Add,
                            amount,
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
        assert!(target.adds());
        // No such item in an empty collection: the edit still names the one modifier.
        let c = Collection::default();
        let first = with_ends(mod_reader(None, 0), 0.0, 15.0);
        let edits = target.set(&c, first.clone(), None, None);
        let [Edit::ItemSetMotion { motion, .. }] = &edits[..] else {
            panic!("one item.set_motion, got {edits:?}");
        };
        assert_eq!(motion.len(), 1);
        assert_eq!(motion[0].modifier_id, "map_1");
        assert_eq!(motion[0].property, MotionProperty::Rotation);
        assert_eq!(motion[0].kind, first);
        assert!(target.clear(&c, "map_1").is_empty());
    }

    #[test]
    fn a_setting_has_one_binding_named_after_it() {
        let value = ValueTarget::SourceGain {
            source_id: jstream_control::protocol::SourceId::new("src_1"),
        };
        let target = MapTarget::Value(value.clone());
        assert_eq!(target.id(), learn_id(&value));
        assert!(!target.adds());
        let edits = target.set(&Collection::default(), mod_reader(None, 3), None, None);
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
