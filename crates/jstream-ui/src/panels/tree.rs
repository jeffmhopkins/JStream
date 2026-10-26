//! What the Scenes and Scene objects panels share (spec-panels.md 1 and 2): the search
//! box and the favourites and labels filter over their rows, the rows' favourite star, and
//! reordering rows by dragging them, before or after another row or into a container
//! row such as a folder. The filters' matching and a drop's new position are plain
//! functions, tested natively.
//!
//! Docs: docs/UI-FEATURES.md#scenes, docs/UI-FEATURES.md#objects

use crate::icons::Icon;
use crate::panels::labels::{LabelIcon, LabelManager};
use crate::store::Studio;
use jstream_control::protocol::LabelId;
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

/// A row's left padding for its depth in the tree: 18 px a level (spec-panels.md S10).
pub fn indent(depth: usize) -> String {
    format!("{}px", 6 + depth * 18)
}

/// The search text, the favourites-only switch and the labels over one panel's rows.
#[derive(Clone, Copy)]
pub struct Filter {
    /// The search box's text.
    pub search: RwSignal<String>,
    /// Whether only favourites show.
    pub favourites: RwSignal<bool>,
    /// The labels checked: a row shows if it carries any of them (S4).
    pub labels: RwSignal<Vec<LabelId>>,
}

impl Filter {
    /// No filter.
    pub fn new() -> Filter {
        Filter {
            search: RwSignal::new(String::new()),
            favourites: RwSignal::new(false),
            labels: RwSignal::new(Vec::new()),
        }
    }

    /// Whether a row of this name, a favourite or not, carrying these labels, shows.
    pub fn shows(&self, name: &str, favourite: bool, carried: &[LabelId]) -> bool {
        (favourite || !self.favourites.get())
            && self.labels.with(|labels| any_of(labels, carried))
            && self.search.with(|search| matches(search, name))
    }

    /// Whether any filter is on, so rows may be hidden and dragging is off (S5).
    pub fn active(&self) -> bool {
        self.count() > 0
    }

    /// The count on the filter button (S3): one for favourites only, one per label and
    /// one for a search.
    fn count(&self) -> usize {
        usize::from(self.favourites.get())
            + self.labels.with(Vec::len)
            + usize::from(self.search.with(|s| !s.trim().is_empty()))
    }

    /// Clears the search, favourites only and the labels.
    fn clear(&self) {
        self.search.set(String::new());
        self.favourites.set(false);
        self.labels.set(Vec::new());
    }
}

/// Whether a row carrying `carried` passes a labels filter of `checked`: any of them,
/// and every row when none is checked.
pub fn any_of(checked: &[LabelId], carried: &[LabelId]) -> bool {
    checked.is_empty() || checked.iter().any(|l| carried.contains(l))
}

impl Default for Filter {
    fn default() -> Filter {
        Filter::new()
    }
}

/// Whether a row's name matches the search: a case-insensitive substring, and an empty
/// search matches everything.
pub fn matches(search: &str, name: &str) -> bool {
    let search = search.trim();
    search.is_empty() || name.to_lowercase().contains(&search.to_lowercase())
}

/// The search box and the filter button over a panel's rows (S2 to S4). With
/// `labels`, the filter lists the collection's labels and offers "Edit labels…"; the
/// Scenes panel has none, since scenes carry no labels (ADR-0024 §4).
#[component]
pub fn SearchBar(
    /// What the rows are, for the placeholder: "scenes" or "objects".
    what: &'static str,
    /// The filter it edits.
    filter: Filter,
    /// Whether the rows carry labels.
    #[prop(optional)]
    labels: bool,
) -> impl IntoView {
    let studio = use_context::<Studio>();
    let manager = use_context::<LabelManager>().filter(|_| labels);
    let open = RwSignal::new(false);
    // A deleted label leaves the filter, so it can't hide every row with nothing to uncheck.
    if let Some(studio) = studio.filter(|_| labels) {
        Effect::new(move |_| {
            let known: Vec<LabelId> = studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| c.labels.iter().map(|l| l.label_id.clone()).collect())
                    .unwrap_or_default()
            });
            if filter
                .labels
                .with_untracked(|l| l.iter().any(|id| !known.contains(id)))
            {
                filter.labels.update(|l| l.retain(|id| known.contains(id)));
            }
        });
    }
    let placeholder = format!("Search {what}");
    let clear = move |_| {
        filter.clear();
        open.set(false);
    };
    let label_rows = move || {
        let all = studio
            .filter(|_| labels)
            .and_then(|s| s.collection.with(|c| c.as_ref().map(|c| c.labels.clone())))
            .unwrap_or_default();
        (!all.is_empty()).then(|| {
            let rows = all
                .into_iter()
                .map(|label| {
                    let id = StoredValue::new(label.label_id.clone());
                    let on = move || filter.labels.with(|l| l.contains(&id.get_value()));
                    let name = label.name.clone();
                    view! {
                        <button
                            type="button"
                            class="tree-check"
                            aria-pressed=move || on().to_string()
                            on:click=move |_| {
                                let mine = id.get_value();
                                filter.labels.update(|l| {
                                    if let Some(at) = l.iter().position(|x| *x == mine) {
                                        l.remove(at);
                                    } else {
                                        l.push(mine);
                                    }
                                });
                            }
                        >
                            <span class="tree-box label-box" class:on=on>{move || on().then_some("✓")}</span>
                            <LabelIcon label=label />
                            {name}
                        </button>
                    }
                })
                .collect_view();
            view! {
                <div class="tree-pop-head">"Labels · any of"</div>
                {rows}
            }
        })
    };
    view! {
        <div class="tree-bar">
            <label class="tree-search">
                <Icon name="search" size=14 />
                <input type="text" aria-label=placeholder.clone() placeholder=placeholder bind:value=filter.search />
            </label>
            <div class="tree-filter">
                <button
                    type="button"
                    class="tree-filter-button"
                    class:on=move || filter.active()
                    aria-label="Filter"
                    title="Filter by favorites and labels"
                    aria-expanded=move || open.get().to_string()
                    on:click=move |_| open.update(|o| *o = !*o)
                >
                    <Icon name="funnel" size=15 />
                    {move || filter.active().then(|| view! { <span class="tree-filter-count">{filter.count()}</span> })}
                </button>
                <Show when=move || open.get()>
                    <div class="menu-catcher" on:pointerdown=move |_| open.set(false)></div>
                    <div
                        class="tree-pop"
                        on:keydown=move |event| {
                            if event.key() == "Escape" {
                                event.stop_propagation();
                                open.set(false);
                            }
                        }
                    >
                        <button
                            type="button"
                            class="tree-check"
                            aria-pressed=move || filter.favourites.get().to_string()
                            on:click=move |_| filter.favourites.update(|f| *f = !*f)
                        >
                            <span class="tree-box" class:on=move || filter.favourites.get()>
                                {move || if filter.favourites.get() { "✓" } else { "" }}
                            </span>
                            <span class="tree-star on"><Icon name="star" size=14 /></span>
                            "Favorites only"
                        </button>
                        {label_rows}
                        <div class="tree-pop-foot">
                            <button type="button" class="link muted" on:click=clear>"Clear"</button>
                            {manager.map(|m| view! {
                                <button
                                    type="button"
                                    class="link"
                                    on:click=move |_| {
                                        open.set(false);
                                        m.open();
                                    }
                                >
                                    "Edit labels…"
                                </button>
                            })}
                        </div>
                    </div>
                </Show>
            </div>
        </div>
    }
}

/// A row's favourite star (S21): gold and always shown when on, otherwise shown on
/// hover.
#[component]
pub fn Star(
    /// The row's name, for the button's label.
    name: String,
    /// Whether it is a favourite.
    #[prop(into)]
    on: Signal<bool>,
    /// Toggles it.
    toggle: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <button
            type="button"
            class="hov tree-star"
            class:on=move || on.get()
            title="Favorite"
            aria-label=format!("Favorite {name}")
            aria-pressed=move || on.get().to_string()
            on:click=move |_| toggle()
        >
            <Icon name="star" size=14 />
        </button>
    }
}

/// Where a dragged row lands, against the row under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Above it.
    Before,
    /// Below it.
    After,
    /// Inside it: only a row that holds others, such as a folder, takes this.
    Into,
}

/// The side of a row a pointer `y` pixels into a row `height` pixels high drops on
/// (S19): a row that holds nothing splits at half its height, before or after; a
/// `container` row takes before over its top quarter, after over its bottom quarter,
/// and into between.
pub fn side(y: f64, height: f64, container: bool) -> Side {
    if container {
        if y < height / 4.0 {
            Side::Before
        } else if y > height * 0.75 {
            Side::After
        } else {
            Side::Into
        }
    } else if y < height / 2.0 {
        Side::Before
    } else {
        Side::After
    }
}

/// Where `moved` goes, counted in `order` as it will be, when it is dropped on the
/// `side` of `target`; `None` when it stays where it is, either isn't in `order`, or
/// it goes into `target` rather than beside it.
/// `order` is the whole list the engine counts in, so a filtered or partial view still
/// gives the engine's index; the reorder edits take that index after removing the moved
/// entry.
pub fn dropped_at<T: PartialEq>(order: &[T], moved: &T, target: &T, side: Side) -> Option<u32> {
    let from = order.iter().position(|e| e == moved)?;
    if moved == target {
        return None;
    }
    let rest: Vec<&T> = order.iter().filter(|e| *e != moved).collect();
    let at = rest.iter().position(|e| *e == target)?;
    let to = match side {
        Side::Before => at,
        Side::After => at + 1,
        Side::Into => return None,
    };
    (to != from).then(|| u32::try_from(to).unwrap_or(u32::MAX))
}

/// A row drag in progress: the row pressed, where, and whether it has moved far enough
/// to be a drag.
#[derive(Clone, Debug)]
struct Press {
    id: String,
    x: i32,
    y: i32,
    dragging: bool,
}

/// Reordering a list's rows by dragging them (S19), with pointer events rather than
/// HTML drag and drop, as the dock's tabs do. Each row carries its ID in `data-tree-id`,
/// and a row that holds others also `data-tree-container`; the list's pointer handlers
/// come from `on_down`, `on_move` and `on_up`.
#[derive(Clone, Copy)]
pub struct Reorder {
    press: StoredValue<Option<Press>>,
    /// The row a drop would land on, and which side, while dragging.
    pub target: RwSignal<Option<(String, Side)>>,
    /// The row being dragged.
    pub dragged: RwSignal<Option<String>>,
}

impl Reorder {
    /// No drag.
    pub fn new() -> Reorder {
        Reorder {
            press: StoredValue::new(None),
            target: RwSignal::new(None),
            dragged: RwSignal::new(None),
        }
    }

    /// A primary press on the row `id` may start a drag.
    pub fn down(&self, id: &str, event: &leptos::ev::PointerEvent) {
        if event.button() != 0 || event.shift_key() {
            return;
        }
        self.press.set_value(Some(Press {
            id: id.to_owned(),
            x: event.client_x(),
            y: event.client_y(),
            dragging: false,
        }));
    }

    /// Follows the pointer: past 4 pixels the press is a drag, and the row under the
    /// pointer becomes the target.
    pub fn moved(&self, event: &leptos::ev::PointerEvent) {
        let Some(mut press) = self.press.get_value() else {
            return;
        };
        if event.buttons() & 1 == 0 {
            self.cancel();
            return;
        }
        if !press.dragging {
            let (dx, dy) = (event.client_x() - press.x, event.client_y() - press.y);
            if dx * dx + dy * dy < 16 {
                return;
            }
            press.dragging = true;
            self.dragged.set(Some(press.id.clone()));
            // Keep the moves coming outside the list, and keep the release from
            // clicking the row.
            if let Some(element) = event
                .current_target()
                .and_then(|t| t.dyn_into::<leptos::web_sys::Element>().ok())
            {
                let _ = element.set_pointer_capture(event.pointer_id());
            }
            self.press.set_value(Some(press));
        }
        self.target.set(row_under(event));
    }

    /// Ends the drag: `drop` gets the dragged row, the target and the side.
    pub fn up(&self, drop: impl FnOnce(String, String, Side)) {
        let press = self.press.get_value();
        let target = self.target.get_untracked();
        self.cancel();
        if let (Some(press), Some((target, side))) = (press, target)
            && press.dragging
        {
            drop(press.id, target, side);
        }
    }

    /// Drops the drag.
    pub fn cancel(&self) {
        self.press.set_value(None);
        self.target.set(None);
        self.dragged.set(None);
    }
}

impl Default for Reorder {
    fn default() -> Reorder {
        Reorder::new()
    }
}

/// The row under the pointer, by its `data-tree-id`, and the side of it the pointer is
/// on, with into for a `data-tree-container` row.
fn row_under(event: &leptos::ev::PointerEvent) -> Option<(String, Side)> {
    let (x, y) = (event.client_x(), event.client_y());
    let row = leptos::prelude::document()
        .element_from_point(x as f32, y as f32)?
        .closest("[data-tree-id]")
        .ok()??;
    let id = row.get_attribute("data-tree-id")?;
    let rect = row.get_bounding_client_rect();
    let container = row.has_attribute("data-tree-container");
    Some((
        id,
        side(f64::from(y) - rect.top(), rect.height(), container),
    ))
}

#[cfg(test)]
mod tests {
    use super::{Side, any_of, dropped_at, matches, side};
    use jstream_control::protocol::LabelId;

    #[test]
    fn a_labels_filter_keeps_rows_with_any_checked_label() {
        let (a, b, c) = (LabelId::new("a"), LabelId::new("b"), LabelId::new("c"));
        assert!(any_of(&[], &[]));
        assert!(any_of(&[], std::slice::from_ref(&a)));
        assert!(any_of(&[a.clone(), b.clone()], &[b.clone(), c.clone()]));
        assert!(!any_of(std::slice::from_ref(&a), &[b, c]));
        assert!(!any_of(&[a], &[]));
    }

    #[test]
    fn search_matches_a_substring_of_the_name_in_any_case() {
        assert!(matches("", "Main"));
        assert!(matches("  ", "Main"));
        assert!(matches("ai", "Main"));
        assert!(matches("MAIN", "main scene"));
        assert!(!matches("intro", "Main"));
    }

    #[test]
    fn a_row_splits_at_half_its_height_and_a_container_in_quarters() {
        assert_eq!(side(0.0, 30.0, false), Side::Before);
        assert_eq!(side(14.9, 30.0, false), Side::Before);
        assert_eq!(side(15.0, 30.0, false), Side::After);
        assert_eq!(side(7.4, 30.0, true), Side::Before);
        assert_eq!(side(7.5, 30.0, true), Side::Into);
        assert_eq!(side(22.5, 30.0, true), Side::Into);
        assert_eq!(side(22.6, 30.0, true), Side::After);
    }

    #[test]
    fn a_drop_gives_the_index_after_removing_the_moved_entry() {
        let order = ["a", "b", "c", "d"];
        // Down: "a" after "c" is index 2 once "a" is out.
        assert_eq!(dropped_at(&order, &"a", &"c", Side::After), Some(2));
        assert_eq!(dropped_at(&order, &"a", &"c", Side::Before), Some(1));
        // Up: "d" before "b".
        assert_eq!(dropped_at(&order, &"d", &"b", Side::Before), Some(1));
        assert_eq!(dropped_at(&order, &"d", &"a", Side::Before), Some(0));
        // Where it already is: nothing to send.
        assert_eq!(dropped_at(&order, &"b", &"a", Side::After), None);
        assert_eq!(dropped_at(&order, &"b", &"c", Side::Before), None);
        assert_eq!(dropped_at(&order, &"b", &"b", Side::After), None);
        // Into a row is no position in the list.
        assert_eq!(dropped_at(&order, &"a", &"c", Side::Into), None);
        // Unknown rows.
        assert_eq!(dropped_at(&order, &"x", &"a", Side::After), None);
        assert_eq!(dropped_at(&order, &"a", &"x", Side::After), None);
    }
}
