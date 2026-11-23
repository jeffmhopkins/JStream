//! Which live data a `data` reader reads (ADR-0171), for the channel card's data input
//! and the camera motion dialog: the sender, then one of a JTunnel sender's 32
//! modulation params (Mod 1 to Mod 32, key `params`, field `values.<n - 1>`, ADR-0181)
//! or any other key and field. The engine names a sender's Mod after what reads it, so
//! the DAW shows "Camera Fov" on the Mod a camera's field of view reads.

use super::motion::{with_data_path, with_input};
use jstream_control::protocol::{Finite32, MotionCurve, MotionKind};
use leptos::prelude::*;

/// How many modulation params a JTunnel sender streams.
pub const MOD_SLOTS: u8 = 32;

/// The key a JTunnel sender streams its modulation params under.
const PARAMS_KEY: &str = "params";

/// The Mod, 1 to [`MOD_SLOTS`], that a data `kind` reads, or `None` when it reads
/// anything else: key `params`, field `values.<n>` with `n` in digits, as the engine
/// counts a slot.
pub fn mod_slot(kind: &MotionKind) -> Option<u8> {
    let MotionKind::Data { key, field, .. } = kind else {
        return None;
    };
    if key != PARAMS_KEY {
        return None;
    }
    let n = field.strip_prefix("values.")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u8 = n.parse().ok()?;
    (n < MOD_SLOTS).then_some(n + 1)
}

/// A data `kind` reading Mod `n` (1 to [`MOD_SLOTS`]) instead, or `None` when it isn't
/// data, `n` is out of range, or it reads that already.
pub fn with_mod(kind: &MotionKind, n: u8) -> Option<MotionKind> {
    if !(1..=MOD_SLOTS).contains(&n) {
        return None;
    }
    with_data_path(kind, PARAMS_KEY, &format!("values.{}", n - 1))
}

/// A data reader of Mod `n` on any sender, smoothed over 50 ms and mapped from its 0 to 1
/// onto `from..to`.
pub fn mod_reader(n: u8, from: f32, to: f32) -> MotionKind {
    let finite = |v: f32| Finite32::new(v).unwrap_or(Finite32::ZERO);
    MotionKind::Data {
        input: None,
        key: PARAMS_KEY.into(),
        field: format!("values.{}", n.clamp(1, MOD_SLOTS) - 1),
        range: [Finite32::ZERO, Finite32::ONE],
        states: Default::default(),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: finite(from),
        to: finite(to),
        color_from: None,
        palette: Default::default(),
    }
}

/// What a data `kind` reads, in words: "Mod 20", "Mod 20 · Bitwig", or
/// "Data transport · playing".
pub fn data_name(kind: &MotionKind) -> String {
    let MotionKind::Data {
        input, key, field, ..
    } = kind
    else {
        return String::new();
    };
    let what = match mod_slot(kind) {
        Some(n) => format!("Mod {n}"),
        None => format!("Data {key} · {field}"),
    };
    match input {
        Some(sender) => format!("{what} · {sender}"),
        None => what,
    }
}

/// The Reads select's value for Other data.
const OTHER: &str = "other";

/// The fields that say what a data reader reads: Sender, Reads (Mod 1 to Mod 32, or
/// Other data), and for other data its Key and Field. Each change goes to `send` as the
/// whole new kind.
#[component]
pub fn DataReads(
    /// The reader, `None` while there is none.
    #[prop(into)]
    kind: Signal<Option<MotionKind>>,
    /// Gets each changed reader.
    send: impl Fn(MotionKind) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    // Other data stays picked while its key and field are being typed, even when they
    // happen to name a Mod.
    let other = RwSignal::new(kind.with_untracked(|k| k.as_ref().is_some_and(|k| mod_slot(k).is_none())));
    let edit = move |change: &dyn Fn(&MotionKind) -> Option<MotionKind>| {
        if let Some(next) = kind.with_untracked(|k| k.as_ref().and_then(change)) {
            send(next);
        }
    };
    let text = move |get: fn(&MotionKind) -> Option<String>| {
        move || kind.with(|k| k.as_ref().and_then(get)).unwrap_or_default()
    };
    let reads = move || {
        if other.get() {
            return OTHER.to_owned();
        }
        kind.with(|k| k.as_ref().and_then(mod_slot))
            .map_or_else(|| OTHER.to_owned(), |n| n.to_string())
    };
    view! {
        <label class="select-field">
            <span class="field-label">"Sender"</span>
            <input
                type="text"
                aria-label="Which data sender"
                placeholder="Any sender"
                title="A JTunnel sender by name; blank for any"
                maxlength="64"
                prop:value=text(|k| match k {
                    MotionKind::Data { input, .. } => input.clone(),
                    _ => None,
                })
                on:change=move |e| {
                    let name = event_target_value(&e);
                    edit(&|k| with_input(k, &name));
                }
            />
        </label>
        <label class="select-field">
            <span class="field-label">"Reads"</span>
            <select
                aria-label="Which data it reads"
                title="One of a JTunnel sender's 32 Mod params, or any other key and field of its data"
                prop:value=reads
                on:change=move |e| {
                    let value = event_target_value(&e);
                    match value.parse::<u8>() {
                        Ok(n) => {
                            other.set(false);
                            edit(&|k| with_mod(k, n));
                        }
                        Err(_) => other.set(true),
                    }
                }
            >
                {(1..=MOD_SLOTS)
                    .map(|n| view! { <option value=n.to_string()>{format!("Mod {n}")}</option> })
                    .collect_view()}
                <option value=OTHER>"Other data"</option>
            </select>
        </label>
        <Show when=move || other.get()>
            <label class="select-field">
                <span class="field-label">"Key"</span>
                <input
                    type="text"
                    aria-label="Which data message"
                    placeholder="transport"
                    title="The message's key, as the sender names it: transport, track/1"
                    maxlength="63"
                    prop:value=text(|k| match k {
                        MotionKind::Data { key, .. } => Some(key.clone()),
                        _ => None,
                    })
                    on:change=move |e| {
                        let next = event_target_value(&e);
                        edit(&|k| match k {
                            MotionKind::Data { field, .. } => with_data_path(k, &next, field),
                            _ => None,
                        });
                    }
                />
            </label>
            <label class="select-field">
                <span class="field-label">"Field"</span>
                <input
                    type="text"
                    aria-label="Which field of the message"
                    placeholder="The whole message"
                    title="A dotted path into the message: playing, slots.0.state; blank for the whole message"
                    maxlength="255"
                    prop:value=text(|k| match k {
                        MotionKind::Data { field, .. } => Some(field.clone()),
                        _ => None,
                    })
                    on:change=move |e| {
                        let next = event_target_value(&e);
                        edit(&|k| match k {
                            MotionKind::Data { key, .. } => with_data_path(k, key, &next),
                            _ => None,
                        });
                    }
                />
            </label>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mod_is_a_params_values_field_counted_from_one() {
        let reader = mod_reader(20, 0.0, 1.0);
        assert_eq!(mod_slot(&reader), Some(20));
        assert_eq!(data_name(&reader), "Mod 20");
        let MotionKind::Data { field, .. } = &reader else {
            panic!("a data reader");
        };
        assert_eq!(field, "values.19");
        let eleven = with_mod(&reader, 11).expect("another Mod");
        assert_eq!(mod_slot(&eleven), Some(11));
        assert!(with_mod(&eleven, 11).is_none());
        assert!(with_mod(&eleven, 0).is_none());
        assert!(with_mod(&eleven, 33).is_none());
        let from_bitwig = with_input(&eleven, "Bitwig").expect("a sender");
        assert_eq!(data_name(&from_bitwig), "Mod 11 · Bitwig");
    }

    #[test]
    fn other_data_is_not_a_mod() {
        let reader = mod_reader(1, 0.0, 1.0);
        for (key, field) in [
            ("transport", "playing"),
            ("params", "values.32"),
            ("params", "values.+4"),
            ("params", "names.0"),
        ] {
            let other = with_data_path(&reader, key, field).expect("another path");
            assert_eq!(mod_slot(&other), None, "{key} {field}");
        }
        let transport = with_data_path(&reader, "transport", "playing").expect("a path");
        assert_eq!(data_name(&transport), "Data transport · playing");
    }
}
