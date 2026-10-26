//! The right-click menu: one menu at a time, opened at the pointer by whatever was
//! right-clicked (the canvas, a panel's empty space), with a check column, a colour
//! swatch column, shortcut hints, heads, separators and Windows-style submenus that open on hover. Choosing an
//! item, any press outside, Escape or Tab closes it; the arrow keys, Home and End move
//! through it. Placement and keyboard steps are pure functions, tested natively.
//!
//! Docs: docs/UI-FEATURES.md#menus

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos::web_sys::wasm_bindgen::JsCast;
use std::rc::Rc;

/// What an item runs when chosen.
pub type Run = Rc<dyn Fn()>;

/// One line of a menu.
#[derive(Clone)]
pub enum Entry {
    /// Something to choose, or the opener of a submenu.
    Item(Item),
    /// A thin line between groups.
    Sep,
    /// An upper-case title over the items after it.
    Head(String),
}

/// A menu item.
#[derive(Clone)]
pub struct Item {
    label: String,
    hint: Option<String>,
    checked: Option<bool>,
    swatch: Option<String>,
    disabled: bool,
    run: Option<Run>,
    sub: Vec<Entry>,
}

impl Entry {
    /// An item that runs `run`.
    pub fn item(label: impl Into<String>, run: impl Fn() + 'static) -> Entry {
        Entry::Item(Item {
            label: label.into(),
            hint: None,
            checked: None,
            swatch: None,
            disabled: false,
            run: Some(Rc::new(run)),
            sub: Vec::new(),
        })
    }

    /// An item that can't be chosen: something the UI or the engine can't do yet.
    pub fn unavailable(label: impl Into<String>) -> Entry {
        Entry::Item(Item {
            label: label.into(),
            hint: None,
            checked: None,
            swatch: None,
            disabled: true,
            run: None,
            sub: Vec::new(),
        })
    }

    /// An item that opens `entries` as a submenu.
    pub fn submenu(label: impl Into<String>, entries: Vec<Entry>) -> Entry {
        Entry::Item(Item {
            label: label.into(),
            hint: None,
            checked: None,
            swatch: None,
            disabled: entries.is_empty(),
            run: None,
            sub: entries,
        })
    }

    /// The same item with a shortcut hint (or, on a submenu opener, its current value).
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.hint = Some(hint.into());
        }
        self
    }

    /// The same item with a check column showing `on`.
    #[must_use]
    pub fn checked(mut self, on: bool) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.checked = Some(on);
        }
        self
    }

    /// The same item with a colour in its swatch column, such as a label's.
    #[must_use]
    pub fn swatch(mut self, color: impl Into<String>) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.swatch = Some(color.into());
        }
        self
    }

    /// The same item, disabled when `off`.
    #[must_use]
    pub fn disabled_if(mut self, off: bool) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.disabled |= off;
        }
        self
    }

    /// The height the menu gives this line, for placing it before it is drawn.
    fn height(&self) -> f64 {
        match self {
            Entry::Item(_) => ITEM_HEIGHT,
            Entry::Head(_) => HEAD_HEIGHT,
            Entry::Sep => SEP_HEIGHT,
        }
    }
}

/// The width the main menu is placed by (it may draw narrower).
const MENU_WIDTH: f64 = 300.0;
/// A submenu's width.
const SUB_WIDTH: f64 = 250.0;
/// The estimated height of an item, a head and a separator, and the menu's padding.
const ITEM_HEIGHT: f64 = 30.0;
const HEAD_HEIGHT: f64 = 24.0;
const SEP_HEIGHT: f64 = 9.0;
const PADDING: f64 = 12.0;
/// The gap kept from the window's edges.
const EDGE: f64 = 4.0;

/// The estimated height of a menu of `entries`.
fn estimated_height(entries: &[Entry]) -> f64 {
    entries.iter().map(Entry::height).sum::<f64>() + PADDING
}

/// Where the main menu opens for a press at `(x, y)` in a `width` × `height` window: at
/// the pointer, moved left by its width when it would pass the right edge, and up when it
/// would pass the bottom.
fn place(x: f64, y: f64, width: f64, height: f64, menu_height: f64) -> (f64, f64) {
    let left = if x + MENU_WIDTH > width {
        (x - MENU_WIDTH).max(EDGE)
    } else {
        x
    };
    let top = if y + menu_height > height {
        (height - menu_height - EDGE).max(EDGE)
    } else {
        y
    };
    (left, top)
}

/// Where a submenu opens beside its opener: flush with the menu's right edge, its first
/// item level with the opener, flipped to the left when there is no room, and kept on
/// screen.
fn place_sub(
    menu_left: f64,
    menu_right: f64,
    opener_top: f64,
    width: f64,
    height: f64,
    sub_height: f64,
) -> (f64, f64) {
    let left = if menu_right - 2.0 + SUB_WIDTH > width {
        (menu_left - SUB_WIDTH + 2.0).max(EDGE)
    } else {
        menu_right - 2.0
    };
    let top = (opener_top - 6.0).min(height - sub_height - EDGE).max(EDGE);
    (left, top)
}

/// The next of `count` enabled items from `at`, by `delta`, wrapping at both ends. With
/// none focused, down starts at the first and up at the last.
fn step(count: usize, at: Option<usize>, delta: isize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let Some(at) = at else {
        return Some(if delta < 0 { count - 1 } else { 0 });
    };
    let count = isize::try_from(count).ok()?;
    let at = isize::try_from(at).ok()?;
    usize::try_from((at + delta).rem_euclid(count)).ok()
}

/// A menu open on screen.
#[derive(Clone)]
struct Open {
    x: f64,
    y: f64,
    entries: Rc<Vec<Entry>>,
}

/// The one right-click menu, provided as context by the shell.
#[derive(Clone, Copy)]
pub struct Menus(RwSignal<Option<Open>, LocalStorage>);

impl Menus {
    /// No menu open.
    pub fn new() -> Menus {
        Menus(RwSignal::new_local(None))
    }

    /// Opens `entries` at the pointer of `event`, in place of any open menu, and keeps the
    /// browser's own menu closed.
    pub fn open(&self, event: &ev::MouseEvent, entries: Vec<Entry>) {
        event.prevent_default();
        event.stop_propagation();
        self.0.set(Some(Open {
            x: f64::from(event.client_x()),
            y: f64::from(event.client_y()),
            entries: Rc::new(entries),
        }));
    }

    /// Closes the menu.
    pub fn close(&self) {
        if self.0.with_untracked(Option::is_some) {
            self.0.set(None);
        }
    }

    /// Whether a menu is open.
    pub fn is_open(&self) -> bool {
        self.0.with(Option::is_some)
    }
}

impl Default for Menus {
    fn default() -> Menus {
        Menus::new()
    }
}

/// The window's size in CSS pixels.
fn window_size() -> (f64, f64) {
    let size = |v: Result<leptos::wasm_bindgen::JsValue, _>| {
        v.ok().and_then(|v| v.as_f64()).unwrap_or(0.0)
    };
    (size(window().inner_width()), size(window().inner_height()))
}

/// The enabled items of `menu`, in order.
fn enabled_items(menu: &leptos::web_sys::Element) -> Vec<leptos::web_sys::HtmlElement> {
    let Ok(list) = menu.query_selector_all(":scope > .menuitem:not(:disabled)") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i)?.dyn_into().ok())
        .collect()
}

/// Moves the focus within `menu` by `delta`, or to the first (`Some(0)`) or last item.
fn move_focus(menu: &leptos::web_sys::Element, delta: isize, to: Option<bool>) {
    let items = enabled_items(menu);
    let active = document().active_element();
    let at = items
        .iter()
        .position(|item| active.as_ref() == Some(item.unchecked_ref()));
    let next = match to {
        Some(first) => (!items.is_empty()).then(|| if first { 0 } else { items.len() - 1 }),
        None => step(items.len(), at, delta),
    };
    if let Some(item) = next.and_then(|n| items.get(n)) {
        let _ = item.focus();
    }
}

/// The open submenu: which item opened it, and where it is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SubAt {
    index: usize,
    left: f64,
    top: f64,
}

/// The right-click menu, drawn over everything while one is open.
#[component]
pub fn ContextMenu() -> impl IntoView {
    let menus = expect_context::<Menus>();
    let root = NodeRef::<html::Div>::new();
    let sub_root = NodeRef::<html::Div>::new();
    let sub = RwSignal::new(None::<SubAt>);

    let close = move || {
        sub.set(None);
        menus.close();
    };
    // A new menu starts with no submenu open and its first item focused.
    Effect::new(move |_| {
        if menus.is_open() {
            sub.set(None);
            request_animation_frame(move || {
                if let Some(menu) = root.get_untracked() {
                    move_focus(&menu, 1, Some(true));
                }
            });
        }
    });
    let escape = window_event_listener(ev::keydown, move |event| {
        if event.key() == "Escape" && menus.0.with_untracked(Option::is_some) {
            close();
        }
    });
    on_cleanup(move || escape.remove());

    let open_sub = move |index: usize, opener: &leptos::web_sys::Element, entries: &[Entry]| {
        let Some(menu) = root.get_untracked() else {
            return;
        };
        let menu = menu.get_bounding_client_rect();
        let (width, height) = window_size();
        let (left, top) = place_sub(
            menu.left(),
            menu.right(),
            opener.get_bounding_client_rect().top(),
            width,
            height,
            estimated_height(entries),
        );
        sub.set(Some(SubAt { index, left, top }));
    };

    let item_view = move |index: usize, item: Item, in_sub: bool| {
        let Item {
            label,
            hint,
            checked,
            swatch,
            disabled,
            run,
            sub: children,
        } = item;
        let has_sub = !children.is_empty();
        let children = Rc::new(children);
        let opened = move || !in_sub && sub.get().is_some_and(|s| s.index == index);
        let on_click = {
            let children = Rc::clone(&children);
            move |event: ev::MouseEvent| {
                if has_sub {
                    if opened() {
                        sub.set(None);
                    } else if let Some(target) = event.current_target()
                        && let Ok(opener) = target.dyn_into::<leptos::web_sys::Element>()
                    {
                        open_sub(index, &opener, &children);
                    }
                    return;
                }
                close();
                if let Some(run) = &run {
                    run();
                }
            }
        };
        let on_enter = {
            let children = Rc::clone(&children);
            move |event: ev::PointerEvent| {
                let Some(target) = event.current_target() else {
                    return;
                };
                let Ok(element) = target.dyn_into::<leptos::web_sys::HtmlElement>() else {
                    return;
                };
                if !disabled {
                    let _ = element.focus();
                }
                if in_sub {
                    return;
                }
                if has_sub && !disabled {
                    if !opened() {
                        open_sub(index, &element, &children);
                    }
                } else if sub.get_untracked().is_some() {
                    sub.set(None);
                }
            }
        };
        let on_key = {
            let children = Rc::clone(&children);
            move |event: ev::KeyboardEvent| {
                if event.key() == "ArrowRight" && has_sub && !in_sub {
                    event.prevent_default();
                    event.stop_propagation();
                    if let Some(target) = event.current_target()
                        && let Ok(opener) = target.dyn_into::<leptos::web_sys::Element>()
                    {
                        open_sub(index, &opener, &children);
                    }
                    request_animation_frame(move || {
                        if let Some(menu) = sub_root.get_untracked() {
                            move_focus(&menu, 1, Some(true));
                        }
                    });
                }
            }
        };
        let role = if in_sub && checked.is_some() {
            "menuitemradio"
        } else if checked.is_some() {
            "menuitemcheckbox"
        } else {
            "menuitem"
        };
        view! {
            <button
                type="button"
                class="menuitem ctx-item"
                class:on=opened
                role=role
                aria-checked=checked.map(|on| if on { "true" } else { "false" })
                aria-haspopup=has_sub.then_some("menu")
                aria-expanded=move || has_sub.then(|| if opened() { "true" } else { "false" })
                data-index=index
                disabled=disabled
                on:click=on_click
                on:pointerenter=on_enter
                on:keydown=on_key
            >
                <span class="ctx-check" aria-hidden="true">
                    {(checked == Some(true)).then_some("✓")}
                </span>
                <span class="ctx-swatch" aria-hidden="true" style:background-color=swatch></span>
                <span class="ctx-label">{label}</span>
                {hint.map(|hint| view! { <span class="hint">{hint}</span> })}
                {has_sub.then(|| view! { <span class="ctx-arrow" aria-hidden="true">"▸"</span> })}
            </button>
        }
    };

    let entries_view = move |entries: &[Entry], in_sub: bool| {
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                Entry::Item(item) => item_view(index, item.clone(), in_sub).into_any(),
                Entry::Sep => view! { <div class="menu-sep" role="separator"></div> }.into_any(),
                Entry::Head(text) => {
                    view! { <div class="ctx-head" role="presentation">{text.clone()}</div> }
                        .into_any()
                }
            })
            .collect_view()
    };

    // Up, Down, Home and End move through the items; Left leaves a submenu; Tab closes.
    let on_menu_key = move |in_sub: bool| {
        move |event: ev::KeyboardEvent| {
            let menu = if in_sub {
                sub_root.get_untracked()
            } else {
                root.get_untracked()
            };
            let Some(menu) = menu else { return };
            let (delta, to) = match event.key().as_str() {
                "ArrowDown" => (1, None),
                "ArrowUp" => (-1, None),
                "Home" => (0, Some(true)),
                "End" => (0, Some(false)),
                "ArrowLeft" if in_sub => {
                    event.prevent_default();
                    if let Some(at) = sub.get_untracked() {
                        sub.set(None);
                        if let Some(main) = root.get_untracked() {
                            let selector =
                                format!(":scope > .menuitem[data-index=\"{}\"]", at.index);
                            if let Ok(Some(opener)) = main.query_selector(&selector)
                                && let Ok(opener) =
                                    opener.dyn_into::<leptos::web_sys::HtmlElement>()
                            {
                                let _ = opener.focus();
                            }
                        }
                    }
                    return;
                }
                "Tab" => {
                    event.prevent_default();
                    close();
                    return;
                }
                _ => return,
            };
            event.prevent_default();
            event.stop_propagation();
            move_focus(&menu, delta, to);
        }
    };

    let main = move || {
        let open = menus.0.get()?;
        let (width, height) = window_size();
        let (left, top) = place(
            open.x,
            open.y,
            width,
            height,
            estimated_height(&open.entries),
        );
        let submenu = move || {
            let at = sub.get()?;
            let children = menus
                .0
                .with(|open| match open.as_ref()?.entries.get(at.index) {
                    Some(Entry::Item(item)) => Some(item.sub.clone()),
                    _ => None,
                })?;
            Some(view! {
                <div
                    class="context-menu submenu"
                    role="menu"
                    node_ref=sub_root
                    style:left=format!("{}px", at.left)
                    style:top=format!("{}px", at.top)
                    on:keydown=on_menu_key(true)
                    on:contextmenu=|event: ev::MouseEvent| event.prevent_default()
                >
                    {entries_view(&children, true)}
                </div>
            })
        };
        Some(view! {
            <div
                class="ctx-catcher"
                on:pointerdown=move |_| close()
                on:contextmenu=move |event: ev::MouseEvent| {
                    event.prevent_default();
                    close();
                }
            ></div>
            <div
                class="context-menu"
                role="menu"
                aria-label="Context menu"
                node_ref=root
                style:left=format!("{left}px")
                style:top=format!("{top}px")
                on:keydown=on_menu_key(false)
                on:contextmenu=|event: ev::MouseEvent| event.prevent_default()
            >
                {entries_view(&open.entries, false)}
            </div>
            {submenu}
        })
    };

    view! { {main} }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_opens_at_the_pointer_and_stays_on_screen() {
        assert_eq!(place(100.0, 80.0, 1280.0, 800.0, 200.0), (100.0, 80.0));
        // Too far right: it opens to the pointer's left.
        assert_eq!(place(1100.0, 80.0, 1280.0, 800.0, 200.0), (800.0, 80.0));
        // Too low: it moves up to end 4 px above the bottom.
        assert_eq!(place(100.0, 700.0, 1280.0, 800.0, 200.0), (100.0, 596.0));
        // A window smaller than the menu keeps its corner on screen.
        assert_eq!(place(250.0, 100.0, 280.0, 150.0, 200.0), (4.0, 4.0));
    }

    #[test]
    fn the_height_is_estimated_from_its_lines() {
        let entries = [
            Entry::Head("Scene".into()),
            Entry::unavailable("A"),
            Entry::Sep,
            Entry::unavailable("B"),
        ];
        assert_eq!(estimated_height(&entries), 24.0 + 30.0 + 9.0 + 30.0 + 12.0);
    }

    #[test]
    fn a_submenu_opens_beside_its_opener_or_flips_left() {
        // Room on the right: flush with the menu's edge, its first item level.
        assert_eq!(
            place_sub(100.0, 400.0, 200.0, 1280.0, 800.0, 100.0),
            (398.0, 194.0)
        );
        // No room: it opens on the menu's left.
        assert_eq!(
            place_sub(900.0, 1200.0, 200.0, 1280.0, 800.0, 100.0),
            (652.0, 194.0)
        );
        // Near the bottom: moved up to fit.
        assert_eq!(
            place_sub(100.0, 400.0, 760.0, 1280.0, 800.0, 100.0),
            (398.0, 696.0)
        );
    }

    #[test]
    fn the_arrow_keys_wrap() {
        assert_eq!(step(3, None, 1), Some(0));
        assert_eq!(step(3, None, -1), Some(2));
        assert_eq!(step(3, Some(2), 1), Some(0));
        assert_eq!(step(3, Some(0), -1), Some(2));
        assert_eq!(step(3, Some(1), 1), Some(2));
        assert_eq!(step(0, None, 1), None);
    }

    #[test]
    fn a_submenu_with_nothing_in_it_is_disabled() {
        let Entry::Item(item) = Entry::submenu("Transition in", Vec::new()) else {
            panic!("an item");
        };
        assert!(item.disabled);
        let Entry::Item(item) = Entry::unavailable("Paste").hint("Ctrl+V").checked(true) else {
            panic!("an item");
        };
        assert_eq!(
            (item.hint.as_deref(), item.checked, item.disabled),
            (Some("Ctrl+V"), Some(true), true)
        );
    }
}
