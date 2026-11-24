//! Which audio level an `audio` reader hears (ADR-0206), for the channel card's audio
//! input: the program mix, a bus or a source with audio, what it follows (level, peak,
//! beat or a band) and its floor, ceiling, attack and release.

use super::motion::{
    MEASURES, Param, hears_value, param_value, with_hears, with_measure, with_param,
};
use super::number::NumberField;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{Finite32, MotionCurve, MotionKind, MotionMeasure};
use leptos::prelude::*;

/// A reader of the program mix's level, from -60 to 0 dBFS with a 10 ms attack and a
/// 250 ms release, over the channel's whole 0 to 1.
pub fn audio_reader() -> MotionKind {
    MotionKind::Audio {
        audio: None,
        audio_source: None,
        measure: MotionMeasure::Level,
        low_hz: None,
        high_hz: None,
        floor_db: Finite32::new(-60.0).unwrap_or(Finite32::ZERO),
        ceiling_db: Finite32::ZERO,
        attack_ms: 10,
        release_ms: 250,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }
}

/// What an audio `kind` hears, in words: "Program mix", a bus's or a source's name.
pub fn heard_name(collection: &Collection, kind: &MotionKind) -> String {
    match kind {
        MotionKind::Audio {
            audio_source: Some(source),
            ..
        } => collection.source_name(source),
        MotionKind::Audio {
            audio: Some(bus), ..
        } => collection
            .buses
            .iter()
            .find(|b| &b.bus_id == bus)
            .map_or_else(|| bus.to_string(), |b| b.name.clone()),
        _ => "Program mix".into(),
    }
}

/// An audio `kind` as an input row says it: "Audio · Program mix · Level".
pub fn audio_name(collection: &Collection, kind: &MotionKind) -> String {
    let MotionKind::Audio { measure, .. } = kind else {
        return String::new();
    };
    let follows = MEASURES
        .iter()
        .find(|(m, _)| m == measure)
        .map_or("Level", |(_, name)| *name);
    format!("Audio · {} · {follows}", heard_name(collection, kind))
}

/// The number fields an audio reader shows here: its band's edges for a band, then the
/// floor, ceiling, attack and release. Its range is the input's own `from` and `to`.
pub fn audio_fields(kind: &MotionKind) -> &'static [Param] {
    match kind {
        MotionKind::Audio {
            measure: MotionMeasure::Band,
            ..
        } => &[
            Param::LowHz,
            Param::HighHz,
            Param::Floor,
            Param::Ceiling,
            Param::Attack,
            Param::Release,
        ],
        MotionKind::Audio {
            measure: MotionMeasure::Onset,
            ..
        } => &[Param::Attack, Param::Release],
        MotionKind::Audio { .. } => &[Param::Floor, Param::Ceiling, Param::Attack, Param::Release],
        _ => &[],
    }
}

/// The fields that say what an audio reader hears: Hears, Follows, then
/// [`audio_fields`]. Each change goes to `send` as the whole new kind.
#[component]
pub fn AudioReads(
    /// The reader, `None` while there is none.
    #[prop(into)]
    kind: Signal<Option<MotionKind>>,
    /// Gets each changed reader.
    send: impl Fn(MotionKind) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let edit = move |change: &dyn Fn(&MotionKind) -> Option<MotionKind>| {
        if let Some(next) = kind.with_untracked(|k| k.as_ref().and_then(change)) {
            send(next);
        }
        true
    };
    // The program mix, each bus, then each source with audio.
    let options = move || {
        studio.collection.with(|c| {
            let Some(c) = c.as_ref() else {
                return Vec::new();
            };
            let buses = c
                .buses
                .iter()
                .map(|b| (b.bus_id.as_str().to_owned(), b.name.clone()));
            let sources = c
                .sources
                .iter()
                .filter(|s| s.audio.is_some())
                .map(|s| (format!("source:{}", s.source_id.as_str()), s.name.clone()));
            std::iter::once((String::new(), "Program mix".to_owned()))
                .chain(buses)
                .chain(sources)
                .collect::<Vec<_>>()
        })
    };
    let fields = move || {
        let shown = kind.with(|k| k.as_ref().map(audio_fields)).unwrap_or(&[]);
        shown
            .iter()
            .map(|param| {
                let param = *param;
                let (label, step, decimals, min, max) = param.field();
                let unit = match param {
                    Param::Floor | Param::Ceiling => "dB",
                    Param::LowHz | Param::HighHz => "Hz",
                    _ => "ms",
                };
                let value = Signal::derive(move || {
                    kind.with(|k| k.as_ref().and_then(|k| param_value(k, param)))
                });
                let send = move |v: f64| edit(&|k| with_param(k, param, v).filter(|n| n != k));
                view! { <NumberField label unit value step decimals min max send /> }
            })
            .collect_view()
    };
    view! {
        <label class="select-field">
            <span class="field-label">"Hears"</span>
            <select
                aria-label="What it hears"
                title="The program mix as the outputs hear it, one bus after its fader, or one source"
                prop:value=move || kind.with(|k| k.as_ref().map(hears_value)).unwrap_or_default()
                on:change=move |e| {
                    let value = event_target_value(&e);
                    edit(&|k| with_hears(k, &value));
                }
            >
                {move || options()
                    .into_iter()
                    .map(|(value, name)| view! { <option value=value>{name}</option> })
                    .collect_view()}
            </select>
        </label>
        <label class="select-field">
            <span class="field-label">"Follows"</span>
            <select
                aria-label="What it follows"
                title="Its loudness, its peaks, each beat, or one band of frequencies"
                prop:value=move || kind.with(|k| match k {
                    Some(MotionKind::Audio { measure, .. }) => MEASURES
                        .iter()
                        .find(|(m, _)| m == measure)
                        .map(|(_, name)| *name),
                    _ => None,
                }).unwrap_or("")
                on:change=move |e| {
                    let name = event_target_value(&e);
                    if let Some(next) = MEASURES.iter().find(|(_, n)| *n == name).map(|(m, _)| *m) {
                        edit(&|k| with_measure(k, next));
                    }
                }
            >
                {MEASURES
                    .iter()
                    .map(|(_, name)| view! { <option value=*name>{*name}</option> })
                    .collect_view()}
            </select>
        </label>
        {fields}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_audio_input_hears_the_program_until_a_bus_or_source_is_picked() {
        let collection = Collection::default();
        let program = audio_reader();
        assert_eq!(
            audio_name(&collection, &program),
            "Audio · Program mix · Level"
        );
        assert_eq!(audio_fields(&program).len(), 4);
        let music = with_hears(&program, "music").expect("a bus");
        assert_eq!(heard_name(&collection, &music), "music");
        let beat = with_measure(&music, MotionMeasure::Onset).expect("a beat");
        assert_eq!(audio_name(&collection, &beat), "Audio · music · Beat");
        assert_eq!(audio_fields(&beat), [Param::Attack, Param::Release]);
        let band = with_measure(&beat, MotionMeasure::Band).expect("a band");
        assert_eq!(audio_fields(&band).len(), 6);
        let slower = with_param(&program, Param::Release, 600.0).expect("a release");
        assert_eq!(param_value(&slower, Param::Release), Some(600.0));
    }
}
