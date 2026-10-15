//! A variant's override set, its edits and their inverses (ADR-0049 §1 and §2).

use std::collections::BTreeMap;
use std::fmt;

/// One of the four fields a keyframe can override (ADR-0024 §2). Each is overridden
/// whole: an overridden geometry replaces all of the parent's, angles included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Field {
    /// The item's placement on the canvas.
    Geometry,
    /// Source pixels cut from each edge.
    Crop,
    /// Whether the item is drawn.
    Visible,
    /// Straight-alpha multiplier in `0..=1`.
    Opacity,
}

impl Field {
    /// Every field, in the order the protocol lists them.
    pub const ALL: [Field; 4] = [Field::Geometry, Field::Crop, Field::Visible, Field::Opacity];

    /// The field's name on the wire, as `item.clear_override` will name it.
    pub fn name(self) -> &'static str {
        match self {
            Field::Geometry => "geometry",
            Field::Crop => "crop",
            Field::Visible => "visible",
            Field::Opacity => "opacity",
        }
    }
}

/// An item's keyable values: the parent's, or a variant's effective ones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Props<G, C> {
    /// Its placement.
    pub geometry: G,
    /// Its crop.
    pub crop: C,
    /// Whether it is drawn.
    pub visible: bool,
    /// Its opacity, in `0..=1`.
    pub opacity: f32,
}

/// One field's value, as a placement edit addressed to a variant writes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<G, C> {
    /// A whole geometry.
    Geometry(G),
    /// A whole crop.
    Crop(C),
    /// Visibility.
    Visible(bool),
    /// Opacity, finite and within `0..=1`.
    Opacity(f32),
}

impl<G, C> Value<G, C> {
    /// The field this value is for.
    pub fn field(&self) -> Field {
        match self {
            Value::Geometry(_) => Field::Geometry,
            Value::Crop(_) => Field::Crop,
            Value::Visible(_) => Field::Visible,
            Value::Opacity(_) => Field::Opacity,
        }
    }

    /// Why this value can't be stored, if it can't. Only opacity has a check here:
    /// geometry and crop are checked by the same checks as the item's own placement
    /// edits, which the graph runs before writing.
    pub fn problem(&self) -> Option<InvalidValue> {
        match self {
            Value::Opacity(o) if !o.is_finite() || !(0.0..=1.0).contains(o) => Some(InvalidValue {
                field: Field::Opacity,
                reason: "must be finite and within 0..=1",
            }),
            _ => None,
        }
    }
}

/// A value [`Keyframe::write`] refuses. It names the field and a fixed reason, never the
/// value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidValue {
    /// The field the value was for.
    pub field: Field,
    /// Why it was refused.
    pub reason: &'static str,
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.field.name(), self.reason)
    }
}

impl std::error::Error for InvalidValue {}

/// The overrides a variant stores for one item. An absent field follows the parent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Override<G, C> {
    /// The overridden geometry, if any.
    pub geometry: Option<G>,
    /// The overridden crop, if any.
    pub crop: Option<C>,
    /// The overridden visibility, if any.
    pub visible: Option<bool>,
    /// The overridden opacity, if any.
    pub opacity: Option<f32>,
}

impl<G, C> Default for Override<G, C> {
    fn default() -> Self {
        Self {
            geometry: None,
            crop: None,
            visible: None,
            opacity: None,
        }
    }
}

impl<G: Clone, C: Clone> Override<G, C> {
    /// Whether no field is overridden.
    pub fn is_empty(&self) -> bool {
        self.geometry.is_none()
            && self.crop.is_none()
            && self.visible.is_none()
            && self.opacity.is_none()
    }

    /// The overridden value of `field`, if it is overridden.
    pub fn get(&self, field: Field) -> Option<Value<G, C>> {
        match field {
            Field::Geometry => self.geometry.clone().map(Value::Geometry),
            Field::Crop => self.crop.clone().map(Value::Crop),
            Field::Visible => self.visible.map(Value::Visible),
            Field::Opacity => self.opacity.map(Value::Opacity),
        }
    }

    /// The overridden values, in [`Field::ALL`] order.
    pub fn values(&self) -> impl Iterator<Item = Value<G, C>> + '_ {
        Field::ALL.into_iter().filter_map(|f| self.get(f))
    }

    /// The item's effective values over `parent`: each overridden field takes the
    /// override, and every other field takes the parent's value as given, so it follows
    /// the parent live (ADR-0049 §1).
    pub fn apply(&self, parent: &Props<G, C>) -> Props<G, C> {
        Props {
            geometry: self
                .geometry
                .clone()
                .unwrap_or_else(|| parent.geometry.clone()),
            crop: self.crop.clone().unwrap_or_else(|| parent.crop.clone()),
            visible: self.visible.unwrap_or(parent.visible),
            opacity: self.opacity.unwrap_or(parent.opacity),
        }
    }

    /// Sets `value`'s field and returns the value it replaced.
    fn set(&mut self, value: Value<G, C>) -> Option<Value<G, C>> {
        match value {
            Value::Geometry(g) => self.geometry.replace(g).map(Value::Geometry),
            Value::Crop(c) => self.crop.replace(c).map(Value::Crop),
            Value::Visible(v) => self.visible.replace(v).map(Value::Visible),
            Value::Opacity(o) => self.opacity.replace(o).map(Value::Opacity),
        }
    }

    /// Removes `field` and returns the value it held.
    fn take(&mut self, field: Field) -> Option<Value<G, C>> {
        match field {
            Field::Geometry => self.geometry.take().map(Value::Geometry),
            Field::Crop => self.crop.take().map(Value::Crop),
            Field::Visible => self.visible.take().map(Value::Visible),
            Field::Opacity => self.opacity.take().map(Value::Opacity),
        }
    }
}

/// The edit that undoes a write or a clear (ADR-0049 §2), for the same item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Inverse<G, C> {
    /// Write this value back: the field held it before.
    Write(Value<G, C>),
    /// Clear this field: it was absent before (`item.clear_override`).
    Clear(Field),
}

/// [`Keyframe::restore`] found an item that already has overrides. Nothing was restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreConflict<K>(pub K);

/// A variant's overrides, keyed by item ID in ID order. Never stores an empty override.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe<K, G, C> {
    overrides: BTreeMap<K, Override<G, C>>,
}

impl<K: Ord, G, C> Default for Keyframe<K, G, C> {
    fn default() -> Self {
        Self {
            overrides: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Clone, G: Clone, C: Clone> Keyframe<K, G, C> {
    /// A keyframe that overrides nothing, so it looks exactly like its parent.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing is overridden.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    /// How many items have overrides.
    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    /// The overrides for `item`, if it has any.
    pub fn get(&self, item: &K) -> Option<&Override<G, C>> {
        self.overrides.get(item)
    }

    /// Every item with overrides, in ID order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &Override<G, C>)> {
        self.overrides.iter()
    }

    /// `item`'s effective values in this keyframe over the parent's current values
    /// (ADR-0049 §1). An item with no overrides looks exactly as in the parent.
    pub fn effective(&self, item: &K, parent: &Props<G, C>) -> Props<G, C> {
        match self.overrides.get(item) {
            Some(o) => o.apply(parent),
            None => parent.clone(),
        }
    }

    /// Writes one field of `item`'s override (a placement edit addressed to this
    /// variant). Returns the edit that undoes it: clearing the field if it was absent,
    /// or writing the old value back. An invalid value changes nothing.
    pub fn write(&mut self, item: K, value: Value<G, C>) -> Result<Inverse<G, C>, InvalidValue> {
        if let Some(problem) = value.problem() {
            return Err(problem);
        }
        let field = value.field();
        let old = self.overrides.entry(item).or_default().set(value);
        Ok(match old {
            Some(old) => Inverse::Write(old),
            None => Inverse::Clear(field),
        })
    }

    /// Clears one field of `item`'s override (`item.clear_override`). Returns the
    /// write that undoes it, or `None` when the field was absent: that clear changes
    /// nothing and has no inverse (ADR-0049 §2). An item left with no field loses its
    /// entry.
    pub fn clear(&mut self, item: &K, field: Field) -> Option<Inverse<G, C>> {
        let entry = self.overrides.get_mut(item)?;
        let old = entry.take(field)?;
        if entry.is_empty() {
            self.overrides.remove(item);
        }
        Some(Inverse::Write(old))
    }

    /// "Reset keyframe": clears every field of every item, as a batch of clears would.
    /// Returns the writes that undo it, item by item in ID order, each item's fields in
    /// [`Field::ALL`] order. They restore the keyframe in any order, since no two name
    /// the same field of the same item.
    pub fn reset(&mut self) -> Vec<(K, Inverse<G, C>)> {
        let overrides = std::mem::take(&mut self.overrides);
        overrides
            .into_iter()
            .flat_map(|(item, o)| {
                o.values()
                    .map(|v| (item.clone(), Inverse::Write(v)))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Drops the overrides of every item `keep` refuses, as an edit that takes items out
    /// of the variant's tree does (ADR-0040 §1, ADR-0024 §2 on `item.remove`). Returns
    /// each dropped item's whole override, in ID order, for the edit's result and for
    /// [`Keyframe::restore`] in its inverse.
    pub fn drop_unless(&mut self, mut keep: impl FnMut(&K) -> bool) -> Vec<(K, Override<G, C>)> {
        let mut dropped = Vec::new();
        self.overrides.retain(|item, o| {
            let stays = keep(item);
            if !stays {
                dropped.push((item.clone(), o.clone()));
            }
            stays
        });
        dropped
    }

    /// Restores overrides that [`Keyframe::drop_unless`] dropped. Refused, changing
    /// nothing, if one of the items already has overrides (the inverse would overwrite a
    /// later edit). Empty overrides are skipped, so the keyframe still never stores one.
    pub fn restore(&mut self, dropped: Vec<(K, Override<G, C>)>) -> Result<(), RestoreConflict<K>> {
        if let Some((item, _)) = dropped
            .iter()
            .find(|(item, _)| self.overrides.contains_key(item))
        {
            return Err(RestoreConflict(item.clone()));
        }
        for (item, o) in dropped {
            if !o.is_empty() {
                self.overrides.insert(item, o);
            }
        }
        Ok(())
    }

    /// Applies an [`Inverse`] to `item`: a write or a clear. A convenience for undo; an
    /// inverse that came from this keyframe always applies.
    pub fn undo(
        &mut self,
        item: K,
        inverse: Inverse<G, C>,
    ) -> Result<Option<Inverse<G, C>>, InvalidValue> {
        match inverse {
            Inverse::Write(v) => self.write(item, v).map(Some),
            Inverse::Clear(f) => Ok(self.clear(&item, f)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type G = (i32, i32);
    type C = u32;
    type Kf = Keyframe<&'static str, G, C>;

    fn parent() -> Props<G, C> {
        Props {
            geometry: (10, 20),
            crop: 0,
            visible: true,
            opacity: 1.0,
        }
    }

    #[test]
    fn an_empty_keyframe_looks_like_its_parent() {
        let kf = Kf::new();
        assert!(kf.is_empty());
        assert_eq!(kf.effective(&"a", &parent()), parent());
    }

    #[test]
    fn absent_fields_follow_the_parent_live() {
        let mut kf = Kf::new();
        kf.write("a", Value::Geometry((100, 200))).expect("valid");
        let mut p = parent();
        p.geometry = (1, 1);
        p.opacity = 0.5;
        p.crop = 7;
        let e = kf.effective(&"a", &p);
        assert_eq!(
            e.geometry,
            (100, 200),
            "the override wins over a later parent edit"
        );
        assert_eq!(
            e.opacity, 0.5,
            "an absent field takes the parent's value now"
        );
        assert_eq!(e.crop, 7);
        assert!(e.visible);
        assert_eq!(
            kf.effective(&"b", &p),
            p,
            "an item with no overrides is the parent's"
        );
    }

    #[test]
    fn writing_an_absent_field_is_undone_by_clearing_it() {
        let mut kf = Kf::new();
        let inv = kf.write("a", Value::Visible(false)).expect("valid");
        assert_eq!(inv, Inverse::Clear(Field::Visible));
        assert_eq!(
            kf.undo("a", inv),
            Ok(Some(Inverse::Write(Value::Visible(false))))
        );
        assert!(kf.is_empty(), "the emptied entry is removed");
    }

    #[test]
    fn writing_a_present_field_is_undone_by_the_old_value() {
        let mut kf = Kf::new();
        kf.write("a", Value::Opacity(0.25)).expect("valid");
        let inv = kf.write("a", Value::Opacity(0.75)).expect("valid");
        assert_eq!(inv, Inverse::Write(Value::Opacity(0.25)));
        kf.undo("a", inv).expect("applies");
        assert_eq!(kf.get(&"a").and_then(|o| o.opacity), Some(0.25));
    }

    #[test]
    fn clearing_an_absent_field_changes_nothing_and_has_no_inverse() {
        let mut kf = Kf::new();
        assert_eq!(kf.clear(&"a", Field::Crop), None);
        kf.write("a", Value::Crop(3)).expect("valid");
        let before = kf.clone();
        assert_eq!(kf.clear(&"a", Field::Geometry), None);
        assert_eq!(kf, before);
    }

    #[test]
    fn invalid_opacity_is_refused_and_changes_nothing() {
        let mut kf = Kf::new();
        for bad in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let err = kf.write("a", Value::Opacity(bad)).expect_err("refused");
            assert_eq!(err.field, Field::Opacity);
            assert_eq!(err.to_string(), "opacity must be finite and within 0..=1");
            assert!(kf.is_empty());
        }
        for good in [0.0, 1.0] {
            kf.write("a", Value::Opacity(good)).expect("in range");
        }
    }

    #[test]
    fn reset_clears_everything_and_its_writes_restore_it() {
        let mut kf = Kf::new();
        kf.write("b", Value::Opacity(0.5)).expect("valid");
        kf.write("a", Value::Visible(false)).expect("valid");
        kf.write("a", Value::Geometry((1, 2))).expect("valid");
        let before = kf.clone();
        let undo = kf.reset();
        assert!(kf.is_empty());
        assert_eq!(
            undo,
            vec![
                ("a", Inverse::Write(Value::Geometry((1, 2)))),
                ("a", Inverse::Write(Value::Visible(false))),
                ("b", Inverse::Write(Value::Opacity(0.5))),
            ]
        );
        for (item, inv) in undo.into_iter().rev() {
            kf.undo(item, inv).expect("applies");
        }
        assert_eq!(kf, before);
    }

    #[test]
    fn dropped_overrides_restore_whole() {
        let mut kf = Kf::new();
        kf.write("a", Value::Opacity(0.5)).expect("valid");
        kf.write("a", Value::Crop(4)).expect("valid");
        kf.write("b", Value::Visible(false)).expect("valid");
        kf.write("c", Value::Geometry((0, 0))).expect("valid");
        let before = kf.clone();
        let dropped = kf.drop_unless(|item| *item == "b");
        assert_eq!(
            dropped.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(dropped[0].1.crop, Some(4));
        assert_eq!(kf.len(), 1);
        kf.restore(dropped).expect("nothing in the way");
        assert_eq!(kf, before);
    }

    #[test]
    fn a_restore_onto_a_later_override_is_refused_whole() {
        let mut kf = Kf::new();
        kf.write("a", Value::Opacity(0.5)).expect("valid");
        kf.write("b", Value::Opacity(0.5)).expect("valid");
        let dropped = kf.drop_unless(|_| false);
        kf.write("b", Value::Visible(true)).expect("valid");
        let before = kf.clone();
        assert_eq!(kf.restore(dropped), Err(RestoreConflict("b")));
        assert_eq!(kf, before, "a refused restore changes nothing");
    }

    #[test]
    fn field_names_match_the_wire() {
        let names: Vec<_> = Field::ALL.iter().map(|f| f.name()).collect();
        assert_eq!(names, ["geometry", "crop", "visible", "opacity"]);
    }

    /// A seeded walk of random writes, clears, resets, drops and restores: after every
    /// step, applying the returned inverses in reverse must restore the keyframe
    /// exactly, and no empty override may be stored.
    #[test]
    fn random_edits_undo_exactly() {
        let mut seed: u64 = 0x5eed_0049;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % n
        };
        let items = ["a", "b", "c", "d"];
        let mut kf = Kf::new();
        for _ in 0..5000 {
            let before = kf.clone();
            let item = items[next(4) as usize];
            match next(7) {
                0..=2 => {
                    let value = match next(4) {
                        0 => Value::Geometry((next(5) as i32, next(5) as i32)),
                        1 => Value::Crop(next(5) as u32),
                        2 => Value::Visible(next(2) == 0),
                        _ => Value::Opacity(next(5) as f32 / 4.0),
                    };
                    let inv = kf.write(item, value).expect("valid");
                    let mut undone = kf.clone();
                    undone.undo(item, inv).expect("applies");
                    assert_eq!(undone, before);
                }
                3 | 4 => {
                    let field = Field::ALL[next(4) as usize];
                    let mut undone = kf.clone();
                    match kf.clear(&item, field) {
                        Some(inv) => {
                            undone = kf.clone();
                            undone.undo(item, inv).expect("applies");
                        }
                        None => assert_eq!(kf, before),
                    }
                    assert_eq!(undone, before);
                }
                5 => {
                    let undo = kf.reset();
                    let mut undone = kf.clone();
                    for (k, inv) in undo.into_iter().rev() {
                        undone.undo(k, inv).expect("applies");
                    }
                    assert_eq!(undone, before);
                }
                _ => {
                    let keep = next(16);
                    let dropped = kf.drop_unless(|k| {
                        keep & (1 << items.iter().position(|i| i == k).unwrap_or(0)) != 0
                    });
                    let mut undone = kf.clone();
                    undone.restore(dropped).expect("nothing in the way");
                    assert_eq!(undone, before);
                }
            }
            assert!(
                kf.iter().all(|(_, o)| !o.is_empty()),
                "no empty override is stored"
            );
        }
    }
}
