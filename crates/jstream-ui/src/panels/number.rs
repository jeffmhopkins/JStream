//! The number field the panels share (spec-panels.md G10): a value with a unit, which
//! takes typed text, arrow keys and a pointer drag across it or its caption; the mixer's
//! gain fields add − and + steppers. It sends one value per change: on Enter or blur for
//! typed text, per press for a stepper or an arrow key, and once on release for a drag,
//! so each is one edit and one undo step. While a drag is under way, a field given
//! `live` also hands it the dragged value at most once per animation frame (#1734), so
//! the output or a dialog's draft can follow the drag before the release sends it: the
//! Inspector sends those values as a gesture's `live.set` (ADR-0032), a dialog puts them
//! in its draft.
//!
//! Docs: docs/UI-FEATURES.md#theme, crates/jstream-ui/README.md#design

use jstream_control::protocol::ParamType;
use leptos::ev;
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

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
/// `last` is the pointer's position along the drag's axis, growing towards larger values.
#[derive(Clone, Copy, Debug)]
struct Scrub {
    pointer: i32,
    last: f64,
    started: bool,
    /// The committed value the drag started from.
    start: f64,
    value: f64,
}

/// What a field given `live` hands on while it is dragged (#1734).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scrubbing {
    /// The value the drag shows now, kept to the field's range and rounded: at most one
    /// per animation frame, and none after the drag ends.
    Moved(f64),
    /// The drag was released, after its value went to `send`, which may have refused it
    /// or found it unchanged. A gesture that `send` didn't end ends here.
    Ended,
    /// The drag stopped without a release (the pointer was lost); the value it started
    /// from is given, to put back.
    Cancelled(f64),
}

/// The newest dragged value waiting for its animation frame: offered on each pointer
/// move, taken by the frame, and dropped when the drag ends, so at most one value goes
/// out per frame and none after the release.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameQueue {
    pending: Option<f64>,
    scheduled: bool,
}

impl FrameQueue {
    /// Holds `value` for the next frame, replacing one not yet taken. Returns whether a
    /// frame must be asked for: none is while one is already on its way.
    pub fn offer(&mut self, value: f64) -> bool {
        self.pending = Some(value);
        !std::mem::replace(&mut self.scheduled, true)
    }

    /// The frame's value, if one is still waiting.
    pub fn take(&mut self) -> Option<f64> {
        self.scheduled = false;
        self.pending.take()
    }

    /// Drops the waiting value: the drag ended before its frame.
    pub fn clear(&mut self) {
        self.pending = None;
    }
}

/// A field's `live` when its value is a dialog's draft, applied only on OK: each
/// dragged value goes into the draft as it goes, with no engine call, and a drag lost
/// before its release puts the start back (#1734).
pub fn into_draft(
    set: impl Fn(f64) + Copy + Send + Sync + 'static,
) -> impl Fn(Scrubbing) + Copy + Send + Sync + 'static {
    move |scrubbing| match scrubbing {
        Scrubbing::Moved(v) | Scrubbing::Cancelled(v) => set(v),
        Scrubbing::Ended => {}
    }
}

/// How a kind descriptor's number parameter (ADR-0025) is shown as a [`NumberField`]:
/// its step, decimal places and range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumberSpec {
    /// What an arrow key or [`SCRUB_PX`] of drag adds.
    pub step: f64,
    /// The decimal places the value is rounded to.
    pub decimals: usize,
    /// The smallest value.
    pub min: f64,
    /// The largest value.
    pub max: f64,
}

/// The decimal places `step` is written with: 0 for 1, 2 for 0.05. At most 6.
fn places(step: f64) -> usize {
    (0..6)
        .find(|&n| {
            let scaled = step * 10_f64.powi(n);
            (scaled - scaled.round()).abs() < 1e-9
        })
        .map_or(6, |n| n as usize)
}

/// A number parameter's field: a whole number steps by its step (1 by default); a
/// decimal one by its step, or a hundredth of its range, or 0.01, and keeps two more
/// places than its step, so a fine drag or a typed value isn't rounded off. A range
/// the descriptor leaves open is the field's widest. `None` for any other type.
pub fn number_spec(ty: &ParamType) -> Option<NumberSpec> {
    const WIDEST: f64 = 1e9;
    match ty {
        ParamType::Int { min, max, step, .. } => Some(NumberSpec {
            step: step.filter(|s| *s > 0).map_or(1.0, |s| s as f64),
            decimals: 0,
            min: min.map_or(-WIDEST, |n| n as f64),
            max: max.map_or(WIDEST, |n| n as f64),
        }),
        ParamType::Float { min, max, step, .. } => {
            let step = step
                .filter(|s| *s > 0.0)
                .unwrap_or_else(|| match (min, max) {
                    (Some(lo), Some(hi)) if hi > lo => nice_step((hi - lo) / 100.0),
                    _ => 0.01,
                });
            Some(NumberSpec {
                step,
                decimals: (places(step) + 2).min(6),
                min: min.unwrap_or(-WIDEST),
                max: max.unwrap_or(WIDEST),
            })
        }
        _ => None,
    }
}

/// The step of 1, 2 or 5 times a power of ten nearest under `raw`.
fn nice_step(raw: f64) -> f64 {
    let power = 10_f64.powf(raw.log10().floor());
    let first = raw / power;
    let lead = if first >= 5.0 {
        5.0
    } else if first >= 2.0 {
        2.0
    } else {
        1.0
    };
    lead * power
}

/// A number field with a unit, and a label above it, or `named` in place of the label. `value` is the committed value, `None` to disable the field;
/// `send` gets each new value, kept to `[min, max]` and rounded to `decimals` places.
#[component]
pub fn NumberField(
    /// The label above the field, also the input's accessible name.
    #[prop(optional)]
    label: &'static str,
    /// The input's accessible name, for a field shown without a label (a row that
    /// names it already).
    #[prop(optional, into)]
    named: Option<Signal<String>>,
    /// Stacks + over the input over − with the unit below, for a narrow column, and
    /// scrubs up and down, up for larger values.
    #[prop(optional)]
    vertical: bool,
    /// Shows the label as a short caption inside the field's box, before the number, for
    /// a row of small fields under one row label (an [`AxisRow`]). A drag on the caption
    /// changes the value as a drag on the number does.
    #[prop(optional)]
    inline: bool,
    /// The caption shown in place of `label`, which stays the accessible name: "L" for
    /// Left.
    #[prop(optional)]
    caption: &'static str,
    /// Adds − and + steppers on either side of the number. A field has none by default
    /// (docs/UI-DESIGN.md#forms): dragging it, the arrow keys and typing change it.
    #[prop(optional)]
    steppers: bool,
    /// The unit after the number, such as `px`.
    #[prop(optional, into)]
    unit: String,
    /// The committed value; `None` disables the field.
    #[prop(into)]
    value: Signal<Option<f64>>,
    /// While true the field shows no number, only a dash, though the steppers, arrow
    /// keys and a drag still start from `value`: for a field that follows another
    /// value until it is set.
    #[prop(optional, into)]
    blank: Option<Signal<bool>>,
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
    /// Takes a drag's values while it is under way, at most one per animation frame, and
    /// then how it ended (#1734). Without it, a drag only shows its value until the
    /// release sends it.
    #[prop(optional, into)]
    live: Option<Callback<Scrubbing>>,
) -> AnyView {
    // The value a drag shows before its release sends it.
    let dragged = RwSignal::new(None::<f64>);
    // Bumped to show the committed value again after cancelled or refused text.
    let again = RwSignal::new(0_u32);
    let scrub = StoredValue::new(None::<Scrub>);
    // The dragged value waiting for its frame, for `live`.
    let frames = StoredValue::new(FrameQueue::default());
    // Whether a drag just ended, until the click that follows it.
    let dropped = StoredValue::new(false);
    let input = NodeRef::<leptos::html::Input>::new();

    let shown = move || {
        let blank = blank.is_some_and(|b| b.get());
        dragged
            .get()
            .or_else(|| value.get().filter(|_| !blank))
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
    // Hands the dragged value to `live` on the next animation frame, unless the drag has
    // ended by then.
    let offer = move |v: f64| {
        let Some(live) = live else { return };
        if !frames.try_update_value(|f| f.offer(v)).unwrap_or(false) {
            return;
        }
        request_animation_frame(move || {
            let Some(Some(v)) = frames.try_update_value(FrameQueue::take) else {
                return;
            };
            if scrub
                .try_with_value(|s| s.is_some_and(|s| s.started))
                .unwrap_or(false)
            {
                live.run(Scrubbing::Moved(v));
            }
        });
    };
    // Ends a drag that never reached its release: `live` puts its start back.
    let drop_scrub = move || {
        let Some(s) = scrub.get_value() else { return };
        scrub.set_value(None);
        dragged.set(None);
        frames.update_value(FrameQueue::clear);
        if s.started
            && let Some(live) = live
        {
            live.run(Scrubbing::Cancelled(s.start));
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
    // The pointer along the drag's axis: right, or up in a vertical field.
    // In the page's CSS pixels, so a drag moves as far at every UI scale (`ui_scale.rs`).
    let along = move |event: &ev::PointerEvent| {
        let (x, y) = crate::ui_scale::pointer(event);
        if vertical { -y } else { x }
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
            last: along(&event),
            started: false,
            start: v,
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
            drop_scrub();
            return;
        }
        let x = along(&event);
        if !s.started {
            if (x - s.last).abs() < SCRUB_PX {
                return;
            }
            s.started = true;
            event.prevent_default();
            // The field the drag started on keeps the pointer: the input, or the caption
            // in front of it.
            if let Some(el) = event
                .current_target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            {
                _ = el.set_pointer_capture(s.pointer);
            }
            if let Some(el) = input.get_untracked() {
                _ = el.blur();
            }
        }
        let keys = Keys {
            shift: event.shift_key(),
            coarse: event.ctrl_key() || event.meta_key() || event.alt_key(),
        };
        // Each move adds its own travel at the modifiers held now, so pressing or
        // releasing one mid-drag never makes the value jump.
        s.value = (s.value + scrub_delta(x - s.last, step, keys)).clamp(min, max);
        s.last = x;
        scrub.set_value(Some(s));
        let shown = settle(s.value, min, max, decimals);
        if dragged.get_untracked() != Some(shown) {
            dragged.set(Some(shown));
            offer(shown);
        }
    };
    let on_up = move |event: ev::PointerEvent| {
        let Some(s) = scrub.get_value() else { return };
        scrub.set_value(None);
        frames.update_value(FrameQueue::clear);
        if s.started {
            event.prevent_default();
            dragged.set(None);
            dropped.set_value(true);
            put(s.value);
            if let Some(live) = live {
                live.run(Scrubbing::Ended);
            }
        }
    };
    // The click a drag ends with would focus the input through its label: drop it.
    let on_click = move |event: ev::MouseEvent| {
        if dropped.get_value() {
            dropped.set_value(false);
            event.prevent_default();
        }
    };
    let on_cancel = move |_: ev::PointerEvent| drop_scrub();

    let name = move || named.map_or_else(|| label.to_owned(), |n| n.get());
    // A caption that differs from the label would give the input two names.
    let caption_hidden = (!caption.is_empty()).then_some("true");
    let field = move || {
        view! {
            <input
                type="text"
                inputmode="decimal"
                class="mono"
                aria-label=name
                title="Drag to change · Shift: fine · Ctrl: coarse · or type a value"
                node_ref=input
                disabled=disabled
                placeholder=move || blank.map(|_| "—")
                prop:value=text
                on:change=on_change
                on:keydown=on_key
                on:pointerdown=on_down
                on:pointermove=on_move
                on:pointerup=on_up
                on:pointercancel=on_cancel
            />
        }
    };
    let decrease = move || {
        view! {
            <button type="button" tabindex="-1" aria-label=move || format!("Decrease {}", name())
                disabled=disabled on:click=move |_| nudge(-step)>"−"</button>
        }
    };
    let increase = move || {
        view! {
            <button type="button" tabindex="-1" aria-label=move || format!("Increase {}", name())
                disabled=disabled on:click=move |_| nudge(step)>"+"</button>
        }
    };
    if vertical {
        return view! {
            <label class="number-field vertical">
                {increase()}
                {field()}
                {decrease()}
                <span class="unit">{unit}</span>
            </label>
        }
        .into_any();
    }
    let shown = if caption.is_empty() { label } else { caption };
    // An inline field's caption sits inside its box, before the number, and a drag on it
    // changes the value as a drag on the number does.
    let prefix = (inline && named.is_none()).then(|| {
        view! {
            <span class="field-label prefix" aria-hidden=caption_hidden
                on:pointerdown=on_down on:pointermove=on_move on:pointerup=on_up
                on:pointercancel=on_cancel>{shown}</span>
        }
    });
    let above = (!inline && named.is_none()).then(|| {
        view! { <span class="field-label" aria-hidden=caption_hidden>{shown}</span> }
    });
    view! {
        <label class="number-field" class:inline=inline class:bare=!steppers on:click=on_click>
            {above}
            <span class="stepper">
                {steppers.then(decrease)}
                {prefix}
                {field()}
                <span class="unit">{unit}</span>
                {steppers.then(increase)}
            </span>
        </label>
    }
    .into_any()
}

/// A row of small fields under one label on its left, such as Position's X and Y or
/// Crop's four sides (#1219), the same row the scene Properties dialog's camera uses
/// (#1214). Its fields share the row's width equally and never push it wider than its
/// column.
#[component]
pub fn AxisRow(
    /// The row's label.
    label: &'static str,
    /// The fields, each usually an `inline` [`NumberField`].
    children: Children,
) -> impl IntoView {
    view! {
        <div class="axis-row" role="group" aria-label=label>
            <span class="label">{label}</span>
            <div class="axes">{children()}</div>
        </div>
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
    fn a_drag_hands_on_one_value_per_frame_and_none_after_it_ends() {
        let mut frames = FrameQueue::default();
        // The first move asks for a frame; the moves before it arrives only replace
        // its value.
        assert!(frames.offer(1.0));
        assert!(!frames.offer(2.0));
        assert!(!frames.offer(3.0));
        assert_eq!(frames.take(), Some(3.0));
        // The next move asks for the next frame.
        assert!(frames.offer(4.0));
        // A release before that frame drops its value: the frame finds nothing.
        frames.clear();
        assert_eq!(frames.take(), None);
        assert!(frames.offer(5.0));
    }

    #[test]
    fn descriptor_numbers_get_their_step_places_and_range() {
        let int = ParamType::Int {
            min: Some(1),
            max: Some(64),
            step: None,
            unit: Some("px".into()),
        };
        assert_eq!(
            number_spec(&int),
            Some(NumberSpec {
                step: 1.0,
                decimals: 0,
                min: 1.0,
                max: 64.0
            })
        );
        let stepped = ParamType::Float {
            min: Some(0.0),
            max: Some(1.0),
            step: Some(0.05),
            unit: None,
        };
        assert_eq!(
            number_spec(&stepped).map(|s| (s.step, s.decimals)),
            Some((0.05, 4))
        );
        // No step: a hundredth of the range, rounded down to 1, 2 or 5 of a power of ten.
        let ranged = ParamType::Float {
            min: Some(-24.0),
            max: Some(24.0),
            step: None,
            unit: Some("dB".into()),
        };
        assert_eq!(
            number_spec(&ranged).map(|s| (s.step, s.decimals)),
            Some((0.2, 3))
        );
        let open = ParamType::Float {
            min: None,
            max: None,
            step: None,
            unit: None,
        };
        let spec = number_spec(&open).expect("a number");
        assert_eq!((spec.step, spec.decimals), (0.01, 4));
        assert!(spec.min < -1e6 && spec.max > 1e6);
        assert_eq!(number_spec(&ParamType::Bool), None);
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
