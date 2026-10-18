//! The Transform panel: aligns, distributes and arranges the selected items
//! (spec-panels.md §4, spec-workspace.md 9.11). Each button works on the items' unrotated
//! boxes, as the prototype does, and sends every changed item's new box as one batch of
//! `item.set_geometry` edits (or `item.set_crop` for Clear crop), so each press is one
//! undo step.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#transform

use crate::app::SessionContext;
use crate::canvas::model::Layout;
use crate::store::Studio;
use jstream_control::protocol::{
    Canvas, Crop, Edit, Finite32, ItemGeometry, ItemId, PlacementForm, SceneId,
};
use leptos::prelude::*;

/// What the alignment buttons align to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlignTo {
    /// The bounds of the selected items, with two or more selected.
    #[default]
    Selection,
    /// The canvas.
    Canvas,
}

/// A Transform panel button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Left edges to the target's left edge.
    AlignLeft,
    /// Horizontal centres to the target's.
    AlignHCenter,
    /// Right edges to the target's right edge.
    AlignRight,
    /// Top edges to the target's top edge.
    AlignTop,
    /// Vertical centres to the target's.
    AlignVCenter,
    /// Bottom edges to the target's bottom edge.
    AlignBottom,
    /// Equal horizontal gaps, keeping the first and last in centre order.
    DistributeH,
    /// Equal vertical gaps.
    DistributeV,
    /// The selection, as a block, to the middle of the canvas.
    Center,
    /// Each item as large as fits on the canvas, keeping its aspect, centred, upright.
    Fit,
    /// Each item over the whole canvas, upright.
    Stretch,
    /// Each item as wide as the primary one.
    MatchWidth,
    /// Each item as tall as the primary one.
    MatchHeight,
    /// A quarter turn anticlockwise.
    RotateLeft,
    /// A quarter turn clockwise.
    RotateRight,
    /// Upright, with no tilt or turn.
    ResetRotation,
    /// The whole source shown again.
    ClearCrop,
}

impl Action {
    /// The alignment buttons, in the panel's order.
    pub const ALIGN: [Action; 6] = [
        Action::AlignLeft,
        Action::AlignHCenter,
        Action::AlignRight,
        Action::AlignTop,
        Action::AlignVCenter,
        Action::AlignBottom,
    ];
    /// The distribution buttons.
    pub const DISTRIBUTE: [Action; 2] = [Action::DistributeH, Action::DistributeV];
    /// The arrange buttons, in the panel's order.
    pub const ARRANGE: [Action; 9] = [
        Action::Center,
        Action::Fit,
        Action::Stretch,
        Action::MatchWidth,
        Action::MatchHeight,
        Action::RotateLeft,
        Action::RotateRight,
        Action::ResetRotation,
        Action::ClearCrop,
    ];

    /// Its label, as the button's tooltip or text.
    pub fn label(self) -> &'static str {
        match self {
            Action::AlignLeft => "Align left",
            Action::AlignHCenter => "Align horizontal centers",
            Action::AlignRight => "Align right",
            Action::AlignTop => "Align top",
            Action::AlignVCenter => "Align vertical centers",
            Action::AlignBottom => "Align bottom",
            Action::DistributeH => "Distribute horizontally",
            Action::DistributeV => "Distribute vertically",
            Action::Center => "Center on canvas",
            Action::Fit => "Fit to canvas",
            Action::Stretch => "Stretch to canvas",
            Action::MatchWidth => "Match width",
            Action::MatchHeight => "Match height",
            Action::RotateLeft => "Rotate 90° left",
            Action::RotateRight => "Rotate 90° right",
            Action::ResetRotation => "Reset rotation",
            Action::ClearCrop => "Clear crop",
        }
    }

    /// The prototype's icon path for an align or distribute button.
    fn icon(self) -> &'static str {
        match self {
            Action::AlignLeft => "M4 3v18 M8 6h11v5H8z M8 13h6v5H8z",
            Action::AlignHCenter => "M12 3v18 M5 6h14v5H5z M8 13h8v5H8z",
            Action::AlignRight => "M20 3v18 M5 6h11v5H5z M10 13h6v5h-6z",
            Action::AlignTop => "M3 4h18 M6 8v11h5V8z M13 8v6h5V8z",
            Action::AlignVCenter => "M3 12h18 M6 5v14h5V5z M13 8v8h5V8z",
            Action::AlignBottom => "M3 20h18 M6 5v11h5V5z M13 10v6h5v-6z",
            Action::DistributeH => "M4 3v18 M20 3v18 M9.5 7h5v10h-5z",
            Action::DistributeV => "M3 4h18 M3 20h18 M7 9.5h10v5H7z",
            _ => "",
        }
    }

    /// Whether the button works with `count` items selected.
    pub fn enabled(self, count: usize) -> bool {
        match self {
            Action::DistributeH | Action::DistributeV => count >= 3,
            Action::MatchWidth | Action::MatchHeight => count >= 2,
            _ => count >= 1,
        }
    }
}

/// An unrotated box: its top-left corner and size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// The unrotated box of `geometry`.
    pub fn of(geometry: &ItemGeometry) -> Rect {
        let (w, h) = (geometry.size.width.get(), geometry.size.height.get());
        Rect {
            x: geometry.center.x.get() - w / 2.0,
            y: geometry.center.y.get() - h / 2.0,
            w,
            h,
        }
    }

    /// The smallest box around all of `rects`, or `None` for none.
    pub fn union(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
        rects.into_iter().reduce(|a, b| {
            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
            let (r, bottom) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
            Rect {
                x,
                y,
                w: r - x,
                h: bottom - y,
            }
        })
    }

    /// The whole canvas.
    pub fn canvas(canvas: Canvas) -> Rect {
        #[expect(
            clippy::cast_precision_loss,
            reason = "canvas sizes are far below 2^24 pixels, so they convert exactly"
        )]
        let (w, h) = (canvas.width as f32, canvas.height as f32);
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }
}

/// `geometry` with its unrotated box at `rect` and its rotation at `rotation`.
fn placed(geometry: &ItemGeometry, rect: Rect, rotation: f32) -> Option<ItemGeometry> {
    crate::canvas::transform::with_box(
        geometry,
        rect.x + rect.w / 2.0,
        rect.y + rect.h / 2.0,
        rect.w,
        rect.h,
        rotation,
    )
}

/// A rotation turned by `by` degrees, back in [−180, 180).
fn turned(rotation: f32, by: f32) -> f32 {
    (rotation + by + 540.0).rem_euclid(360.0) - 180.0
}

/// The new boxes `action` gives the selected `items` (the primary one first), on
/// `canvas`, aligning to `align_to` when two or more are selected and to the canvas
/// otherwise. Only items whose box changes are returned. Clear crop changes no box, so
/// it returns none; see [`crops_to_clear`].
pub fn plan(
    action: Action,
    items: &[(ItemId, ItemGeometry)],
    canvas: Canvas,
    align_to: AlignTo,
) -> Vec<(ItemId, ItemGeometry)> {
    if !action.enabled(items.len()) {
        return Vec::new();
    }
    let canvas = Rect::canvas(canvas);
    let target = if items.len() < 2 || align_to == AlignTo::Canvas {
        canvas
    } else {
        Rect::union(items.iter().map(|(_, g)| Rect::of(g))).unwrap_or(canvas)
    };
    let new: Vec<(ItemId, Option<ItemGeometry>)> = match action {
        Action::DistributeH | Action::DistributeV => distribute(items, action),
        Action::Center => {
            let Some(block) = Rect::union(items.iter().map(|(_, g)| Rect::of(g))) else {
                return Vec::new();
            };
            let (dx, dy) = (
                (canvas.w - block.w) / 2.0 - block.x,
                (canvas.h - block.h) / 2.0 - block.y,
            );
            each(items, |g, r| {
                placed(
                    g,
                    Rect {
                        x: r.x + dx,
                        y: r.y + dy,
                        ..r
                    },
                    g.rotation_deg.get(),
                )
            })
        }
        _ => {
            let (primary_w, primary_h) = items
                .first()
                .map_or((0.0, 0.0), |(_, g)| (Rect::of(g).w, Rect::of(g).h));
            each(items, |g, r| {
                let rotation = g.rotation_deg.get();
                let t = target;
                match action {
                    Action::AlignLeft => placed(g, Rect { x: t.x, ..r }, rotation),
                    Action::AlignHCenter => placed(
                        g,
                        Rect {
                            x: t.x + (t.w - r.w) / 2.0,
                            ..r
                        },
                        rotation,
                    ),
                    Action::AlignRight => placed(
                        g,
                        Rect {
                            x: t.x + t.w - r.w,
                            ..r
                        },
                        rotation,
                    ),
                    Action::AlignTop => placed(g, Rect { y: t.y, ..r }, rotation),
                    Action::AlignVCenter => placed(
                        g,
                        Rect {
                            y: t.y + (t.h - r.h) / 2.0,
                            ..r
                        },
                        rotation,
                    ),
                    Action::AlignBottom => placed(
                        g,
                        Rect {
                            y: t.y + t.h - r.h,
                            ..r
                        },
                        rotation,
                    ),
                    Action::Fit => {
                        let f = (canvas.w / r.w).min(canvas.h / r.h);
                        let (w, h) = (r.w * f, r.h * f);
                        let fit = Rect {
                            x: (canvas.w - w) / 2.0,
                            y: (canvas.h - h) / 2.0,
                            w,
                            h,
                        };
                        placed(g, fit, 0.0)
                    }
                    Action::Stretch => placed(g, canvas, 0.0),
                    Action::MatchWidth => placed(g, Rect { w: primary_w, ..r }, rotation),
                    Action::MatchHeight => placed(g, Rect { h: primary_h, ..r }, rotation),
                    Action::RotateLeft => placed(g, r, turned(rotation, -90.0)),
                    Action::RotateRight => placed(g, r, turned(rotation, 90.0)),
                    Action::ResetRotation => Some(ItemGeometry {
                        rotation_deg: Finite32::ZERO,
                        tilt_deg: Finite32::ZERO,
                        turn_deg: Finite32::ZERO,
                        ..*g
                    }),
                    _ => Some(*g),
                }
            })
        }
    };
    new.into_iter()
        .zip(items)
        .filter_map(|((id, new), (_, old))| new.filter(|n| n != old).map(|n| (id, n)))
        .collect()
}

/// Each item through `f`, with its unrotated box.
fn each(
    items: &[(ItemId, ItemGeometry)],
    f: impl Fn(&ItemGeometry, Rect) -> Option<ItemGeometry>,
) -> Vec<(ItemId, Option<ItemGeometry>)> {
    items
        .iter()
        .map(|(id, g)| (id.clone(), f(g, Rect::of(g))))
        .collect()
}

/// Equal gaps along one axis: the items in centre order, the first and last kept, the
/// others moved so the gaps between neighbours' edges are equal. Returned in `items`'s
/// order.
fn distribute(
    items: &[(ItemId, ItemGeometry)],
    action: Action,
) -> Vec<(ItemId, Option<ItemGeometry>)> {
    let horizontal = action == Action::DistributeH;
    let along = |r: &Rect| if horizontal { (r.x, r.w) } else { (r.y, r.h) };
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        let (pa, sa) = along(&Rect::of(&items[a].1));
        let (pb, sb) = along(&Rect::of(&items[b].1));
        (pa + sa / 2.0).total_cmp(&(pb + sb / 2.0))
    });
    let (Some(&first), Some(&last)) = (order.first(), order.last()) else {
        return Vec::new();
    };
    let (start, _) = along(&Rect::of(&items[first].1));
    let (last_pos, last_size) = along(&Rect::of(&items[last].1));
    let total: f32 = items.iter().map(|(_, g)| along(&Rect::of(g)).1).sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a selection is far smaller than 2^24 items"
    )]
    let gap = (last_pos + last_size - start - total) / (items.len() - 1) as f32;
    let mut out: Vec<(ItemId, Option<ItemGeometry>)> =
        items.iter().map(|(id, g)| (id.clone(), Some(*g))).collect();
    let mut at = start;
    for index in order {
        let (_, g) = &items[index];
        let r = Rect::of(g);
        let moved = if horizontal {
            Rect { x: at, ..r }
        } else {
            Rect { y: at, ..r }
        };
        out[index].1 = placed(g, moved, g.rotation_deg.get());
        at += along(&r).1 + gap;
    }
    out
}

/// The selected items whose crop Clear crop resets.
pub fn crops_to_clear(items: &[(ItemId, Crop)]) -> Vec<ItemId> {
    items
        .iter()
        .filter(|(_, crop)| *crop != Crop::NONE)
        .map(|(id, _)| id.clone())
        .collect()
}

/// The panel's summary line for `count` selected items.
pub fn summary(count: usize) -> String {
    match count {
        0 => "Nothing selected. Click or Shift-click items on the canvas.".into(),
        1 => "1 item selected".into(),
        n => format!("{n} items selected"),
    }
}

/// The Transform panel.
#[component]
pub fn Transform() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let align_to = RwSignal::new(AlignTo::Selection);
    let count = move || studio.selection.with(|s| s.items().count());
    let to_canvas = move || count() < 2 || align_to.get() == AlignTo::Canvas;

    let press = move |action: Action| {
        let Some(s) = session.get() else { return };
        let selection = studio.selection.get_untracked();
        let edits = studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            let scene: SceneId = selection.scene.clone()?;
            let layout = Layout::of(c, Some(&scene));
            if action == Action::ClearCrop {
                let crops: Vec<(ItemId, Crop)> = selection
                    .items()
                    .filter_map(|id| c.item(id).map(|(_, i)| (id.clone(), i.crop)))
                    .collect();
                return Some(
                    crops_to_clear(&crops)
                        .into_iter()
                        .map(|item_id| Edit::ItemSetCrop {
                            item_id,
                            scene_id: Some(scene.clone()),
                            crop: Crop::NONE,
                        })
                        .collect::<Vec<_>>(),
                );
            }
            let geometry_form = |id: &ItemId| {
                c.item(id)
                    .is_some_and(|(_, i)| i.placement == PlacementForm::Geometry)
            };
            // Matching takes the primary item's size, which a legacy item doesn't have.
            if matches!(action, Action::MatchWidth | Action::MatchHeight)
                && !selection.items().next().is_some_and(geometry_form)
            {
                return None;
            }
            // Items in the legacy form have no box to move.
            let items: Vec<(ItemId, ItemGeometry)> = selection
                .items()
                .filter(|id| geometry_form(id))
                .filter_map(|id| layout.geometry_of(id).map(|g| (id.clone(), g)))
                .collect();
            let align = if align_to.get_untracked() == AlignTo::Canvas {
                AlignTo::Canvas
            } else {
                AlignTo::Selection
            };
            Some(
                plan(action, &items, c.canvas, align)
                    .into_iter()
                    .map(|(item_id, geometry)| Edit::ItemSetGeometry {
                        item_id,
                        scene_id: Some(scene.clone()),
                        geometry,
                    })
                    .collect(),
            )
        });
        if let Some(edits) = edits.filter(|e: &Vec<Edit>| !e.is_empty()) {
            studio.apply(&s, edits);
        }
    };

    let icon_button = move |action: Action| {
        view! {
            <button
                class="tool-button"
                title=action.label()
                aria-label=action.label()
                disabled=move || !action.enabled(count())
                on:click=move |_| press(action)
            >
                <svg viewBox="0 0 24 24" width="17" height="17" aria-hidden="true" fill="none"
                    stroke="currentColor" stroke-width="1.7" stroke-linejoin="round">
                    <path d=action.icon() />
                </svg>
            </button>
        }
    };

    view! {
        <h2>"Transform"</h2>
        <div class="transform">
            <p class="summary">{move || summary(count())}</p>
            <div class="align-to">
                <span class="key">"Align to"</span>
                <span class="segmented" role="group" aria-label="Align to">
                    <button
                        aria-pressed=move || (!to_canvas()).to_string()
                        disabled=move || count() < 2
                        on:click=move |_| align_to.set(AlignTo::Selection)
                    >
                        "Selection"
                    </button>
                    <button
                        aria-pressed=move || to_canvas().to_string()
                        on:click=move |_| align_to.set(AlignTo::Canvas)
                    >
                        "Canvas"
                    </button>
                </span>
            </div>
            <h3>"Align"</h3>
            <div class="tool-grid">{Action::ALIGN.map(icon_button).to_vec()}</div>
            <h3>"Distribute · 3 or more"</h3>
            <div class="tool-grid">{Action::DISTRIBUTE.map(icon_button).to_vec()}</div>
            <h3>"Arrange"</h3>
            <div class="arrange-grid">
                {Action::ARRANGE
                    .map(|action| {
                        view! {
                            <button
                                disabled=move || !action.enabled(count())
                                on:click=move |_| press(action)
                            >
                                {action.label()}
                            </button>
                        }
                    })
                    .to_vec()}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{Size, Vec2};

    const CANVAS: Canvas = Canvas {
        width: 1920,
        height: 1080,
    };

    /// An item with its unrotated box at `x, y, w, h`.
    fn item(id: &str, x: f32, y: f32, w: f32, h: f32) -> (ItemId, ItemGeometry) {
        let g = ItemGeometry::try_new(
            Vec2::new(x + w / 2.0, y + h / 2.0).expect("finite"),
            Size::try_new(w, h).expect("positive"),
        )
        .expect("a valid box");
        (ItemId::from(id), g)
    }

    fn rects(placed: &[(ItemId, ItemGeometry)]) -> Vec<(String, Rect)> {
        placed
            .iter()
            .map(|(id, g)| (id.to_string(), Rect::of(g)))
            .collect()
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn one_item_aligns_to_the_canvas() {
        let a = item("a", 100.0, 100.0, 200.0, 100.0);
        let moved = plan(
            Action::AlignRight,
            std::slice::from_ref(&a),
            CANVAS,
            AlignTo::Selection,
        );
        assert_eq!(
            rects(&moved),
            [("a".into(), rect(1720.0, 100.0, 200.0, 100.0))]
        );
        let moved = plan(Action::AlignVCenter, &[a], CANVAS, AlignTo::Selection);
        assert_eq!(
            rects(&moved),
            [("a".into(), rect(100.0, 490.0, 200.0, 100.0))]
        );
    }

    #[test]
    fn several_align_to_their_bounds_or_the_canvas() {
        let items = [
            item("a", 100.0, 100.0, 200.0, 100.0),
            item("b", 400.0, 300.0, 100.0, 100.0),
        ];
        // The bounds run from 100 to 500; a is already at the left.
        let moved = plan(Action::AlignLeft, &items, CANVAS, AlignTo::Selection);
        assert_eq!(
            rects(&moved),
            [("b".into(), rect(100.0, 300.0, 100.0, 100.0))]
        );
        let moved = plan(Action::AlignHCenter, &items, CANVAS, AlignTo::Selection);
        assert_eq!(
            rects(&moved),
            [
                ("a".into(), rect(200.0, 100.0, 200.0, 100.0)),
                ("b".into(), rect(250.0, 300.0, 100.0, 100.0)),
            ]
        );
        let moved = plan(Action::AlignBottom, &items, CANVAS, AlignTo::Canvas);
        assert_eq!(
            rects(&moved),
            [
                ("a".into(), rect(100.0, 980.0, 200.0, 100.0)),
                ("b".into(), rect(400.0, 980.0, 100.0, 100.0)),
            ]
        );
    }

    #[test]
    fn distributing_evens_the_gaps_in_centre_order() {
        // Centre order is a, c, b: a and b stay, c moves to leave 50 px gaps.
        let items = [
            item("a", 0.0, 0.0, 100.0, 50.0),
            item("b", 400.0, 0.0, 100.0, 50.0),
            item("c", 120.0, 0.0, 200.0, 50.0),
        ];
        let moved = plan(Action::DistributeH, &items, CANVAS, AlignTo::Selection);
        assert_eq!(rects(&moved), [("c".into(), rect(150.0, 0.0, 200.0, 50.0))]);
        // Two items can't be distributed.
        assert!(plan(Action::DistributeV, &items[..2], CANVAS, AlignTo::Selection).is_empty());
    }

    #[test]
    fn center_moves_the_selection_as_a_block() {
        let items = [
            item("a", 0.0, 0.0, 100.0, 100.0),
            item("b", 200.0, 100.0, 100.0, 100.0),
        ];
        let moved = plan(Action::Center, &items, CANVAS, AlignTo::Selection);
        assert_eq!(
            rects(&moved),
            [
                ("a".into(), rect(810.0, 440.0, 100.0, 100.0)),
                ("b".into(), rect(1010.0, 540.0, 100.0, 100.0)),
            ]
        );
    }

    #[test]
    fn fit_and_stretch_fill_the_canvas_upright() {
        let mut a = item("a", 0.0, 0.0, 100.0, 100.0);
        a.1.rotation_deg = Finite32::new(30.0).expect("finite");
        let fit = plan(
            Action::Fit,
            std::slice::from_ref(&a),
            CANVAS,
            AlignTo::Selection,
        );
        assert_eq!(
            rects(&fit),
            [("a".into(), rect(420.0, 0.0, 1080.0, 1080.0))]
        );
        assert_eq!(fit[0].1.rotation_deg, Finite32::ZERO);
        let stretched = plan(Action::Stretch, &[a], CANVAS, AlignTo::Selection);
        assert_eq!(
            rects(&stretched),
            [("a".into(), rect(0.0, 0.0, 1920.0, 1080.0))]
        );
    }

    #[test]
    fn match_size_takes_the_primary_items() {
        let items = [
            item("a", 0.0, 0.0, 300.0, 50.0),
            item("b", 500.0, 500.0, 100.0, 100.0),
        ];
        let moved = plan(Action::MatchWidth, &items, CANVAS, AlignTo::Selection);
        assert_eq!(
            rects(&moved),
            [("b".into(), rect(500.0, 500.0, 300.0, 100.0))]
        );
        assert!(plan(Action::MatchHeight, &items[..1], CANVAS, AlignTo::Selection).is_empty());
    }

    #[test]
    fn quarter_turns_wrap_and_reset_clears_every_angle() {
        let mut a = item("a", 0.0, 0.0, 100.0, 100.0);
        a.1.rotation_deg = Finite32::new(170.0).expect("finite");
        a.1.tilt_deg = Finite32::new(10.0).expect("finite");
        let right = plan(
            Action::RotateRight,
            &[a.clone()],
            CANVAS,
            AlignTo::Selection,
        );
        assert_eq!(right[0].1.rotation_deg.get(), -100.0);
        let left = plan(
            Action::RotateLeft,
            std::slice::from_ref(&a),
            CANVAS,
            AlignTo::Selection,
        );
        assert_eq!(left[0].1.rotation_deg.get(), 80.0);
        let reset = plan(Action::ResetRotation, &[a], CANVAS, AlignTo::Selection);
        assert_eq!(reset[0].1.rotation_deg, Finite32::ZERO);
        assert_eq!(reset[0].1.tilt_deg, Finite32::ZERO);
    }

    #[test]
    fn nothing_selected_does_nothing() {
        for action in Action::ALIGN.into_iter().chain(Action::ARRANGE) {
            assert!(!action.enabled(0));
            assert!(plan(action, &[], CANVAS, AlignTo::Selection).is_empty());
        }
    }

    #[test]
    fn only_cropped_items_are_cleared() {
        let crop = Crop {
            left: 10,
            ..Crop::NONE
        };
        let cleared = crops_to_clear(&[("a".into(), Crop::NONE), ("b".into(), crop)]);
        assert_eq!(cleared, [ItemId::from("b")]);
    }

    #[test]
    fn the_summary_counts_the_selection() {
        assert_eq!(
            summary(0),
            "Nothing selected. Click or Shift-click items on the canvas."
        );
        assert_eq!(summary(1), "1 item selected");
        assert_eq!(summary(3), "3 items selected");
    }
}
