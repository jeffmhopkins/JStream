//! The number field the panels share (spec-panels.md G10): a value with − and + steppers
//! and a unit, which takes typed text, arrow keys and a pointer drag across it. It sends
//! one value per change: on Enter or blur for typed text, per press for a stepper or an
//! arrow key, and once on release for a drag, so each is one edit and one undo step.
//!
//! Docs: docs/UI-FEATURES.md#theme, crates/jstream-ui/README.md#design

use leptos::ev;
use leptos::prelude::*;

/// How far the pointer travels for one step of a drag, in CSS pixels.
pub const SCRUB_PX: f64 = 3.0;

/// The minus sign numbers are written with.
const MINUS: char = '\u{2212}';

/// Typed text as a number: `−` (U+2212) reads as a minus and `,` as the decimal point.
/// `None` for text that isn't a finite number.
pub fn parse_number(text: &str) -> Option<f64> {
    let text: String = text
        .trim()
        .chars()
        .map(|c| match c {
            MINUS => '-',
            ',' => '.',
            c => c,
        })
        .collect();
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A number as a field shows it: rounded to `decimals` places, without trailing zeros
/// unless `fixed`, with `−` for a minus. A negative that rounds to zero reads as zero.
pub fn format_number(value: f64, decimals: usize, fixed: bool) -> String {
    let text = format!("{value:.decimals$}");
    let text = if fixed || !text.contains('.') {
        text
    } else {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| matches!(c, '0' | '.')) => rest.to_owned(),
        Some(rest) => format!("{MINUS}{rest}"),
        None => text,
    }
}

/// The value kept to `[min, max]` and rounded to `decimals` places.
pub fn settle(value: f64, min: f64, max: f64, decimals: usize) -> f64 {
    let scale = 10_f64.powi(i32::try_from(decimals).unwrap_or(6));
    (value.clamp(min, max) * scale).round() / scale
}

/// The modifier keys held during a key press or a drag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    /// Shift: a finer step.
    pub shift: bool,
    /// Ctrl, Meta or Alt: a coarser step.
    pub coarse: bool,
}

/// What one arrow key press adds: `step`, a tenth of it with Shift when the step is
/// under 1, ten times it with Ctrl, Meta or Alt.
pub fn key_step(step: f64, keys: Keys) -> f64 {
    if keys.coarse {
        step * 10.0
    } else if keys.shift && step < 1.0 {
        step / 10.0
    } else {
        step
    }
}

/// What `px` of drag adds: one `step` per [`SCRUB_PX`], a tenth with Shift, four times
/// with Ctrl, Meta or Alt.
pub fn scrub_delta(px: f64, step: f64, keys: Keys) -> f64 {
    let factor = if keys.coarse {
        4.0
    } else if keys.shift {
        0.1
    } else {
        1.0
    };
    px / SCRUB_PX * step * factor
}

/// A drag across the field: where it started, how far it has gone, and the value so far.
#[derive(Clone, Copy, Debug)]
struct Scrub {
    pointer: i32,
    last_x: f64,
    started: bool,
    value: f64,
}

/// A number field with − and + steppers, a unit, and a label above it. `value` is the
/// committed value, `None` to disable the field; `send` gets each new value, kept to
/// `[min, max]` and rounded to `decimals` places.
#[component]
pub fn NumberField(
    /// The label above the field, also the input's accessible name.
    label: &'static str,
    /// The unit after the number, such as `px`.
    #[prop(optional)]
    unit: &'static str,
    /// The committed value; `None` disables the field.
    #[prop(into)]
    value: Signal<Option<f64>>,
    /// What a stepper, an arrow key or [`SCRUB_PX`] of drag adds.
    step: f64,
    /// The decimal places values are rounded to and shown with.
    decimals: usize,
    /// Whether to show every decimal place, trailing zeros included.
    #[prop(optional)]
    fixed: bool,
    /// The smallest value sent.
    #[prop(default = -100_000.0)]
    min: f64,
    /// The largest value sent.
    #[prop(default = 100_000.0)]
    max: f64,
    /// Takes each new value, and says whether it sent an edit; when it didn't, the field
    /// shows the committed value again.
    send: impl Fn(f64) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    // The value a drag shows before its release sends it.
    let dragged = RwSignal::new(None::<f64>);
    // Bumped to show the committed value again after cancelled or refused text.
    let again = RwSignal::new(0_u32);
    let scrub = StoredValue::new(None::<Scrub>);
    let input = NodeRef::<leptos::html::Input>::new();

    let shown = move || {
        dragged
            .get()
            .or_else(|| value.get())
            .map_or_else(String::new, |v| format_number(v, decimals, fixed))
    };
    let text = move || {
        again.track();
        shown()
    };
    let disabled = move || value.get().is_none();
    let put = move |v: f64| {
        let v = settle(v, min, max, decimals);
        if value.get_untracked() == Some(v) || !send(v) {
            again.update(|n| *n += 1);
        }
    };
    let nudge = move |by: f64| {
        if let Some(v) = value.get_untracked() {
            put(v + by);
        }
    };
    let blur = move || {
        if let Some(el) = input.get_untracked() {
            _ = el.blur();
        }
    };

    let on_change = move |event: ev::Event| match parse_number(&event_target_value(&event)) {
        Some(v) => put(v),
        None => again.update(|n| *n += 1),
    };
    let on_key = move |event: ev::KeyboardEvent| {
        let keys = Keys {
            shift: event.shift_key(),
            coarse: event.ctrl_key() || event.meta_key() || event.alt_key(),
        };
        match event.key().as_str() {
            "Enter" => blur(),
            "Escape" => {
                event.prevent_default();
                // Put the committed text back before the blur, which would otherwise
                // send the typed text as a change before the re-render reaches the input.
                if let Some(el) = input.get_untracked() {
                    el.set_value(&untrack(shown));
                }
                again.update(|n| *n += 1);
                blur();
            }
            "ArrowUp" => {
                event.prevent_default();
                nudge(key_step(step, keys));
            }
            "ArrowDown" => {
                event.prevent_default();
                nudge(-key_step(step, keys));
            }
            _ => {}
        }
    };

    // A drag starts only on an unfocused field, so a press in a focused one places the
    // caret as usual.
    let focused = move || {
        let Some(el) = input.get_untracked() else {
            return false;
        };
        document().active_element().is_some_and(|a| {
            let a: &web_sys::Element = &a;
            let el: &web_sys::Element = &el;
            a == el
        })
    };
    let on_down = move |event: ev::PointerEvent| {
        if event.button() != 0 || focused() {
            return;
        }
        let Some(v) = value.get_untracked() else {
            return;
        };
        dragged.set(None);
        scrub.set_value(Some(Scrub {
            pointer: event.pointer_id(),
            last_x: f64::from(event.client_x()),
            started: false,
            value: v,
        }));
    };
    let on_move = move |event: ev::PointerEvent| {
        let Some(mut s) = scrub.get_value() else {
            return;
        };
        // A release the field never saw (outside the window, before capture) leaves
        // no button held: drop the scrub rather than drag with nothing pressed.
        if event.buttons() == 0 {
            scrub.set_value(None);
            dragged.set(None);
            return;
        }
        let x = f64::from(event.client_x());
        if !s.started {
            if (x - s.last_x).abs() < SCRUB_PX {
                return;
            }
            s.started = true;
            event.prevent_default();
            if let Some(el) = input.get_untracked() {
                _ = el.set_pointer_capture(s.pointer);
                _ = el.blur();
            }
        }
        let keys = Keys {
            shift: event.shift_key(),
            coarse: event.ctrl_key() || event.meta_key() || event.alt_key(),
        };
        // Each move adds its own travel at the modifiers held now, so pressing or
        // releasing one mid-drag never makes the value jump.
        s.value = (s.value + scrub_delta(x - s.last_x, step, keys)).clamp(min, max);
        s.last_x = x;
        scrub.set_value(Some(s));
        dragged.set(Some(settle(s.value, min, max, decimals)));
    };
    let on_up = move |event: ev::PointerEvent| {
        let Some(s) = scrub.get_value() else { return };
        scrub.set_value(None);
        if s.started {
            event.prevent_default();
            dragged.set(None);
            put(s.value);
        }
    };
    let on_cancel = move |_: ev::PointerEvent| {
        scrub.set_value(None);
        dragged.set(None);
    };

    view! {
        <label class="number-field">
            <span class="field-label">{label}</span>
            <span class="stepper">
                <button type="button" tabindex="-1" aria-label=format!("Decrease {label}")
                    disabled=disabled on:click=move |_| nudge(-step)>"−"</button>
                <input
                    type="text"
                    inputmode="decimal"
                    class="mono"
                    aria-label=label
                    title="Drag to change · Shift: fine · Ctrl: coarse · or type a value"
                    node_ref=input
                    disabled=disabled
                    prop:value=text
                    on:change=on_change
                    on:keydown=on_key
                    on:pointerdown=on_down
                    on:pointermove=on_move
                    on:pointerup=on_up
                    on:pointercancel=on_cancel
                />
                <span class="unit">{unit}</span>
                <button type="button" tabindex="-1" aria-label=format!("Increase {label}")
                    disabled=disabled on:click=move |_| nudge(step)>"+"</button>
            </span>
        </label>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_takes_a_unicode_minus_and_a_comma() {
        assert_eq!(parse_number(" 12.5 "), Some(12.5));
        assert_eq!(parse_number("\u{2212}3,5"), Some(-3.5));
        assert_eq!(parse_number("-7"), Some(-7.0));
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn numbers_are_written_with_the_unicode_minus() {
        assert_eq!(format_number(1384.0, 2, false), "1384");
        assert_eq!(format_number(112.2, 2, false), "112.2");
        assert_eq!(format_number(0.0, 1, true), "0.0");
        assert_eq!(format_number(-12.5, 1, true), "\u{2212}12.5");
        assert_eq!(format_number(-0.001, 2, false), "0");
        assert_eq!(format_number(-0.01, 1, true), "0.0");
    }

    #[test]
    fn values_settle_in_range_and_rounded() {
        assert_eq!(settle(12.345, 0.0, 100.0, 1), 12.3);
        assert_eq!(settle(-5.0, 0.0, 100.0, 0), 0.0);
        assert_eq!(settle(1e9, -1e5, 1e5, 0), 1e5);
    }

    #[test]
    fn arrow_steps_follow_the_modifiers() {
        let plain = Keys::default();
        let shift = Keys {
            shift: true,
            coarse: false,
        };
        let ctrl = Keys {
            shift: false,
            coarse: true,
        };
        assert_eq!(key_step(1.0, plain), 1.0);
        // Shift is finer only for a step under 1.
        assert_eq!(key_step(1.0, shift), 1.0);
        assert_eq!(key_step(0.5, shift), 0.05);
        assert_eq!(key_step(1.0, ctrl), 10.0);
    }

    #[test]
    fn a_drag_adds_one_step_per_three_pixels() {
        let plain = Keys::default();
        assert_eq!(scrub_delta(9.0, 1.0, plain), 3.0);
        assert_eq!(scrub_delta(-3.0, 0.5, plain), -0.5);
        let shift = Keys {
            shift: true,
            coarse: false,
        };
        assert!((scrub_delta(30.0, 1.0, shift) - 1.0).abs() < 1e-9);
        let ctrl = Keys {
            shift: false,
            coarse: true,
        };
        assert_eq!(scrub_delta(3.0, 1.0, ctrl), 4.0);
    }
}
