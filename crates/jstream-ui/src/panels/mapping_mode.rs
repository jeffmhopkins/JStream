//! Mapping mode (UI-FEATURES VC-04): one source is armed, and a drag up or down on any
//! mappable setting maps the source onto it by an amount, as a modulation source is
//! mapped in a synth. The source moves the setting from where it is now by up to the
//! amount dragged; Shift drags finely; a click without a drag maps nothing. Any number
//! of settings can be mapped while the source stays armed, until Escape or Done.
//!
//! [`MappingMode`] is the app-wide state: whatever lists sources arms one by setting
//! `armed`, and [`MappingBar`] shows it and turns pointer drags on a setting's row
//! into `drag`, which that setting's control button (`learn.rs`) previews and, once the
//! drag ends, writes as its value binding.
//!
//! Docs: docs/UI-FEATURES.md#automation, docs/adr/0178-value-bindings-drive-any-number.md

use crate::live_sources::{MOD_COUNT, mod_of, mod_reader, reader_name};
use crate::mapping_index::{self, SourceKey};
use crate::store::{Collection, Studio};
use jstream_control::protocol::MotionKind;
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

/// How far the pointer travels, in CSS pixels, for an amount of the setting's whole
/// range.
pub const FULL_TRAVEL: f64 = 200.0;
/// How much slower a drag with Shift held goes.
pub const FINE: f64 = 0.1;
/// The travel, in CSS pixels, under which a press is a click, which maps nothing.
pub const CLICK_SLOP: f64 = 3.0;

/// The source being mapped.
#[derive(Clone, Debug, PartialEq)]
pub struct ArmedSource {
    /// How a mapping reads it: a reader over 0 to 1, whose ends each mapping sets.
    pub reader: MotionKind,
    /// Its name, as the bar shows it.
    pub name: String,
}

impl ArmedSource {
    /// `reader` armed, named as the pickers name it.
    pub fn of(reader: MotionKind) -> Self {
        let name = reader_name(None, &reader);
        ArmedSource { reader, name }
    }
}

/// A drag on a setting while a source is armed.
#[derive(Clone, Debug, PartialEq)]
pub struct Drag {
    /// The setting, by its control button's `data-learn`.
    pub setting: String,
    /// The amount so far, as a share of the setting's range: positive is up.
    pub amount: f64,
    /// Whether the pointer has been let go, so the amount is to be written.
    pub done: bool,
}

/// Mapping mode's state, shared by everything that arms a source or takes a drag.
#[derive(Clone, Copy)]
pub struct MappingMode {
    /// The armed source, or `None` outside mapping mode.
    pub armed: RwSignal<Option<ArmedSource>>,
    /// The drag under way, or just ended.
    pub drag: RwSignal<Option<Drag>>,
}

impl MappingMode {
    /// Nothing armed.
    pub fn new() -> Self {
        MappingMode {
            armed: RwSignal::new(None),
            drag: RwSignal::new(None),
        }
    }

    /// Arms `reader`, replacing whatever was armed.
    pub fn arm(self, reader: MotionKind) {
        self.drag.set(None);
        self.armed.set(Some(ArmedSource::of(reader)));
    }

    /// Leaves mapping mode.
    pub fn disarm(self) {
        self.drag.set(None);
        self.armed.set(None);
    }
}

impl Default for MappingMode {
    fn default() -> Self {
        Self::new()
    }
}

/// The amount a drag has reached after the pointer moved `dy` pixels down (so a
/// negative `dy` adds), from `amount`, kept within one whole range either way.
pub fn dragged(amount: f64, dy: f64, fine: bool) -> f64 {
    let step = -dy / FULL_TRAVEL * if fine { FINE } else { 1.0 };
    (amount + step).clamp(-1.0, 1.0)
}

/// The ends a mapping gets from a drag of `amount`: it starts where the setting is
/// (`current`, or the low end when that isn't known) and reaches `amount` of the range
/// from there, kept inside the range. A setting the armed source maps already
/// (`existing`, its ends) keeps its start, and the drag moves its far end.
pub fn ends_after(
    existing: Option<(f32, f32)>,
    current: Option<f64>,
    range: (f64, f64),
    amount: f64,
) -> (f32, f32) {
    let (low, high) = (range.0.min(range.1), range.0.max(range.1));
    let span = range.1 - range.0;
    let (from, reach) = match existing {
        Some((from, to)) => (f64::from(from), f64::from(to)),
        None => {
            let from = current.unwrap_or(range.0);
            (from, from)
        }
    };
    let from = from.clamp(low, high);
    let to = (reach + amount * span).clamp(low, high);
    (from as f32, to as f32)
}

/// How far a mapping moves its setting, as the setting's row shows it: a signed share
/// of the range, `+15 %`.
pub fn amount_label(ends: (f32, f32), range: (f64, f64)) -> String {
    let span = range.1 - range.0;
    if span.abs() < f64::EPSILON {
        return "0 %".into();
    }
    let share = (f64::from(ends.1) - f64::from(ends.0)) / span * 100.0;
    let rounded = if share.abs() < 10.0 {
        (share * 10.0).round() / 10.0
    } else {
        share.round()
    };
    if rounded == 0.0 {
        "0 %".into()
    } else if rounded.fract() == 0.0 {
        format!("{rounded:+.0} %")
    } else {
        format!("{rounded:+.1} %")
    }
}

/// The Mod `by` slots along from the armed one, on the same sender, stopping at the
/// first and the last; `None` when the armed source isn't a Mod.
pub fn stepped(armed: &MotionKind, by: i32) -> Option<MotionKind> {
    stepped_in(None, armed, by)
}

/// [`stepped`] with the collection's named Mods: a Mod the Mappings panel has named is a
/// channel with that Mod as its only input, so stepping onto one arms its channel, and
/// stepping from one goes on from its Mod.
pub fn stepped_in(
    collection: Option<&Collection>,
    armed: &MotionKind,
    by: i32,
) -> Option<MotionKind> {
    let (sender, slot) = match mod_of(armed) {
        Some((sender, slot)) => (sender.map(str::to_owned), slot),
        None => {
            let MotionKind::Channel { channel_id, .. } = armed else {
                return None;
            };
            let key = mapping_index::named_mod(collection?.channel(channel_id)?)?;
            let slot = usize::from(mapping_index::mod_slot(&key)?.checked_sub(1)?);
            let SourceKey::Data { input, .. } = key else {
                return None;
            };
            (input, slot)
        }
    };
    let next = (slot as i32 + by).clamp(0, MOD_COUNT as i32 - 1) as usize;
    let named = collection.zip(sender.as_deref()).and_then(|(c, s)| {
        let n = u8::try_from(next + 1).ok()?;
        mapping_index::mod_channel(c, s, n)
    });
    Some(match named {
        Some(channel) => mapping_index::channel_reader(&channel.channel_id),
        None => mod_reader(sender.as_deref(), next),
    })
}

/// The setting a pointer event landed on: the field around it that carries `data-map`,
/// else, by its control button's `data-learn`, the nearest element around it that holds
/// exactly one control button as its own child, looked for no further out than the
/// panel or dialog.
pub fn setting_at(mut el: web_sys::Element) -> Option<String> {
    // A field marked as a target names itself.
    if let Ok(Some(marked)) = el.closest("[data-map]") {
        return marked.get_attribute("data-map");
    }
    loop {
        if let Ok(buttons) = el.query_selector_all(":scope > button.learn") {
            match buttons.length() {
                0 => {}
                1 => {
                    return buttons
                        .item(0)
                        .and_then(|n| n.dyn_into::<web_sys::Element>().ok())
                        .and_then(|b| b.get_attribute("data-learn"));
                }
                _ => return None,
            }
        }
        if el
            .class_name()
            .split_whitespace()
            .any(|c| matches!(c, "panel" | "modal" | "shell"))
        {
            return None;
        }
        el = el.parent_element()?;
    }
}

/// A listener on the document's capture phase, so it hears a press before the control
/// under the pointer does.
struct Captured {
    event: &'static str,
    callback: leptos::wasm_bindgen::JsValue,
}

impl Captured {
    fn add<E: JsCast + 'static>(event: &'static str, handler: impl Fn(E) + 'static) -> Self {
        use leptos::wasm_bindgen::closure::Closure;
        let callback = Closure::<dyn Fn(web_sys::Event)>::new(move |e: web_sys::Event| {
            if let Ok(e) = e.dyn_into::<E>() {
                handler(e);
            }
        })
        .into_js_value();
        _ = document().add_event_listener_with_callback_and_bool(
            event,
            callback.unchecked_ref(),
            true,
        );
        Captured { event, callback }
    }

    fn remove(&self) {
        _ = document().remove_event_listener_with_callback_and_bool(
            self.event,
            self.callback.unchecked_ref(),
            true,
        );
    }
}

/// The element an event landed on.
fn target_of(event: &web_sys::Event) -> Option<web_sys::Element> {
    event.target()?.dyn_into::<web_sys::Element>().ok()
}

/// Whether an event landed inside the mapping bar, whose own buttons work as usual.
fn in_bar(event: &web_sys::Event) -> bool {
    target_of(event).is_some_and(|el| matches!(el.closest(".mapping-bar"), Ok(Some(_))))
}

/// Whether a menu or a dialog is open in front of the page.
fn something_in_front() -> bool {
    matches!(
        document().query_selector(".modal, .context-menu, .ctx-catcher"),
        Ok(Some(_))
    )
}

/// The bar that says which source is armed, shown while one is, and the drags it turns
/// into mappings. Mounted once, in the shell.
#[component]
pub fn MappingBar() -> impl IntoView {
    let mode = expect_context::<MappingMode>();
    let studio = expect_context::<Studio>();
    // Where the pointer was last, and how far it has gone in all, during a drag.
    let last = StoredValue::new(None::<(f64, f64)>);
    let listeners = StoredValue::new_local(Vec::<Captured>::new());
    let unlisten = move || {
        listeners.update_value(|all| {
            for l in all.drain(..) {
                l.remove();
            }
        });
    };
    // Ends the drag under way after `gone` pixels of travel: a mapping, or nothing for
    // what was only a click.
    let finish = move |gone: f64| {
        last.set_value(None);
        if gone < CLICK_SLOP {
            mode.drag.set(None);
        } else {
            mode.drag.update(|d| {
                if let Some(d) = d {
                    d.done = true;
                }
            });
        }
    };
    let listen = move || {
        let down = Captured::add("pointerdown", move |e: web_sys::PointerEvent| {
            if e.button() != 0 || in_bar(&e) {
                return;
            }
            let Some(pressed) = target_of(&e) else {
                return;
            };
            let Some(setting) = setting_at(pressed.clone()) else {
                return;
            };
            e.prevent_default();
            e.stop_propagation();
            // The drag keeps the pointer, so its release is heard outside the window.
            _ = pressed.set_pointer_capture(e.pointer_id());
            last.set_value(Some((crate::ui_scale::pointer(&e).1, 0.0)));
            mode.drag.set(Some(Drag {
                setting,
                amount: 0.0,
                done: false,
            }));
        });
        let moving = Captured::add("pointermove", move |e: web_sys::PointerEvent| {
            let Some((y, gone)) = last.get_value() else {
                return;
            };
            // The button came up where it wasn't heard: the drag ends where it got to.
            if e.buttons() & 1 == 0 {
                finish(gone);
                return;
            }
            // In the page's CSS pixels, so a drag goes as far at every UI scale.
            let now = crate::ui_scale::pointer(&e).1;
            let dy = now - y;
            last.set_value(Some((now, gone + dy.abs())));
            mode.drag.update(|d| {
                if let Some(d) = d {
                    d.amount = dragged(d.amount, dy, e.shift_key());
                }
            });
        });
        let up = Captured::add("pointerup", move |_: web_sys::PointerEvent| {
            if let Some((_, gone)) = last.get_value() {
                finish(gone);
            }
        });
        // The browser took the pointer away (a touch that became a scroll, a lost
        // capture): nothing is mapped.
        let cancel = Captured::add("pointercancel", move |_: web_sys::PointerEvent| {
            if last.get_value().is_some() {
                last.set_value(None);
                mode.drag.set(None);
            }
        });
        // A press on a setting never reaches it while a source is armed, so neither
        // does the click that follows.
        let click = Captured::add("click", move |e: web_sys::MouseEvent| {
            if !in_bar(&e) && target_of(&e).and_then(setting_at).is_some() {
                e.prevent_default();
                e.stop_propagation();
            }
        });
        let keys = Captured::add("keydown", move |e: web_sys::KeyboardEvent| {
            let typing = target_of(&e).is_some_and(|el| {
                matches!(
                    el.tag_name().to_ascii_lowercase().as_str(),
                    "input" | "textarea" | "select"
                )
            });
            let step = match e.key().as_str() {
                "Escape" => {
                    let dragging = last.get_value().is_some();
                    // A menu or a dialog in front takes its own Escape first.
                    if !dragging && something_in_front() {
                        return;
                    }
                    e.prevent_default();
                    e.stop_propagation();
                    if dragging {
                        last.set_value(None);
                        mode.drag.set(None);
                    } else {
                        mode.disarm();
                    }
                    return;
                }
                "[" if !typing => -1,
                "]" if !typing => 1,
                _ => return,
            };
            let next = mode.armed.with_untracked(|a| {
                let a = a.as_ref()?;
                studio
                    .collection
                    .with_untracked(|c| stepped_in(c.as_ref(), &a.reader, step))
            });
            if let Some(reader) = next {
                e.prevent_default();
                e.stop_propagation();
                mode.arm(reader);
            }
        });
        listeners.set_value(vec![down, moving, up, cancel, click, keys]);
    };
    let is_armed = Memo::new(move |_| mode.armed.with(Option::is_some));
    Effect::new(move |_| {
        unlisten();
        let body = document().body();
        if is_armed.get() {
            listen();
            if let Some(body) = &body {
                _ = body.class_list().add_1("mapping-mode");
            }
        } else if let Some(body) = &body {
            _ = body.class_list().remove_1("mapping-mode");
        }
    });
    on_cleanup(move || {
        unlisten();
        if let Some(body) = document().body() {
            _ = body.class_list().remove_1("mapping-mode");
        }
    });
    move || {
        let armed = mode.armed.get()?;
        Some(view! {
            <div class="mapping-bar" role="status">
                <span class="mapping-bar-dot" aria-hidden="true"></span>
                <span class="mapping-bar-name">"Mapping " {armed.name}</span>
                <span class="mapping-bar-help">
                    "Drag up or down on a setting to set how far this moves it. Shift drags finely."
                </span>
                <button type="button" class="secondary" title="Leave mapping mode (Esc)" on:click=move |_| mode.disarm()>
                    "Done"
                </button>
            </div>
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_up_adds_and_shift_goes_a_tenth_as_far() {
        assert!((dragged(0.0, -FULL_TRAVEL / 2.0, false) - 0.5).abs() < 1e-9);
        assert!((dragged(0.0, FULL_TRAVEL / 2.0, false) + 0.5).abs() < 1e-9);
        assert!((dragged(0.0, -FULL_TRAVEL / 2.0, true) - 0.05).abs() < 1e-9);
        assert_eq!(dragged(0.9, -FULL_TRAVEL, false), 1.0);
        assert_eq!(dragged(-0.9, FULL_TRAVEL, false), -1.0);
    }

    #[test]
    fn a_new_mapping_starts_where_the_setting_is_and_stays_in_range() {
        // A fader at -10 dB over -60 to +6: a quarter of the range up is 16.5 dB.
        assert_eq!(
            ends_after(None, Some(-10.0), (-60.0, 6.0), 0.25),
            (-10.0, 6.0)
        );
        assert_eq!(
            ends_after(None, Some(-10.0), (-60.0, 6.0), -0.25),
            (-10.0, -26.5)
        );
        // No value known: from the low end.
        assert_eq!(ends_after(None, None, (0.0, 100.0), 0.5), (0.0, 50.0));
    }

    #[test]
    fn a_setting_the_source_maps_already_keeps_its_start() {
        assert_eq!(
            ends_after(Some((20.0, 40.0)), Some(33.0), (0.0, 100.0), 0.1),
            (20.0, 50.0)
        );
        assert_eq!(
            ends_after(Some((20.0, 40.0)), Some(33.0), (0.0, 100.0), -0.5),
            (20.0, 0.0)
        );
    }

    #[test]
    fn the_amount_reads_as_a_signed_share_of_the_range() {
        assert_eq!(amount_label((0.0, 15.0), (0.0, 100.0)), "+15 %");
        assert_eq!(amount_label((50.0, 20.0), (0.0, 100.0)), "-30 %");
        assert_eq!(amount_label((10.0, 12.5), (0.0, 100.0)), "+2.5 %");
        assert_eq!(amount_label((10.0, 10.0), (0.0, 100.0)), "0 %");
        assert_eq!(amount_label((1.0, 2.0), (5.0, 5.0)), "0 %");
    }

    #[test]
    fn the_brackets_step_the_armed_mod_and_stop_at_the_ends() {
        let third = mod_reader(Some("Bitwig"), 2);
        assert_eq!(stepped(&third, 1), Some(mod_reader(Some("Bitwig"), 3)));
        assert_eq!(stepped(&mod_reader(None, 0), -1), Some(mod_reader(None, 0)));
        assert_eq!(
            stepped(&mod_reader(None, MOD_COUNT - 1), 1),
            Some(mod_reader(None, MOD_COUNT - 1))
        );
        let midi = crate::panels::map_control::midi_reader(
            jstream_control::protocol::MotionMidiControl::Cc,
            7,
            1,
            "",
        );
        assert_eq!(stepped(&midi, 1), None);
    }

    #[test]
    fn the_brackets_step_onto_and_from_a_named_mod_as_its_channel() {
        let mut c = Collection::default();
        let edits =
            mapping_index::name_mod_edits(&c, "Bitwig", 21, "Zoom", mod_reader(Some("Bitwig"), 20));
        let Some(jstream_control::protocol::Edit::ChannelSet { channel }) = edits.first() else {
            panic!("naming a Mod makes its channel");
        };
        c.channels.push(channel.clone());
        let zoom = mapping_index::channel_reader(&channel.channel_id);
        let twentieth = mod_reader(Some("Bitwig"), 19);
        assert_eq!(stepped_in(Some(&c), &twentieth, 1), Some(zoom.clone()));
        assert_eq!(
            stepped_in(Some(&c), &zoom, 1),
            Some(mod_reader(Some("Bitwig"), 21))
        );
        assert_eq!(stepped_in(Some(&c), &zoom, -1), Some(twentieth));
        assert_eq!(
            stepped_in(None, &zoom, 1),
            None,
            "a channel alone isn't a Mod"
        );
    }
}
