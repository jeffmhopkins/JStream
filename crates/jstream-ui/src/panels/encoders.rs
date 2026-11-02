//! The Outputs panel's encoder profiles and video mixes: a row for each, with what it
//! encodes or shows, and a form that creates or edits one (`encoder.create`,
//! `encoder.update`, `video_mix.create`, `video_mix.update`). Delete sends
//! `encoder.delete` or `video_mix.delete`, and is disabled while an output uses the
//! profile or shows the mix, naming those outputs, as the engine refuses it then. An
//! output's card picks its profile and mix ([`outputs`](super::outputs)).
//!
//! A profile has the video codec (H.264, HEVC or AV1), constant or variable bit rate
//! with its peak, the keyframe interval, the hardware preference and the AAC bit rate. A
//! mix has what it shows (the program, the preview or one scene), its size, with
//! presets that include vertical 1080 × 1920 and 720 × 1280, and its rate as a divisor
//! of the base rate. Both are checked with the protocol's own `problem` before they are
//! sent, so the form names the field the engine would refuse.
//!
//! Docs: crates/jstream-ui/README.md#design

use jstream_control::protocol::{
    AudioCodec, AudioEncoding, Edit, EncoderProfile, EncoderProfileId, EncoderProfileInfo,
    HardwarePreference, MixInput, MixResolution, OutputId, OutputInfo, RateControl, SceneId,
    VideoCodec, VideoEncoding, VideoMix, VideoMixId, VideoMixInfo,
};
use leptos::prelude::*;

/// What the encoder form edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderDraft {
    /// The display name, as typed.
    pub name: String,
    /// The video codec.
    pub codec: VideoCodec,
    /// Variable bit rate, with a peak, rather than constant.
    pub vbr: bool,
    /// The video bit rate, kbit/s: the constant rate, or the average.
    pub kbps: u32,
    /// The variable bit rate's peak, kbit/s.
    pub max_kbps: u32,
    /// Seconds between keyframes.
    pub keyframe_s: u32,
    /// Whether a hardware encoder is used.
    pub hardware: HardwarePreference,
    /// The AAC bit rate, kbit/s.
    pub audio_kbps: u32,
}

impl EncoderDraft {
    /// A new profile: the engine's default encoding.
    pub fn new() -> EncoderDraft {
        EncoderDraft::of_profile("New encoder", &EncoderProfile::DEFAULT)
    }

    /// The profile `info` as it is.
    pub fn of(info: &EncoderProfileInfo) -> EncoderDraft {
        EncoderDraft::of_profile(&info.name, &info.profile)
    }

    fn of_profile(name: &str, profile: &EncoderProfile) -> EncoderDraft {
        let video = &profile.video;
        let (vbr, kbps, max_kbps) = match video.rate_control {
            RateControl::Vbr { kbps, max_kbps } => (true, kbps, max_kbps),
            RateControl::Cbr { kbps } => (false, kbps, kbps.saturating_mul(3) / 2),
            // A rate control this build doesn't know: the form starts from the default.
            _ => (false, 2500, 3750),
        };
        EncoderDraft {
            name: name.to_owned(),
            codec: video.codec,
            vbr,
            kbps,
            max_kbps,
            keyframe_s: video.keyframe_interval_s,
            hardware: video.hardware,
            audio_kbps: profile.audio.kbps,
        }
    }

    /// The encoding the form describes.
    pub fn profile(&self) -> EncoderProfile {
        let rate_control = if self.vbr {
            RateControl::Vbr {
                kbps: self.kbps,
                max_kbps: self.max_kbps,
            }
        } else {
            RateControl::Cbr { kbps: self.kbps }
        };
        EncoderProfile {
            video: VideoEncoding {
                codec: self.codec,
                rate_control,
                keyframe_interval_s: self.keyframe_s,
                hardware: self.hardware,
            },
            audio: AudioEncoding {
                codec: AudioCodec::Aac,
                kbps: self.audio_kbps,
            },
        }
    }

    /// The edit that saves the form: `encoder.update` of `editing`, or `encoder.create`
    /// with an ID none of `existing` has. Refused, naming the field, for a blank name or
    /// an encoding the engine would refuse.
    pub fn edit(
        &self,
        editing: Option<&EncoderProfileId>,
        existing: &[EncoderProfileInfo],
    ) -> Result<Edit, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the encoder a name.".into());
        }
        let profile = self.profile();
        if let Some((field, reason)) = profile.problem() {
            return Err(format!("{}: {reason}.", encoder_field(field)));
        }
        Ok(match editing {
            Some(profile_id) => Edit::EncoderUpdate {
                profile_id: profile_id.clone(),
                name: name.to_owned(),
                profile,
            },
            None => Edit::EncoderCreate {
                profile_id: EncoderProfileId::new(fresh_id(
                    "enc",
                    existing.iter().map(|e| e.profile_id.as_str()),
                )),
                name: name.to_owned(),
                profile,
            },
        })
    }
}

impl Default for EncoderDraft {
    fn default() -> EncoderDraft {
        EncoderDraft::new()
    }
}

/// The form's label for a field `EncoderProfile::problem` names.
fn encoder_field(field: &str) -> &'static str {
    match field {
        "video.codec" => "Video codec",
        "video.rate_control.max_kbps" => "Peak bit rate",
        "video.rate_control.kbps" | "video.rate_control.type" => "Bit rate",
        "video.keyframe_interval_s" => "Keyframe every",
        "video.hardware" => "Hardware encoder",
        _ => "Audio bit rate",
    }
}

/// `prefix_N` for the lowest `N` from 1 that none of `taken` is.
fn fresh_id<'a>(prefix: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    (1..)
        .map(|n| format!("{prefix}_{n}"))
        .find(|id| !taken.clone().any(|t| t == id))
        .unwrap_or_else(|| prefix.to_owned())
}

/// A codec's name as the form shows it.
pub fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::Av1 => "AV1",
        _ => "Unknown codec",
    }
}

/// One line on what a profile encodes, for its row: "H.264 · CBR 6000 kbps · keyframe
/// every 2 s · hardware first · AAC 160 kbps".
pub fn encoder_summary(profile: &EncoderProfile) -> String {
    let video = &profile.video;
    let rate = match video.rate_control {
        RateControl::Cbr { kbps } => format!("CBR {kbps} kbps"),
        RateControl::Vbr { kbps, max_kbps } => format!("VBR {kbps} kbps, peak {max_kbps}"),
        _ => "unknown rate control".to_owned(),
    };
    let hardware = match video.hardware {
        HardwarePreference::Prefer => "hardware first",
        HardwarePreference::Require => "hardware only",
        HardwarePreference::Avoid => "software",
        _ => "unknown encoder",
    };
    format!(
        "{} · {rate} · keyframe every {} s · {hardware} · AAC {} kbps",
        codec_label(video.codec),
        video.keyframe_interval_s,
        profile.audio.kbps
    )
}

/// The sizes the mix form offers, with their labels.
pub const MIX_SIZES: &[(&str, u32, u32)] = &[
    ("1920 × 1080", 1920, 1080),
    ("1280 × 720", 1280, 720),
    ("3840 × 2160", 3840, 2160),
    ("1080 × 1920 (vertical)", 1080, 1920),
    ("720 × 1280 (vertical)", 720, 1280),
    ("1080 × 1080 (square)", 1080, 1080),
];

/// What the video mix form edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixDraft {
    /// The display name, as typed.
    pub name: String,
    /// What it shows.
    pub input: MixInput,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// It draws every `fps_divisor`th frame of the base rate.
    pub fps_divisor: u32,
}

impl MixDraft {
    /// A new mix: the program, vertical 1080 × 1920, at the base rate.
    pub fn new() -> MixDraft {
        MixDraft {
            name: "Vertical".to_owned(),
            input: MixInput::Program,
            width: 1080,
            height: 1920,
            fps_divisor: 1,
        }
    }

    /// The mix `info` as it is.
    pub fn of(info: &VideoMixInfo) -> MixDraft {
        MixDraft {
            name: info.name.clone(),
            input: info.mix.input.clone(),
            width: info.mix.resolution.width,
            height: info.mix.resolution.height,
            fps_divisor: info.mix.fps_divisor,
        }
    }

    /// The mix the form describes.
    pub fn mix(&self) -> VideoMix {
        VideoMix {
            input: self.input.clone(),
            resolution: MixResolution {
                width: self.width,
                height: self.height,
            },
            fps_divisor: self.fps_divisor,
        }
    }

    /// The edit that saves the form: `video_mix.update` of `editing`, or
    /// `video_mix.create` with an ID none of `existing` has. Refused, naming the field,
    /// for a blank name or a mix the engine would refuse.
    pub fn edit(
        &self,
        editing: Option<&VideoMixId>,
        existing: &[VideoMixInfo],
    ) -> Result<Edit, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the video mix a name.".into());
        }
        let mix = self.mix();
        if let Some((field, reason)) = mix.problem() {
            let label = match field {
                "resolution.width" => "Width",
                "resolution.height" => "Height",
                "fps_divisor" => "Rate",
                _ => "Shows",
            };
            return Err(format!("{label}: {reason}."));
        }
        Ok(match editing {
            Some(mix_id) => Edit::VideoMixUpdate {
                mix_id: mix_id.clone(),
                name: name.to_owned(),
                mix,
            },
            None => Edit::VideoMixCreate {
                mix_id: VideoMixId::new(fresh_id(
                    "mix",
                    existing.iter().map(|m| m.mix_id.as_str()),
                )),
                name: name.to_owned(),
                mix,
            },
        })
    }
}

impl Default for MixDraft {
    fn default() -> MixDraft {
        MixDraft::new()
    }
}

/// What a mix shows, for its row and the form's select: "Program", "Preview" or the
/// scene's name (its ID when the collection has no such scene).
pub fn input_label(input: &MixInput, scenes: &[(SceneId, String)]) -> String {
    match input {
        MixInput::Program => "Program".to_owned(),
        MixInput::Preview => "Preview".to_owned(),
        MixInput::Scene { scene_id } => scenes
            .iter()
            .find(|(id, _)| id == scene_id)
            .map_or_else(|| scene_id.to_string(), |(_, name)| name.clone()),
        _ => "Unknown input".to_owned(),
    }
}

/// One line on what a mix shows, for its row: "Program · 1080 × 1920 · every frame".
pub fn mix_summary(mix: &VideoMix, scenes: &[(SceneId, String)]) -> String {
    let rate = match mix.fps_divisor {
        1 => "every frame".to_owned(),
        2 => "every 2nd frame".to_owned(),
        3 => "every 3rd frame".to_owned(),
        n => format!("every {n}th frame"),
    };
    format!(
        "{} · {} × {} · {rate}",
        input_label(&mix.input, scenes),
        mix.resolution.width,
        mix.resolution.height
    )
}

/// The outputs that use profile `profile_id`, by ID.
pub fn using_encoder(outputs: &[OutputInfo], profile_id: &EncoderProfileId) -> Vec<OutputId> {
    outputs
        .iter()
        .filter(|o| o.encoder_profile.as_ref() == Some(profile_id))
        .map(|o| o.output_id.clone())
        .collect()
}

/// The outputs that show mix `mix_id`, by ID.
pub fn showing_mix(outputs: &[OutputInfo], mix_id: &VideoMixId) -> Vec<OutputId> {
    outputs
        .iter()
        .filter(|o| o.video_mix.as_ref() == Some(mix_id))
        .map(|o| o.output_id.clone())
        .collect()
}

/// Why a profile or mix can't be deleted: the outputs that use it, or `None`.
fn in_use(users: &[OutputId], verb: &str) -> Option<String> {
    (!users.is_empty()).then(|| {
        let names: Vec<String> = users.iter().map(ToString::to_string).collect();
        format!("{verb} {}; pick another for it first", names.join(", "))
    })
}

/// The `select` value of an input: `program`, `preview` or `scene:<id>`.
fn input_value(input: &MixInput) -> String {
    match input {
        MixInput::Program => "program".to_owned(),
        MixInput::Preview => "preview".to_owned(),
        MixInput::Scene { scene_id } => format!("scene:{scene_id}"),
        _ => String::new(),
    }
}

/// The input a `select` value names.
fn input_of(value: &str) -> MixInput {
    match value {
        "preview" => MixInput::Preview,
        other => other
            .strip_prefix("scene:")
            .map_or(MixInput::Program, |id| MixInput::Scene {
                scene_id: SceneId::new(id),
            }),
    }
}

/// A number field's value, or `fallback` when it isn't one.
fn number(event: &leptos::ev::Event, fallback: u32) -> u32 {
    event_target_value(event).trim().parse().unwrap_or(fallback)
}

/// The encoder profiles and video mixes, under the output cards. `send` sends one edit
/// and reports its failure in the panel.
#[component]
pub(crate) fn Profiles(
    encoders: RwSignal<Vec<EncoderProfileInfo>>,
    mixes: RwSignal<Vec<VideoMixInfo>>,
    outputs: Signal<Vec<OutputInfo>>,
    scenes: Signal<Vec<(SceneId, String)>>,
    send: Callback<Edit>,
) -> impl IntoView {
    // Which form is open: `None` closed, `Some(None)` a new one, `Some(Some(id))` an edit.
    let encoder_form = RwSignal::new(None::<Option<EncoderProfileId>>);
    let mix_form = RwSignal::new(None::<Option<VideoMixId>>);
    view! {
        <section class="profiles" aria-label="Encoders">
            <h3>"Encoders"</h3>
            <ul class="profile-list">
                <For
                    each=move || encoders.get()
                    key=|e| (e.profile_id.clone(), e.name.clone(), encoder_summary(&e.profile))
                    let:encoder
                >
                    {
                        let id = encoder.profile_id.clone();
                        let users = {
                            let id = id.clone();
                            move || outputs.with(|o| using_encoder(o, &id))
                        };
                        let blocked = Memo::new(move |_| in_use(&users(), "Used by"));
                        let edit_id = id.clone();
                        let delete_id = id.clone();
                        view! {
                            <li class="profile">
                                <strong>{encoder.name.clone()}</strong>
                                <span class="profile-summary">{encoder_summary(&encoder.profile)}</span>
                                <button
                                    type="button"
                                    aria-label=format!("Edit {}", encoder.name)
                                    on:click=move |_| encoder_form.set(Some(Some(edit_id.clone())))
                                >
                                    "Edit…"
                                </button>
                                <button
                                    type="button"
                                    aria-label=format!("Delete {}", encoder.name)
                                    disabled=move || blocked.with(Option::is_some)
                                    title=move || blocked.get().unwrap_or_default()
                                    on:click=move |_| {
                                        send.run(Edit::EncoderDelete { profile_id: delete_id.clone() })
                                    }
                                >
                                    "Delete"
                                </button>
                            </li>
                        }
                    }
                </For>
            </ul>
            <button type="button" on:click=move |_| encoder_form.set(Some(None))>"New encoder…"</button>
            {move || {
                encoder_form
                    .get()
                    .map(|editing| {
                        view! { <EncoderForm editing=editing encoders=encoders send=send open=encoder_form /> }
                    })
            }}
        </section>
        <section class="profiles" aria-label="Video mixes">
            <h3>"Video mixes"</h3>
            <p class="hint">"An output shows the program at the canvas size unless it picks a mix."</p>
            <ul class="profile-list">
                <For
                    each=move || mixes.get()
                    key=|m| (m.mix_id.clone(), m.name.clone(), m.mix.clone())
                    let:mix
                >
                    {
                        let id = mix.mix_id.clone();
                        let users = {
                            let id = id.clone();
                            move || outputs.with(|o| showing_mix(o, &id))
                        };
                        let blocked = Memo::new(move |_| in_use(&users(), "Shown by"));
                        let edit_id = id.clone();
                        let delete_id = id.clone();
                        let shown = mix.mix.clone();
                        let summary = move || scenes.with(|s| mix_summary(&shown, s));
                        view! {
                            <li class="profile">
                                <strong>{mix.name.clone()}</strong>
                                <span class="profile-summary">{summary}</span>
                                <button
                                    type="button"
                                    aria-label=format!("Edit {}", mix.name)
                                    on:click=move |_| mix_form.set(Some(Some(edit_id.clone())))
                                >
                                    "Edit…"
                                </button>
                                <button
                                    type="button"
                                    aria-label=format!("Delete {}", mix.name)
                                    disabled=move || blocked.with(Option::is_some)
                                    title=move || blocked.get().unwrap_or_default()
                                    on:click=move |_| {
                                        send.run(Edit::VideoMixDelete { mix_id: delete_id.clone() })
                                    }
                                >
                                    "Delete"
                                </button>
                            </li>
                        }
                    }
                </For>
            </ul>
            <button type="button" on:click=move |_| mix_form.set(Some(None))>"New video mix…"</button>
            {move || {
                mix_form
                    .get()
                    .map(|editing| {
                        view! { <MixForm editing=editing mixes=mixes scenes=scenes send=send open=mix_form /> }
                    })
            }}
        </section>
    }
}

/// The form that creates a profile, or edits `editing`.
#[component]
fn EncoderForm(
    editing: Option<EncoderProfileId>,
    encoders: RwSignal<Vec<EncoderProfileInfo>>,
    send: Callback<Edit>,
    open: RwSignal<Option<Option<EncoderProfileId>>>,
) -> impl IntoView {
    let start = editing
        .as_ref()
        .and_then(|id| {
            encoders
                .with_untracked(|e| e.iter().find(|e| &e.profile_id == id).map(EncoderDraft::of))
        })
        .unwrap_or_default();
    let draft = RwSignal::new(start);
    let problem = RwSignal::new(None::<String>);
    let editing = StoredValue::new(editing);
    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let built = encoders
            .with_untracked(|e| draft.with_untracked(|d| d.edit(editing.get_value().as_ref(), e)));
        match built {
            Ok(edit) => {
                send.run(edit);
                open.set(None);
            }
            Err(why) => problem.set(Some(why)),
        }
    };
    let title = if editing.get_value().is_some() {
        "Edit encoder"
    } else {
        "New encoder"
    };
    view! {
        <form class="profile-form" aria-label=title on:submit=save>
            <label class="profile-field">
                <span class="label">"Name"</span>
                <input
                    type="text"
                    aria-label="Encoder name"
                    prop:value=move || draft.with(|d| d.name.clone())
                    on:input=move |event| {
                        let name = event_target_value(&event);
                        draft.update(|d| d.name = name);
                    }
                />
            </label>
            <label class="profile-field">
                <span class="label">"Video codec"</span>
                <select
                    aria-label="Video codec"
                    on:change=move |event| {
                        let codec = match event_target_value(&event).as_str() {
                            "hevc" => VideoCodec::Hevc,
                            "av1" => VideoCodec::Av1,
                            _ => VideoCodec::H264,
                        };
                        draft.update(|d| d.codec = codec);
                    }
                >
                    {[(VideoCodec::H264, "h264"), (VideoCodec::Hevc, "hevc"), (VideoCodec::Av1, "av1")]
                        .into_iter()
                        .map(|(codec, value)| {
                            view! {
                                <option value=value prop:selected=move || draft.with(|d| d.codec == codec)>
                                    {codec_label(codec)}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
                <span class="field-note">"HEVC and AV1 need an encoder for them on this machine, else the output fails to start."</span>
            </label>
            <label class="profile-field">
                <span class="label">"Rate control"</span>
                <select
                    aria-label="Rate control"
                    on:change=move |event| {
                        let vbr = event_target_value(&event) == "vbr";
                        draft.update(|d| d.vbr = vbr);
                    }
                >
                    <option value="cbr" prop:selected=move || draft.with(|d| !d.vbr)>"Constant (CBR)"</option>
                    <option value="vbr" prop:selected=move || draft.with(|d| d.vbr)>"Variable (VBR)"</option>
                </select>
            </label>
            <label class="profile-field">
                <span class="label">"Bit rate (kbps)"</span>
                <input
                    type="number"
                    aria-label="Bit rate"
                    min="100"
                    max="100000"
                    prop:value=move || draft.with(|d| d.kbps.to_string())
                    on:change=move |event| {
                        let kbps = number(&event, 0);
                        draft.update(|d| d.kbps = kbps);
                    }
                />
            </label>
            <Show when=move || draft.with(|d| d.vbr)>
                <label class="profile-field">
                    <span class="label">"Peak (kbps)"</span>
                    <input
                        type="number"
                        aria-label="Peak bit rate"
                        min="100"
                        max="200000"
                        prop:value=move || draft.with(|d| d.max_kbps.to_string())
                        on:change=move |event| {
                            let max = number(&event, 0);
                            draft.update(|d| d.max_kbps = max);
                        }
                    />
                </label>
            </Show>
            <label class="profile-field">
                <span class="label">"Keyframe every (s)"</span>
                <input
                    type="number"
                    aria-label="Keyframe every"
                    min="1"
                    max="10"
                    prop:value=move || draft.with(|d| d.keyframe_s.to_string())
                    on:change=move |event| {
                        let s = number(&event, 0);
                        draft.update(|d| d.keyframe_s = s);
                    }
                />
            </label>
            <label class="profile-field">
                <span class="label">"Hardware encoder"</span>
                <select
                    aria-label="Hardware encoder"
                    on:change=move |event| {
                        let hardware = match event_target_value(&event).as_str() {
                            "require" => HardwarePreference::Require,
                            "avoid" => HardwarePreference::Avoid,
                            _ => HardwarePreference::Prefer,
                        };
                        draft.update(|d| d.hardware = hardware);
                    }
                >
                    {[
                        (HardwarePreference::Prefer, "prefer", "Prefer, else software"),
                        (HardwarePreference::Require, "require", "Require"),
                        (HardwarePreference::Avoid, "avoid", "Software only"),
                    ]
                        .into_iter()
                        .map(|(hardware, value, label)| {
                            view! {
                                <option value=value prop:selected=move || draft.with(|d| d.hardware == hardware)>
                                    {label}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            </label>
            <label class="profile-field">
                <span class="label">"Audio bit rate (kbps, AAC)"</span>
                <input
                    type="number"
                    aria-label="Audio bit rate"
                    min="32"
                    max="320"
                    prop:value=move || draft.with(|d| d.audio_kbps.to_string())
                    on:change=move |event| {
                        let kbps = number(&event, 0);
                        draft.update(|d| d.audio_kbps = kbps);
                    }
                />
            </label>
            {move || problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
            <p class="hint">"A running output keeps its encoding until it starts again."</p>
            <div class="buttons">
                <button type="submit" class="primary">"Save"</button>
                <button type="button" on:click=move |_| open.set(None)>"Cancel"</button>
            </div>
        </form>
    }
}

/// The form that creates a video mix, or edits `editing`.
#[component]
fn MixForm(
    editing: Option<VideoMixId>,
    mixes: RwSignal<Vec<VideoMixInfo>>,
    scenes: Signal<Vec<(SceneId, String)>>,
    send: Callback<Edit>,
    open: RwSignal<Option<Option<VideoMixId>>>,
) -> impl IntoView {
    let start = editing
        .as_ref()
        .and_then(|id| {
            mixes.with_untracked(|m| m.iter().find(|m| &m.mix_id == id).map(MixDraft::of))
        })
        .unwrap_or_default();
    let draft = RwSignal::new(start);
    let problem = RwSignal::new(None::<String>);
    let editing = StoredValue::new(editing);
    let save = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let built = mixes
            .with_untracked(|m| draft.with_untracked(|d| d.edit(editing.get_value().as_ref(), m)));
        match built {
            Ok(edit) => {
                send.run(edit);
                open.set(None);
            }
            Err(why) => problem.set(Some(why)),
        }
    };
    let preset = move || {
        draft.with(|d| {
            MIX_SIZES
                .iter()
                .position(|(_, w, h)| (*w, *h) == (d.width, d.height))
                .map_or_else(|| "custom".to_owned(), |i| i.to_string())
        })
    };
    let title = if editing.get_value().is_some() {
        "Edit video mix"
    } else {
        "New video mix"
    };
    view! {
        <form class="profile-form" aria-label=title on:submit=save>
            <label class="profile-field">
                <span class="label">"Name"</span>
                <input
                    type="text"
                    aria-label="Video mix name"
                    prop:value=move || draft.with(|d| d.name.clone())
                    on:input=move |event| {
                        let name = event_target_value(&event);
                        draft.update(|d| d.name = name);
                    }
                />
            </label>
            <label class="profile-field">
                <span class="label">"Shows"</span>
                <select
                    aria-label="Shows"
                    on:change=move |event| {
                        let input = input_of(&event_target_value(&event));
                        draft.update(|d| d.input = input);
                    }
                >
                    {move || {
                        let chosen = draft.with(|d| input_value(&d.input));
                        let mut options = vec![
                            ("program".to_owned(), "Program".to_owned()),
                            ("preview".to_owned(), "Preview".to_owned()),
                        ];
                        options.extend(
                            scenes.get().into_iter().map(|(id, name)| (format!("scene:{id}"), name)),
                        );
                        options
                            .into_iter()
                            .map(|(value, label)| {
                                let selected = value == chosen;
                                view! { <option value=value selected=selected>{label}</option> }
                            })
                            .collect_view()
                    }}
                </select>
            </label>
            <label class="profile-field">
                <span class="label">"Size"</span>
                <select
                    aria-label="Size"
                    on:change=move |event| {
                        if let Some(&(_, w, h)) = event_target_value(&event)
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| MIX_SIZES.get(i))
                        {
                            draft.update(|d| {
                                d.width = w;
                                d.height = h;
                            });
                        }
                    }
                >
                    {MIX_SIZES
                        .iter()
                        .enumerate()
                        .map(|(i, (label, _, _))| {
                            let value = i.to_string();
                            let mine = value.clone();
                            view! {
                                <option value=value prop:selected=move || preset() == mine>{*label}</option>
                            }
                        })
                        .collect_view()}
                    <option value="custom" prop:selected=move || preset() == "custom">"Custom"</option>
                </select>
            </label>
            <div class="profile-field size-pair">
                <label>
                    <span class="label">"Width"</span>
                    <input
                        type="number"
                        aria-label="Width"
                        min="16"
                        max="8192"
                        step="2"
                        prop:value=move || draft.with(|d| d.width.to_string())
                        on:change=move |event| {
                            let w = number(&event, 0);
                            draft.update(|d| d.width = w);
                        }
                    />
                </label>
                <label>
                    <span class="label">"Height"</span>
                    <input
                        type="number"
                        aria-label="Height"
                        min="16"
                        max="8192"
                        step="2"
                        prop:value=move || draft.with(|d| d.height.to_string())
                        on:change=move |event| {
                            let h = number(&event, 0);
                            draft.update(|d| d.height = h);
                        }
                    />
                </label>
            </div>
            <label class="profile-field">
                <span class="label">"Rate"</span>
                <select
                    aria-label="Rate"
                    on:change=move |event| {
                        let n = number(&event, 1);
                        draft.update(|d| d.fps_divisor = n);
                    }
                >
                    {[(1, "Every frame"), (2, "Every 2nd frame (half rate)"), (3, "Every 3rd frame"), (4, "Every 4th frame")]
                        .into_iter()
                        .map(|(n, label)| {
                            view! {
                                <option value=n.to_string() prop:selected=move || draft.with(|d| d.fps_divisor == n)>
                                    {label}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
                <span class="field-note">"Of the canvas's base rate."</span>
            </label>
            {move || problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
            <p class="hint">"A running output follows a new input at once; a new size or rate applies when it starts again."</p>
            <div class="buttons">
                <button type="submit" class="primary">"Save"</button>
                <button type="button" on:click=move |_| open.set(None)>"Cancel"</button>
            </div>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, name: &str, profile: EncoderProfile) -> EncoderProfileInfo {
        serde_json::from_value(serde_json::json!({
            "profile_id": id,
            "name": name,
            "profile": profile,
        }))
        .expect("a profile")
    }

    #[test]
    fn a_new_encoder_is_the_default_encoding_with_a_fresh_id() {
        let taken = [info("enc_1", "A", EncoderProfile::DEFAULT)];
        let draft = EncoderDraft::new();
        assert_eq!(draft.profile(), EncoderProfile::DEFAULT);
        assert_eq!(
            draft.edit(None, &taken),
            Ok(Edit::EncoderCreate {
                profile_id: EncoderProfileId::new("enc_2"),
                name: "New encoder".into(),
                profile: EncoderProfile::DEFAULT,
            })
        );
    }

    #[test]
    fn an_edit_sends_the_codec_and_rate_control_and_round_trips() {
        let mut draft = EncoderDraft::new();
        draft.name = " Vertical AV1 ".into();
        draft.codec = VideoCodec::Av1;
        draft.vbr = true;
        draft.kbps = 6000;
        draft.max_kbps = 9000;
        draft.hardware = HardwarePreference::Require;
        let id = EncoderProfileId::new("enc_1");
        let Ok(Edit::EncoderUpdate {
            profile_id,
            name,
            profile,
        }) = draft.edit(Some(&id), &[])
        else {
            panic!("an update");
        };
        assert_eq!((profile_id, name.as_str()), (id.clone(), "Vertical AV1"));
        assert_eq!(
            profile.video.rate_control,
            RateControl::Vbr {
                kbps: 6000,
                max_kbps: 9000
            }
        );
        let back = EncoderDraft::of(&info("enc_1", "Vertical AV1", profile));
        assert_eq!(back.profile(), profile);
        assert_eq!(
            encoder_summary(&profile),
            "AV1 · VBR 6000 kbps, peak 9000 · keyframe every 2 s · hardware only · AAC 160 kbps"
        );
    }

    #[test]
    fn an_encoding_the_engine_refuses_names_the_field() {
        let mut draft = EncoderDraft::new();
        draft.vbr = true;
        draft.kbps = 6000;
        draft.max_kbps = 5000;
        let refused = draft.edit(None, &[]);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.starts_with("Peak bit rate")),
            "{refused:?}"
        );
        draft.vbr = false;
        draft.audio_kbps = 1000;
        let refused = draft.edit(None, &[]);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.starts_with("Audio bit rate")),
            "{refused:?}"
        );
        draft.audio_kbps = 160;
        draft.name = "  ".into();
        assert!(draft.edit(None, &[]).is_err());
    }

    #[test]
    fn a_vertical_mix_of_a_scene_is_created_and_checked() {
        let scenes = vec![(SceneId::new("cam"), "Camera".to_owned())];
        let mut draft = MixDraft::new();
        draft.input = input_of("scene:cam");
        assert_eq!(input_value(&draft.input), "scene:cam");
        let existing: Vec<VideoMixInfo> =
            vec![serde_json::from_value(serde_json::json!({
            "mix_id": "mix_1",
            "name": "Side",
            "mix": {"input": {"type": "program"}, "resolution": {"width": 1280, "height": 720}},
        }))
        .expect("a mix")];
        let Ok(Edit::VideoMixCreate { mix_id, name, mix }) = draft.edit(None, &existing) else {
            panic!("a create");
        };
        assert_eq!((mix_id.as_str(), name.as_str()), ("mix_2", "Vertical"));
        assert_eq!(
            mix_summary(&mix, &scenes),
            "Camera · 1080 × 1920 · every frame"
        );
        assert_eq!(MixDraft::of(&existing[0]).mix(), existing[0].mix);
        draft.width = 1081;
        let refused = draft.edit(None, &existing);
        assert!(
            refused.as_ref().is_err_and(|e| e.starts_with("Width")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_profile_or_mix_in_use_names_its_outputs() {
        let outputs: Vec<OutputInfo> = vec![
            serde_json::from_value(serde_json::json!({
                "output_id": "out_1",
                "kind": {"type": "record", "path": "/tmp/a.mkv", "container": "mkv"},
                "encoder_profile": "enc_1",
                "video_mix": "mix_1",
            }))
            .expect("an output"),
        ];
        let users = using_encoder(&outputs, &EncoderProfileId::new("enc_1"));
        assert_eq!(
            in_use(&users, "Used by").as_deref(),
            Some("Used by out_1; pick another for it first")
        );
        assert!(using_encoder(&outputs, &EncoderProfileId::new("enc_2")).is_empty());
        assert_eq!(showing_mix(&outputs, &VideoMixId::new("mix_1")).len(), 1);
        assert_eq!(in_use(&[], "Shown by"), None);
    }
}
