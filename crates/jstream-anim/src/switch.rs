//! The plan for a switch between two members of a family (ADR-0049 §3).

use crate::keyframe::{Keyframe, Props};

/// What an item does during a switch between two members of a family, by its effective
/// visibility on each side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Motion {
    /// Visible on both sides with equal values: drawn as it is, with no interpolation.
    Still,
    /// Visible on both sides with different values: interpolated from `from` to `to`
    /// as ADR-0029 §5 says.
    Move,
    /// Visible only on the incoming side: appears with its appear effect, ending at `to`.
    Appear,
    /// Visible only on the outgoing side: disappears with its disappear effect, from
    /// `from`.
    Disappear,
    /// Visible on neither side: not drawn.
    Hidden,
}

/// One item's part in a switch: what it does and its effective values at each end.
#[derive(Clone, Debug, PartialEq)]
pub struct Step<K, G, C> {
    /// The item, paired with itself by ID.
    pub item: K,
    /// What it does.
    pub motion: Motion,
    /// Its effective values on the outgoing side.
    pub from: Props<G, C>,
    /// Its effective values on the incoming side.
    pub to: Props<G, C>,
}

/// Plans a switch between two members of one family (ADR-0049 §3). `items` are the
/// parent's items with their current values, in drawing order (the caller includes a
/// group's children when it plans inside the group's texture). `from` and `to` are the
/// outgoing and incoming keyframes, `None` meaning the parent itself.
///
/// Items pair by ID, so every item gets exactly one step, in the order given. The end
/// values are taken now: an edit during the switch is a new plan, which the render
/// thread retargets to from what is on screen (ADR-0029 §7). An item whose glide is
/// `Never` is the caller's to split into a disappear and an appear, as for any switch.
pub fn plan_switch<K, G, C>(
    items: &[(K, Props<G, C>)],
    from: Option<&Keyframe<K, G, C>>,
    to: Option<&Keyframe<K, G, C>>,
) -> Vec<Step<K, G, C>>
where
    K: Ord + Clone,
    G: Clone + PartialEq,
    C: Clone + PartialEq,
{
    let resolve = |kf: Option<&Keyframe<K, G, C>>, item: &K, parent: &Props<G, C>| match kf {
        Some(kf) => kf.effective(item, parent),
        None => parent.clone(),
    };
    items
        .iter()
        .map(|(item, parent)| {
            let from = resolve(from, item, parent);
            let to = resolve(to, item, parent);
            let motion = match (from.visible, to.visible) {
                (true, true) if from == to => Motion::Still,
                (true, true) => Motion::Move,
                (false, true) => Motion::Appear,
                (true, false) => Motion::Disappear,
                (false, false) => Motion::Hidden,
            };
            Step {
                item: item.clone(),
                motion,
                from,
                to,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyframe::Value;

    type G = (i32, i32);
    type C = u32;
    type Kf = Keyframe<&'static str, G, C>;

    fn props(x: i32, visible: bool) -> Props<G, C> {
        Props {
            geometry: (x, 0),
            crop: 0,
            visible,
            opacity: 1.0,
        }
    }

    fn motions(steps: &[Step<&'static str, G, C>]) -> Vec<(&'static str, Motion)> {
        steps.iter().map(|s| (s.item, s.motion)).collect()
    }

    #[test]
    fn parent_to_parent_keeps_everything_still() {
        let items = [("a", props(0, true)), ("b", props(5, false))];
        let steps = plan_switch(&items, None, None);
        assert_eq!(
            motions(&steps),
            [("a", Motion::Still), ("b", Motion::Hidden)]
        );
    }

    #[test]
    fn each_visibility_case_from_the_parent_into_a_keyframe() {
        let items = [
            ("still", props(0, true)),
            ("moves", props(0, true)),
            ("appears", props(0, false)),
            ("disappears", props(0, true)),
            ("hidden", props(0, false)),
        ];
        let mut kf = Kf::new();
        kf.write("moves", Value::Geometry((50, 50))).expect("valid");
        kf.write("appears", Value::Visible(true)).expect("valid");
        kf.write("disappears", Value::Visible(false))
            .expect("valid");
        kf.write("hidden", Value::Opacity(0.5)).expect("valid");
        let steps = plan_switch(&items, None, Some(&kf));
        assert_eq!(
            motions(&steps),
            [
                ("still", Motion::Still),
                ("moves", Motion::Move),
                ("appears", Motion::Appear),
                ("disappears", Motion::Disappear),
                ("hidden", Motion::Hidden),
            ]
        );
        assert_eq!(steps[1].from.geometry, (0, 0));
        assert_eq!(steps[1].to.geometry, (50, 50));
    }

    #[test]
    fn between_two_keyframes_the_ends_are_each_sides_effective_values() {
        let items = [("a", props(0, true)), ("b", props(0, true))];
        let mut one = Kf::new();
        one.write("a", Value::Geometry((10, 0))).expect("valid");
        one.write("b", Value::Opacity(0.5)).expect("valid");
        let mut two = Kf::new();
        two.write("a", Value::Geometry((90, 0))).expect("valid");
        let steps = plan_switch(&items, Some(&one), Some(&two));
        assert_eq!(motions(&steps), [("a", Motion::Move), ("b", Motion::Move)]);
        assert_eq!(
            (steps[0].from.geometry, steps[0].to.geometry),
            ((10, 0), (90, 0))
        );
        assert_eq!(
            (steps[1].from.opacity, steps[1].to.opacity),
            (0.5, 1.0),
            "b goes back to the parent's opacity"
        );
        let back = plan_switch(&items, Some(&two), Some(&one));
        assert_eq!(
            back[0].from, steps[0].to,
            "the reverse switch swaps the ends"
        );
        assert_eq!(back[0].to, steps[0].from);
    }

    #[test]
    fn ends_follow_the_parent_values_given_now() {
        let mut kf = Kf::new();
        kf.write("a", Value::Opacity(0.25)).expect("valid");
        let before = plan_switch(&[("a", props(0, true))], None, Some(&kf));
        let after = plan_switch(&[("a", props(40, true))], None, Some(&kf));
        assert_eq!(before[0].to.geometry, (0, 0));
        assert_eq!(
            after[0].to.geometry,
            (40, 0),
            "an unoverridden field ends at the parent's current value"
        );
        assert_eq!(after[0].to.opacity, 0.25);
    }

    #[test]
    fn items_pair_by_id_in_the_given_order() {
        // Two placements of the same source are two items: each pairs with itself.
        let items = [("top", props(1, true)), ("bottom", props(2, true))];
        let mut kf = Kf::new();
        kf.write("bottom", Value::Geometry((3, 0))).expect("valid");
        let steps = plan_switch(&items, None, Some(&kf));
        assert_eq!(
            motions(&steps),
            [("top", Motion::Still), ("bottom", Motion::Move)]
        );
    }

    #[test]
    fn overrides_on_items_not_listed_plan_nothing() {
        let mut kf = Kf::new();
        kf.write("gone", Value::Visible(false)).expect("valid");
        assert!(plan_switch(&[], None, Some(&kf)).is_empty());
    }
}
