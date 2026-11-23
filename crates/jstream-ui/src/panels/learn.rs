//! Controller learn (ADR-0178): a button beside a live setting or a fader that binds the
//! next MIDI control moved to it, as a value binding, and shows and forgets the one it
//! has.
//!
//! Learning reads `midi.last_controls` once for a baseline, then about seven times a
//! second for up to 15 s; the first input whose count of control changes grows is the
//! control moved, and `binding.set_value` binds it over the setting's whole range (a
//! fader's over -60 to +6 dB). A binding the button made is named after its target
//! ([`learn_id`]), so learning again replaces it.

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::store::{Studio, query};
use jstream_control::protocol::{
    Action, ChannelSwitch, Chord, Edit, Finite32, MidiControlInfo, MotionCurve, MotionKind,
    MotionMidiControl, ParamDescriptor, ParamType, Query, QueryResult, TimelineId, ValueBinding,
    ValueTarget,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;

/// How often learning reads the inputs' latest controls.
const POLL: Duration = Duration::from_millis(150);
/// How many times learning reads the controls before it gives up: 15 s.
const TRIES: u32 = 100;
/// The throw a learned fader gets, in dB: wide enough to fade out, short of the top.
pub const FADER_THROW: (f64, f64) = (-60.0, 6.0);

/// What owns the settings a form shows, for the learn button beside each.
#[derive(Clone, Debug, PartialEq)]
pub enum LearnOwner {
    /// A source's own settings.
    Source(jstream_control::protocol::SourceId),
    /// A filter's settings.
    Filter(
        jstream_control::protocol::FilterOwner,
        jstream_control::protocol::FilterId,
    ),
}

impl LearnOwner {
    /// The value target of its setting `param`.
    pub fn target(&self, param: &str) -> ValueTarget {
        match self {
            LearnOwner::Source(source_id) => ValueTarget::SourceSetting {
                source_id: source_id.clone(),
                param: param.to_owned(),
            },
            LearnOwner::Filter(owner, filter_id) => ValueTarget::FilterSetting {
                owner: owner.clone(),
                filter_id: filter_id.clone(),
                param: param.to_owned(),
            },
        }
    }
}

/// The range a learned binding maps a controller onto for `param`, or `None` when
/// `param` takes no controller: a `live` number with both ends, or a `bool`.
pub fn param_range(param: &ParamDescriptor) -> Option<(f64, f64)> {
    if !param.live {
        return None;
    }
    match &param.ty {
        ParamType::Int {
            min: Some(min),
            max: Some(max),
            ..
        } => Some((*min as f64, *max as f64)),
        ParamType::Float {
            min: Some(min),
            max: Some(max),
            ..
        } => Some((*min, *max)),
        ParamType::Bool => Some((0.0, 1.0)),
        _ => None,
    }
}

/// The ID the button names a binding of `target` with: `learn_` and a hash of the
/// target, so each target has one and learning again replaces it.
pub fn learn_id(target: &ValueTarget) -> String {
    // FNV-1a over the target's wire form: stable across runs and builds.
    let text = serde_json::to_string(target).unwrap_or_default();
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("learn_{hash:016x}")
}

/// The control in `now` that moved since `baseline`: the first input whose count grew,
/// or that wasn't there before.
pub fn moved(baseline: &[MidiControlInfo], now: &[MidiControlInfo]) -> Option<MidiControlInfo> {
    now.iter()
        .find(|control| {
            baseline
                .iter()
                .find(|before| before.input == control.input)
                .is_none_or(|before| control.count > before.count)
        })
        .cloned()
}

/// How long an input follows its source over (`smoothing_ms`), for the kinds that read a
/// control, live data or a channel; `None` for any other.
pub fn smoothing_of(input: &MotionKind) -> Option<u32> {
    match input {
        MotionKind::Midi { smoothing_ms, .. }
        | MotionKind::Data { smoothing_ms, .. }
        | MotionKind::Channel { smoothing_ms, .. } => Some(*smoothing_ms),
        _ => None,
    }
}

/// Sets `input`'s smoothing to `ms`, for the kinds [`smoothing_of`] reads.
pub fn set_smoothing(input: &mut MotionKind, ms: u32) {
    match input {
        MotionKind::Midi { smoothing_ms, .. }
        | MotionKind::Data { smoothing_ms, .. }
        | MotionKind::Channel { smoothing_ms, .. } => *smoothing_ms = ms,
        _ => {}
    }
}

/// The binding of `control` to `target` over `range`, named [`learn_id`].
pub fn learned(control: &MidiControlInfo, target: &ValueTarget, range: (f64, f64)) -> ValueBinding {
    let end = |v: f64| Finite32::new(v as f32).unwrap_or(Finite32::ZERO);
    ValueBinding {
        binding_id: learn_id(target),
        input: MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(control.controller),
            channel: Some(control.channel),
            input: Some(control.input.clone()),
            smoothing_ms: 50,
            curve: MotionCurve::Linear,
            from: end(range.0),
            to: end(range.1),
        },
        target: target.clone(),
        amount: None,
    }
}

/// What a binding reads, as the button shows it: `CC 7`, `Bend`, `Pressure` or `Data`.
pub fn short_name(binding: &ValueBinding) -> String {
    match &binding.input {
        MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(n),
            ..
        } => format!("CC {n}"),
        MotionKind::Midi {
            control: MotionMidiControl::PitchBend,
            ..
        } => "Bend".into(),
        MotionKind::Midi { .. } => "Pressure".into(),
        _ => "Data".into(),
    }
}

/// What a binding reads, in full, for the button's tooltip.
pub fn long_name(binding: &ValueBinding) -> String {
    match &binding.input {
        MotionKind::Midi { channel, input, .. } => {
            let on = channel.map_or("any channel".to_owned(), |c| format!("channel {c}"));
            let from = input.as_deref().unwrap_or("any input");
            format!(
                "{} on {on} from {from} drives this. Click to unbind it.",
                short_name(binding)
            )
        }
        MotionKind::Data {
            input, key, field, ..
        } => format!(
            "Field `{field}` of `{key}` from {} drives this. Click to unbind it.",
            input.as_deref().unwrap_or("any sender")
        ),
        _ => "A binding drives this. Click to unbind it.".into(),
    }
}

/// Each input's latest control change, or `None` when the engine answers none.
pub(crate) async fn controls(session: &crate::session::Session) -> Option<Vec<MidiControlInfo>> {
    match query(session, Query::MidiLastControls).await {
        Ok(QueryResult::MidiLastControls { controls }) => Some(controls),
        _ => None,
    }
}

/// Reads the controls after [`POLL`] and binds the first that moved since `baseline`,
/// or reads again, `tries` more times, while `learning` holds. The button's signals are
/// read with `try_`: once its form closes they are gone, and the loop stops.
fn watch(
    session: crate::session::Session,
    studio: Studio,
    learning: RwSignal<bool>,
    target: StoredValue<ValueTarget>,
    range: (f64, f64),
    baseline: Vec<MidiControlInfo>,
    tries: u32,
) {
    set_timeout(
        move || {
            spawn_local(async move {
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                let now = controls(&session).await.unwrap_or_default();
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                if let Some(control) = moved(&baseline, &now) {
                    let Some(binding) = target.try_with_value(|t| learned(&control, t, range))
                    else {
                        return;
                    };
                    studio.apply(&session, vec![Edit::BindingSetValue { binding }]);
                    learning.try_set(false);
                } else if tries == 0 {
                    learning.try_set(false);
                } else {
                    watch(
                        session,
                        studio,
                        learning,
                        target,
                        range,
                        baseline,
                        tries - 1,
                    );
                }
            });
        },
        POLL,
    );
}

/// The binding of channel `channel_id` to `target` over `range`, named [`learn_id`]
/// (ADR-0183 decision 5): the setting follows the channel, mapped onto `range`.
pub fn follows(channel_id: &str, target: &ValueTarget, range: (f64, f64)) -> ValueBinding {
    let end = |v: f64| Finite32::new(v as f32).unwrap_or(Finite32::ZERO);
    ValueBinding {
        binding_id: learn_id(target),
        input: MotionKind::Channel {
            channel_id: channel_id.to_owned(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: end(range.0),
            to: end(range.1),
        },
        target: target.clone(),
        amount: None,
    }
}

/// A learned MIDI control as a channel's input, reading over the channel's whole 0 to 1
/// (the binding mapped it onto the setting's range); `None` for any other input.
pub fn carried(input: MotionKind) -> Option<MotionKind> {
    match input {
        MotionKind::Midi {
            control,
            controller,
            channel,
            input,
            smoothing_ms,
            curve,
            ..
        } => Some(MotionKind::Midi {
            control,
            controller,
            channel,
            input,
            smoothing_ms,
            curve,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        }),
        _ => None,
    }
}

/// Where `value` sits in `range`, 0 to 1: a new channel's default, so the setting
/// doesn't move when it starts following it.
pub fn fraction(value: f64, range: (f64, f64)) -> f32 {
    let span = range.1 - range.0;
    if span.abs() < f64::EPSILON {
        return 0.0;
    }
    (((value - range.0) / span).clamp(0.0, 1.0)) as f32
}

/// A button that takes the next key pressed while it has the focus, which it takes at
/// once: `done` gets the chord, or `None` on Escape or once the focus leaves. The key
/// is the capture's, so no hotkey runs.
#[component]
pub fn KeyCapture(
    /// Takes the chord pressed, or `None` when the capture ends without one.
    done: impl Fn(Option<Chord>) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let finished = StoredValue::new(false);
    let finish = move |chord: Option<Chord>| {
        if finished.try_get_value() == Some(false) {
            finished.set_value(true);
            done(chord);
        }
    };
    let button = NodeRef::<leptos::html::Button>::new();
    button.on_load(|b| {
        request_animation_frame(move || {
            let _ = b.focus();
        });
    });
    let on_key = move |event: leptos::ev::KeyboardEvent| {
        event.stop_propagation();
        event.prevent_default();
        if event.key() == "Escape" {
            finish(None);
            return;
        }
        if let Some(chord) = crate::hotkeys::of_event(&event) {
            finish(Some(chord));
        }
    };
    view! {
        <button
            type="button"
            class="menuitem key-capture"
            node_ref=button
            on:keydown=on_key
            on:blur=move |_| finish(None)
        >
            <span class="ctx-check"></span>
            <span class="ctx-label">"Press a key…"</span>
            <span class="hint">"Esc cancels"</span>
        </button>
    }
}

/// A right-click on a setting's row, for the control button of that row: the button's
/// `data-learn` ID and where the pointer was. The shell provides it; each button opens
/// its menu there when the ID is its own.
#[derive(Clone, Copy)]
pub struct ControlMenuAt(pub RwSignal<Option<(String, f64, f64)>>);

impl ControlMenuAt {
    /// No right-click waiting.
    pub fn new() -> Self {
        ControlMenuAt(RwSignal::new(None))
    }
}

impl Default for ControlMenuAt {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a right-click on an element with this tag and `type` keeps the browser's own
/// menu: a field of free text, where copy and paste live.
fn keeps_own_menu(tag: &str, ty: &str) -> bool {
    match tag.to_ascii_lowercase().as_str() {
        "textarea" => true,
        "input" => matches!(ty, "" | "text" | "search" | "url" | "password" | "email"),
        _ => false,
    }
}

/// Hands a right-click to the control button of the setting it landed on, and says
/// whether it did. The setting is the nearest element around the click that holds
/// exactly one control button as its own child, looked for no further out than the
/// panel or dialog. A right-click in a field of free text is left to the browser.
pub fn row_context(at: ControlMenuAt, event: &leptos::ev::MouseEvent) -> bool {
    use leptos::wasm_bindgen::JsCast;
    let Some(mut el) = event
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return false;
    };
    if keeps_own_menu(
        &el.tag_name(),
        &el.get_attribute("type").unwrap_or_default(),
    ) {
        return false;
    }
    loop {
        if let Ok(buttons) = el.query_selector_all(":scope > button.learn") {
            match buttons.length() {
                0 => {}
                1 => {
                    let id = buttons
                        .item(0)
                        .and_then(|n| n.dyn_into::<web_sys::Element>().ok())
                        .and_then(|b| b.get_attribute("data-learn"));
                    let Some(id) = id else {
                        return false;
                    };
                    event.prevent_default();
                    at.0.set(Some((
                        id,
                        f64::from(event.client_x()),
                        f64::from(event.client_y()),
                    )));
                    return true;
                }
                _ => return false,
            }
        }
        let class = el.class_name();
        if class
            .split_whitespace()
            .any(|c| matches!(c, "panel" | "modal" | "shell"))
        {
            return false;
        }
        match el.parent_element() {
            Some(parent) => el = parent,
            None => return false,
        }
    }
}

/// What the control menu shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Menu {
    /// The menu, at a point in the window.
    Open(f64, f64),
    /// The channel search, at a point in the window.
    Follow(f64, f64),
    /// The sequences to add a lane to, at a point in the window.
    Sequence(f64, f64),
    /// Waiting for the key that switches the channel, at a point in the window.
    Key(f64, f64),
}

/// The control button for `target`, mapping a controller or a channel onto `range`
/// (ADR-0178, ADR-0183): one menu to learn a MIDI control, make a channel, follow one
/// or remove the control. Once the setting follows a channel, the button shows its name.
#[component]
pub fn Learn(
    /// The setting a learned control drives.
    target: ValueTarget,
    /// What the control's lowest and highest map onto, in the setting's unit.
    range: (f64, f64),
    /// The setting's name, which a channel made from it takes.
    #[prop(optional, into)]
    label: Option<String>,
    /// The setting's value now, so a channel made from it starts there.
    #[prop(optional, into)]
    current: Option<Signal<Option<f64>>>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let target = StoredValue::new(target);
    let label = StoredValue::new(label.unwrap_or_else(|| "Control".into()));
    let learning = RwSignal::new(false);
    let menu = RwSignal::new(None::<Menu>);
    let own_id = target.with_value(learn_id);
    // A right-click on this setting's row opens the menu where the pointer was.
    if let Some(at) = use_context::<ControlMenuAt>() {
        let own_id = own_id.clone();
        Effect::new(move |_| {
            if let Some((id, x, y)) = at.0.get()
                && id == own_id
            {
                at.0.set(None);
                learning.set(false);
                menu.set(Some(Menu::Open(x, y)));
            }
        });
    }
    let map_dialog = use_context::<crate::panels::map_control::MapControlDialog>();
    let map_control = move || {
        menu.set(None);
        if let Some(dialog) = map_dialog {
            dialog
                .0
                .set(Some(crate::panels::map_control::MapControlRequest {
                    target: target.get_value(),
                    range,
                    label: label.get_value(),
                    current: current.and_then(|c| c.get_untracked()),
                }));
        }
    };
    // The control button, which the menus give the focus back to when the keyboard
    // closes them.
    let opener = NodeRef::<leptos::html::Button>::new();
    let close = move || {
        menu.set(None);
        if let Some(button) = opener.get_untracked() {
            _ = button.focus();
        }
    };
    let menu_keys = crate::menu::keys(close);
    let bound = move || {
        studio.collection.with(|c| {
            c.as_ref().and_then(|c| {
                target.with_value(|t| c.value_bindings.iter().find(|b| &b.target == t).cloned())
            })
        })
    };
    let channel = move || {
        bound().and_then(|b| match b.input {
            MotionKind::Channel { channel_id, .. } => Some(channel_id),
            _ => None,
        })
    };
    let learn = move || {
        let Some(s) = session.0.get_value() else {
            return;
        };
        learning.set(true);
        spawn_local(async move {
            match controls(&s).await {
                // The form closed while the baseline was read: nothing to learn for.
                Some(_) if learning.try_get_untracked() != Some(true) => {}
                Some(baseline) => watch(s, studio, learning, target, range, baseline, TRIES),
                None => {
                    // `try_set` hands the value back when the form has closed.
                    if learning.try_set(false).is_some() {
                        return;
                    }
                    studio
                        .problem
                        .set(Some("This engine reads no MIDI controls.".into()));
                }
            }
        });
    };
    let follow = move |channel_id: String| {
        menu.set(None);
        if let Some(s) = session.0.get_value() {
            let binding = target.with_value(|t| follows(&channel_id, t, range));
            studio.apply(&s, vec![Edit::BindingSetValue { binding }]);
        }
    };
    // The edits that make this setting follow a new channel named `name`, and its ID;
    // none, and the channel's ID, when it follows one already.
    let ensure = move |name: &str| -> (Vec<Edit>, String) {
        if let Some(id) = untrack(channel) {
            return (Vec::new(), id);
        }
        let scene = studio.selection.with_untracked(|sel| sel.scene.clone());
        let at = current
            .and_then(|c| c.get_untracked())
            .map_or(0.0, |v| fraction(v, range));
        let mut channel = studio.collection.with_untracked(|c| {
            crate::panels::channels::new_channel(
                name,
                scene,
                at,
                c.as_ref().map_or(&[][..], |c| &c.channels[..]),
            )
        });
        // A control learned here moves over to the channel, over its whole range.
        if let Some(input) = untrack(bound).and_then(|b| carried(b.input)) {
            channel.inputs.push(input);
        }
        let binding = target.with_value(|t| follows(&channel.channel_id, t, range));
        let id = channel.channel_id.clone();
        (
            vec![
                Edit::ChannelSet { channel },
                Edit::BindingSetValue { binding },
            ],
            id,
        )
    };
    let make = move |name: String| {
        menu.set(None);
        let Some(s) = session.0.get_value() else {
            return;
        };
        let (edits, id) = ensure(&name);
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
        crate::panels::channels::open_channel(studio, id);
    };
    // The selected scene's family and its sequences, for Add to sequence.
    let sequences = move || {
        let scene = studio.selection.with(|s| s.scene.clone())?;
        studio.collection.with(|c| {
            let c = c.as_ref()?;
            let family = crate::panels::sequence::family(c, &scene);
            let list = c
                .scene(&family)
                .map(|s| s.timelines.clone())
                .unwrap_or_default();
            Some((family, list))
        })
    };
    // Adds this setting's channel (made if need be) as a lane of sequence `to`, or of a
    // new one, holding where the setting is now, and opens that sequence.
    let add_to = move |to: Option<TimelineId>| {
        menu.set(None);
        let Some(s) = session.0.get_value() else {
            return;
        };
        let Some((scene_id, list)) = untrack(sequences) else {
            studio.problem.set(Some(
                "Select a scene first: a sequence belongs to a scene.".into(),
            ));
            return;
        };
        let mut timeline = match to.and_then(|id| list.into_iter().find(|t| t.timeline_id == id)) {
            Some(t) => t,
            None => {
                let Some((id, name)) = studio
                    .collection
                    .with_untracked(|c| c.as_ref().map(crate::panels::sequence::fresh_sequence))
                else {
                    return;
                };
                crate::panels::sequence::empty_sequence(id, name)
            }
        };
        let (mut edits, id) = ensure(&label.get_value());
        let level = current
            .and_then(|c| c.get_untracked())
            .map_or(0.0, |v| fraction(v, range));
        // A sequence that already has the channel's lane needs no `timeline.set`, so no
        // empty undo step: it only opens.
        let added = crate::panels::sequence::with_lane(&mut timeline, &id, level);
        let timeline_id = timeline.timeline_id.clone();
        if added {
            edits.push(Edit::TimelineSet { scene_id, timeline });
        }
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
        crate::panels::sequence::open_sequence(studio, timeline_id);
    };
    // Binds `chord` to switch this setting's channel (made if need be) on and off.
    let set_key = move |chord: Option<Chord>| {
        menu.set(None);
        let (Some(chord), Some(s)) = (chord, session.0.get_value()) else {
            return;
        };
        let (mut edits, id) = ensure(&label.get_value());
        let toggle = Action::ChannelSwitch {
            channel_id: id,
            to: ChannelSwitch::Toggle,
        };
        studio.collection.with_untracked(|c| {
            if let Some(c) = c {
                edits.extend(crate::hotkeys::bind_edits(c, &toggle, &chord));
            }
        });
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
    };
    let remove = move || {
        menu.set(None);
        if let (Some(binding), Some(s)) = (bound(), session.0.get_value()) {
            studio.apply(
                &s,
                vec![Edit::BindingClearValue {
                    binding_id: binding.binding_id,
                }],
            );
        }
    };
    let click = move |e: leptos::ev::MouseEvent| {
        if learning.get_untracked() {
            learning.set(false);
            return;
        }
        // A click from the keyboard (Enter or Space) has no pointer, so the menu opens
        // under the button.
        let (x, y) = match opener.get_untracked() {
            Some(button) if e.detail() == 0 => {
                let rect = button.get_bounding_client_rect();
                (rect.left() + rect.width() / 2.0, rect.bottom() - 12.0)
            }
            _ => (f64::from(e.client_x()), f64::from(e.client_y())),
        };
        menu.set(Some(Menu::Open(x, y)));
    };
    let title = move || {
        match (learning.get(), bound()) {
        (true, _) => "Move a fader, knob or button on a MIDI controller. Click to stop.".into(),
        (false, Some(binding)) => match channel() {
            Some(id) => format!(
                "Follows the channel “{}”. Click for its menu.",
                studio
                    .collection
                    .with(|c| c.as_ref().map_or(id.clone(), |c| c.channel_name(&id)))
            ),
            None => long_name(&binding).replace("Click to unbind it.", "Click for its menu."),
        },
        (false, None) => {
            "Control this setting: map a control, learn a MIDI control, or make or follow a channel. A right-click on the setting opens the same menu.".to_owned()
        }
    }
    };
    let text = move || match (learning.get(), bound()) {
        (true, _) => "Move…".to_owned(),
        (false, Some(binding)) => match channel() {
            Some(id) => studio
                .collection
                .with(|c| c.as_ref().map_or(id.clone(), |c| c.channel_name(&id))),
            None => short_name(&binding),
        },
        (false, None) => String::new(),
    };
    // Below the button, kept inside the window.
    let place = |x: f64, y: f64| {
        let (w, h) = web_sys::window()
            .map(|w| {
                let n = |v: Result<
                    leptos::wasm_bindgen::JsValue,
                    leptos::wasm_bindgen::JsValue,
                >| v.ok().and_then(|v| v.as_f64());
                (
                    n(w.inner_width()).unwrap_or(1600.0),
                    n(w.inner_height()).unwrap_or(1000.0),
                )
            })
            .unwrap_or((1600.0, 1000.0));
        let left = (x - 200.0).min(w - 296.0).max(4.0);
        let top = if y + 330.0 > h {
            (y - 330.0).max(4.0)
        } else {
            y + 12.0
        };
        (format!("{left}px"), format!("{top}px"))
    };
    let open_menu = move || {
        let Menu::Open(x, y) = menu.get()? else {
            return None;
        };
        let name = label.get_value();
        let linked = channel();
        let has = bound().is_some();
        // A learned control (not a channel, which has its own Smooth) shows how long the
        // setting takes to follow it, which can be set here.
        let smoothing = (linked.is_none() && bound().as_ref().and_then(|b| smoothing_of(&b.input)).is_some()).then(|| {
            let value = Signal::derive(move || bound().and_then(|b| smoothing_of(&b.input)).map(f64::from));
            let send = move |v: f64| {
                if let (Some(mut binding), Some(s)) = (bound(), session.0.get_value()) {
                    set_smoothing(&mut binding.input, v.round().clamp(0.0, 10_000.0) as u32);
                    studio.apply(&s, vec![Edit::BindingSetValue { binding }]);
                    true
                } else {
                    false
                }
            };
            // The field's own arrow keys step it, rather than moving through the menu.
            let own_keys = |e: leptos::ev::KeyboardEvent| {
                if matches!(e.key().as_str(), "ArrowUp" | "ArrowDown" | "Home" | "End") {
                    e.stop_propagation();
                }
            };
            view! {
                <div class="control-menu-row" on:keydown=own_keys title="How long the setting takes to follow the control: longer is smoother, shorter is quicker">
                    <span>"Smoothing"</span>
                    <crate::panels::number::NumberField named=Signal::derive(|| "Smoothing".to_owned()) unit="ms" value=value step=10.0 decimals=0 min=0.0 max=10000.0 send=send />
                </div>
            }
        });
        let node = NodeRef::<leptos::html::Div>::new();
        crate::menu::focus_first(node);
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| menu.set(None)></div>
            <div class="context-menu control-menu" role="menu" aria-label="Control" node_ref=node on:keydown=menu_keys
                style:left=place(x, y).0 style:top=place(x, y).1>
                <div class="ctx-head">{format!("Control “{name}”")}</div>
                <button type="button" class="menuitem" role="menuitem" on:click=move |_| map_control()>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Map control…"</span>
                    <span class="hint">"choose a source"</span>
                </button>
                {smoothing}
                <button type="button" class="menuitem" role="menuitem" on:click=move |_| { menu.set(None); learn(); }>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Learn MIDI control"</span>
                    <span class="hint">"move a knob"</span>
                </button>
                {match linked.clone() {
                    Some(id) => view! {
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| { menu.set(None); crate::panels::channels::open_channel(studio, id.clone()); }>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">"Open channel"</span>
                        </button>
                    }.into_any(),
                    None => {
                        let named = name.clone();
                        view! {
                            <button type="button" class="menuitem" role="menuitem" on:click=move |_| make(named.clone())>
                                <span class="ctx-check"></span>
                                <span class="ctx-label">"Make channel"</span>
                                <span class="hint">{name.clone()}</span>
                            </button>
                        }.into_any()
                    }
                }}
                <button type="button" class="menuitem" role="menuitem" on:click=move |_| menu.set(Some(Menu::Follow(x, y)))>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Follow channel…"</span>
                </button>
                <button type="button" class="menuitem" role="menuitem" on:click=move |_| menu.set(Some(Menu::Sequence(x, y)))>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Add to sequence"</span>
                    <span class="hint">"›"</span>
                </button>
                <button type="button" class="menuitem" role="menuitem" on:click=move |_| menu.set(Some(Menu::Key(x, y)))>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Set key…"</span>
                    <span class="hint">"switches it"</span>
                </button>
                {has.then(|| view! {
                    <div class="menu-sep"></div>
                    <button type="button" class="menuitem danger" role="menuitem" on:click=move |_| remove()>
                        <span class="ctx-check"></span>
                        <span class="ctx-label">"Remove control"</span>
                    </button>
                })}
            </div>
        })
    };
    let follow_menu = move || {
        let Menu::Follow(x, y) = menu.get()? else {
            return None;
        };
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| menu.set(None)></div>
            <div class="context-menu control-menu follow" style:left=place(x, y).0 style:top=place(x, y).1
                on:keydown=move |e: leptos::ev::KeyboardEvent| {
                    if e.key() == "Escape" {
                        e.prevent_default();
                        e.stop_propagation();
                        close();
                    }
                }>
                <crate::panels::channels::ChannelSearch placeholder="Follow a channel" pick=follow make=make />
            </div>
        })
    };
    let sequence_menu = move || {
        let Menu::Sequence(x, y) = menu.get()? else {
            return None;
        };
        let mut list = sequences().map(|(_, l)| l).unwrap_or_default();
        // The open sequence first.
        if let Some(open) = studio.sequence.get()
            && let Some(n) = list.iter().position(|t| t.timeline_id == open)
        {
            let t = list.remove(n);
            list.insert(0, t);
        }
        let node = NodeRef::<leptos::html::Div>::new();
        crate::menu::focus_first(node);
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| menu.set(None)></div>
            <div class="context-menu control-menu" role="menu" aria-label="Add to sequence" node_ref=node on:keydown=menu_keys
                style:left=place(x, y).0 style:top=place(x, y).1>
                <div class="ctx-head">"Add a lane to"</div>
                {list.into_iter().map(|t| {
                    let name = crate::panels::sequence::timeline_name(&t);
                    let id = t.timeline_id.clone();
                    view! {
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| add_to(Some(id.clone()))>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">{name}</span>
                        </button>
                    }
                }).collect_view()}
                <div class="menu-sep"></div>
                <button type="button" class="menuitem" role="menuitem" on:click=move |_| add_to(None)>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"New sequence"</span>
                </button>
            </div>
        })
    };
    let key_menu = move || {
        let Menu::Key(x, y) = menu.get()? else {
            return None;
        };
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| menu.set(None)></div>
            <div class="context-menu control-menu" role="menu" style:left=place(x, y).0 style:top=place(x, y).1>
                <div class="ctx-head">"Set key"</div>
                <KeyCapture done=set_key />
                <p class="hint control-menu-note">"The key switches the channel on and off, with its fades."</p>
            </div>
        })
    };
    view! {
        <button
            type="button"
            class="learn"
            node_ref=opener
            class:learning=learning
            class:bound=move || bound().is_some()
            class:chip=move || channel().is_some()
            data-learn=own_id
            title=title
            aria-label=title
            aria-haspopup="menu"
            on:click=click
        >
            {move || channel().is_none().then(|| view! { <Icon name="midi" size=13 /> })}
            {text}
        </button>
        {open_menu}
        {follow_menu}
        {sequence_menu}
        {key_menu}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{BusId, SourceId};

    fn control(input: &str, controller: u8, count: u64) -> MidiControlInfo {
        MidiControlInfo {
            input: input.into(),
            channel: 1,
            controller,
            value: 64,
            count,
        }
    }

    #[test]
    fn a_right_click_in_free_text_keeps_the_browsers_menu() {
        assert!(keeps_own_menu("TEXTAREA", ""));
        assert!(keeps_own_menu("INPUT", "text"));
        assert!(keeps_own_menu("input", ""));
        assert!(!keeps_own_menu("INPUT", "number"));
        assert!(!keeps_own_menu("INPUT", "range"));
        assert!(!keeps_own_menu("INPUT", "checkbox"));
        assert!(!keeps_own_menu("LABEL", ""));
    }

    #[test]
    fn the_moved_control_is_the_first_whose_count_grew() {
        let baseline = [control("Bitwig", 7, 3), control("Pads", 1, 9)];
        assert_eq!(moved(&baseline, &baseline), None);
        let now = [control("Bitwig", 7, 3), control("Pads", 21, 10)];
        assert_eq!(moved(&baseline, &now).map(|c| c.controller), Some(21));
        let new_input = [control("Bitwig", 7, 3), control("Knobs", 74, 1)];
        assert_eq!(
            moved(&baseline, &new_input).map(|c| c.input),
            Some("Knobs".into())
        );
    }

    #[test]
    fn a_learned_binding_reads_that_control_over_the_range() {
        let target = ValueTarget::BusGain {
            bus_id: BusId::master(),
        };
        let binding = learned(&control("Bitwig", 7, 1), &target, FADER_THROW);
        assert_eq!(binding.problem(), None);
        assert_eq!(binding.binding_id, learn_id(&target));
        assert_eq!(short_name(&binding), "CC 7");
        assert!(long_name(&binding).contains("channel 1 from Bitwig"));
        let other = ValueTarget::SourceGain {
            source_id: SourceId::new("mic"),
        };
        assert_ne!(learn_id(&other), learn_id(&target));
    }

    #[test]
    fn following_a_channel_maps_it_onto_the_range_from_where_the_setting_is() {
        let target = ValueTarget::SourceGain {
            source_id: SourceId::new("mic"),
        };
        let binding = follows("key_light", &target, (0.0, 2.5));
        assert_eq!(binding.problem(), None);
        assert_eq!(binding.binding_id, learn_id(&target));
        assert!(
            matches!(binding.input, MotionKind::Channel { ref channel_id, .. } if channel_id == "key_light")
        );
        assert_eq!(fraction(1.25, (0.0, 2.5)), 0.5);
        assert_eq!(fraction(9.0, (0.0, 2.5)), 1.0);
        assert_eq!(fraction(1.0, (1.0, 1.0)), 0.0);
    }

    #[test]
    fn only_a_live_number_with_ends_or_a_switch_takes_a_controller() {
        let param = |ty: ParamType, live: bool| ParamDescriptor {
            warning: None,
            id: "p".into(),
            ty,
            default: serde_json::Value::Null,
            label: "P".into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live,
        };
        let float = ParamType::Float {
            min: Some(0.0),
            max: Some(2.0),
            step: None,
            unit: None,
        };
        assert_eq!(param_range(&param(float.clone(), true)), Some((0.0, 2.0)));
        assert_eq!(param_range(&param(float, false)), None);
        assert_eq!(param_range(&param(ParamType::Bool, true)), Some((0.0, 1.0)));
        let open = ParamType::Int {
            min: Some(0),
            max: None,
            step: None,
            unit: None,
        };
        assert_eq!(param_range(&param(open, true)), None);
    }

    /// Make channel carries a learned CC over to the new channel, over 0 to 1.
    #[test]
    fn a_learned_control_moves_to_the_channel_over_its_whole_range() {
        let learned = MotionKind::Midi {
            control: MotionMidiControl::Cc,
            controller: Some(7),
            channel: Some(1),
            input: Some("Pad".into()),
            smoothing_ms: 50,
            curve: MotionCurve::Linear,
            from: Finite32::new(-60.0).unwrap_or(Finite32::ZERO),
            to: Finite32::new(6.0).unwrap_or(Finite32::ZERO),
        };
        let Some(MotionKind::Midi {
            controller,
            from,
            to,
            ..
        }) = carried(learned)
        else {
            panic!("a MIDI control is carried");
        };
        assert_eq!(controller, Some(7));
        assert_eq!((from, to), (Finite32::ZERO, Finite32::ONE));
        assert_eq!(
            carried(MotionKind::Channel {
                channel_id: "c".into(),
                smoothing_ms: 0,
                curve: MotionCurve::Linear,
                from: Finite32::ZERO,
                to: Finite32::ONE,
            }),
            None
        );
    }
}
