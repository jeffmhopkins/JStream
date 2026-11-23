//! Motion modifiers on the wire ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 3, since 0.81).
//!
//! An item, a group's item included, holds an ordered list of at most
//! [`MAX_MOTION_MODIFIERS`] modifiers ([`MotionModifier`]). Each changes one property
//! continuously as a pure function of the engine's animation time, and is evaluated as
//! each frame is drawn, never stored: the item's own values don't change. Positions,
//! angles and crop edges add the modifier's value; scale and opacity are multiplied by
//! `1 + value`, and opacity is then clamped to `0..=1`. `item.set_motion` writes the
//! whole list, and a keyframe scene can override it whole (`OverrideField::Motion`).
//!
//! An `audio` modifier ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md),
//! since 0.86) follows a measure of a bus or a source instead of the clock: the engine
//! measures what it hears each frame and follows it with the modifier's attack and
//! release.
//!
//! Since 0.109 a modifier can drive a number or colour setting of one of the item's own
//! effects instead of a placement property ([ADR-0138](../../../docs/adr/0138-motion-drives-effect-settings.md)):
//! `property` `effect` with an `effect` target, [`MotionEffect`]. Its value is added to
//! a number, kept within the range the effect's kind gives it, or to a colour's red,
//! green and blue as a fraction of full.
//!
//! Since 0.145 a modifier can drive a live number or colour setting of the item's
//! source instead ([ADR-0171](../../../docs/adr/0171-live-data-from-a-daw-drives-scenes.md)
//! decision 4): `property` `source` with a `source` target, [`MotionSetting`]: a light's
//! colour and intensity, or, on a model, one material's colours and glow by name.
//!
//! A `midi` modifier ([ADR-0168](../../../docs/adr/0168-midi-controllers-drive-motion.md),
//! since 0.151) follows a MIDI control change, pitch bend or channel pressure instead of
//! the clock: the controller's value is mapped onto 0 to 1, smoothed over
//! `smoothing_ms`, bent by `curve` and mapped onto `from..to`, so a fader or a knob
//! poses an item live, as ADR-0122 decision 1's value bindings describe.
//!
//! A `data` modifier ([ADR-0171](../../../docs/adr/0171-live-data-from-a-daw-drives-scenes.md)
//! decision 3, since 0.160) follows one field of a JTunnel sender's live data the same
//! way: a number through its `range`, `true` and `false` as 1 and 0, and a text through
//! its `states` table, so a DAW's transport, clips and meters pose an item. Since 0.164,
//! on a colour setting it can set the colour instead of lightening it: `color_from`
//! `field` takes a `#rrggbb` text from the field, and `palette` picks one by its text.

use crate::binding::MAX_INPUT_NAME_LEN;
use crate::finite::Finite32;
use crate::{BusId, FilterId, SourceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The most motion modifiers one item, or one scene camera, holds: 32 since 0.167
/// (ADR-0176 decision 6), 8 before, so a controller can be bound to every property.
pub const MAX_MOTION_MODIFIERS: usize = 32;

/// The longest motion modifier ID, in bytes.
pub const MAX_MOTION_ID_LEN: usize = 64;

/// The property a motion modifier changes.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionProperty {
    /// Horizontal position of the box's centre, in canvas pixels: added.
    X,
    /// Vertical position of the box's centre, in canvas pixels: added.
    Y,
    /// The box's size about its centre: multiplied by `1 + value`.
    Scale,
    /// Rotation in the canvas plane, in degrees: added.
    Rotation,
    /// 3D tilt (`tilt_deg`), in degrees: added.
    Tilt,
    /// 3D turn (`turn_deg`), in degrees: added.
    Turn,
    /// 3D offset along the viewing axis (`depth_px`), in pixels toward the viewer:
    /// added.
    Depth,
    /// Opacity: multiplied by `1 + value`, then clamped to `0..=1`.
    Opacity,
    /// Source pixels cut from the left edge: added, never below 0.
    CropLeft,
    /// Source pixels cut from the top edge: added, never below 0.
    CropTop,
    /// Source pixels cut from the right edge: added, never below 0.
    CropRight,
    /// Source pixels cut from the bottom edge: added, never below 0.
    CropBottom,
    /// The box's width alone about its centre, since 0.168 (ADR-0178): multiplied by
    /// `1 + value`, after `scale`.
    Width,
    /// The box's height alone about its centre, since 0.168: as `width`.
    Height,
    /// A number or colour setting of one of the item's own effects, which the
    /// modifier's `effect` names, since 0.109 (ADR-0138): a number has the value added,
    /// kept within the range the effect's kind gives it and rounded for a whole-number
    /// one; a colour has it added to each of its red, green and blue as a fraction of
    /// full. Not in
    /// [`MotionProperty::ALL`], which lists the placement properties.
    Effect,
    /// A live number or colour setting of the item's own source, which the modifier's
    /// `source` names, since 0.145 (ADR-0171 decision 4): changed as an `effect`
    /// setting is, for how this item draws its source only. Not in
    /// [`MotionProperty::ALL`].
    Source,
    /// A property this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl MotionProperty {
    /// Every property, in the order above.
    pub const ALL: [MotionProperty; 14] = [
        MotionProperty::X,
        MotionProperty::Y,
        MotionProperty::Scale,
        MotionProperty::Rotation,
        MotionProperty::Tilt,
        MotionProperty::Turn,
        MotionProperty::Depth,
        MotionProperty::Opacity,
        MotionProperty::CropLeft,
        MotionProperty::CropTop,
        MotionProperty::CropRight,
        MotionProperty::CropBottom,
        MotionProperty::Width,
        MotionProperty::Height,
    ];
}

/// The waveform of an `oscillate` modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionShape {
    /// A sine: starts at 0, rising.
    #[default]
    Sine,
    /// A triangle: starts at 0, rising, straight between -1 and 1.
    Triangle,
    /// A square: 1 for the first half of each period, -1 for the second.
    Square,
    /// A sawtooth: rises from -1 to 1 over each period, then jumps back.
    Saw,
    /// A shape this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl MotionShape {
    /// Every shape, in the order above.
    pub const ALL: [MotionShape; 4] = [
        MotionShape::Sine,
        MotionShape::Triangle,
        MotionShape::Square,
        MotionShape::Saw,
    ];
}

/// What an `audio` modifier measures of what it hears (since 0.86).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionMeasure {
    /// The RMS level of the latest window.
    #[default]
    Level,
    /// The peak absolute sample of the latest window.
    Peak,
    /// A jump of the level above its recent average: 1 on the jump, else 0.
    Onset,
    /// The spectrum between `low_hz` and `high_hz`, as an amplitude: a full-scale sine
    /// inside the band reads 0 dBFS.
    Band,
    /// A measure this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument` and draws a stored
    /// one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// How an `audio` modifier bends its 0 to 1 envelope before mapping it onto its range.
/// None overshoots (since 0.86).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionCurve {
    /// As it is.
    #[default]
    Linear,
    /// Cubic, slow at first: quiet passages stay low and only loud ones lift.
    EaseIn,
    /// Cubic, fast at first: quiet passages already show.
    EaseOut,
    /// Cubic at both ends.
    EaseInOut,
    /// A curve this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// What a `midi` modifier follows (since 0.151, ADR-0168).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionMidiControl {
    /// A control change: `controller`'s value, 0 to 127, as 0 to 1.
    #[default]
    Cc,
    /// The pitch bend, -8192 to 8191, as 0 to 1: the wheel at rest is one half.
    PitchBend,
    /// The channel pressure (aftertouch), 0 to 127, as 0 to 1.
    Pressure,
    /// A control this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument` and draws a stored
    /// one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// How a `data` modifier on a colour setting sets the colour, rather than lightening it
/// (since 0.164, ADR-0171 decision 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionColorFrom {
    /// The field is a `#rrggbb` text (or `#rrggbbaa`, its alpha ignored), taken as it is.
    Field,
    /// The field's text picks a colour from `palette`.
    Palette,
    /// A way this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// What a modifier does, with its parameters, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionKind {
    /// Smooth seeded noise within `-amplitude..=amplitude`. The same seed gives the same
    /// motion on every machine; each extra octave adds detail at twice the frequency and
    /// half the size.
    Wiggle {
        /// The largest value it reaches, `0..=100000`.
        amplitude: Finite32,
        /// About how many turns of direction a second, `0..=60` and not 0.
        frequency_hz: Finite32,
        /// Picks the noise. Default 0.
        #[serde(default)]
        seed: u32,
        /// How many octaves, `1..=4`. Default 1.
        #[serde(default = "one_octave")]
        octaves: u8,
    },
    /// A periodic wave within `-amplitude..=amplitude`.
    Oscillate {
        /// Its waveform. Default `sine`.
        #[serde(default)]
        shape: MotionShape,
        /// The largest value it reaches, `0..=100000`.
        amplitude: Finite32,
        /// One period, in milliseconds, `1..=3600000`.
        period_ms: u32,
        /// Its offset into the period, as a fraction in `0..1`. Default 0.
        #[serde(default)]
        phase: Finite32,
    },
    /// A constant rate per second, of either sign, wrapping into `0..wrap` when `wrap`
    /// is set.
    Drift {
        /// Units per second, within `-100000..=100000`.
        rate_per_s: Finite32,
        /// Where it wraps back to 0, `0..=100000` and not 0. Omitted for no wrap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wrap: Option<Finite32>,
    },
    /// Follows a measure of a bus or a source (ADR-0100, since 0.86): the measure in dBFS from
    /// `floor_db` to `ceiling_db` is mapped onto 0 to 1 (an onset as it is), followed with
    /// `attack_ms` and `release_ms`, bent by `curve`, and mapped onto `from..to`. Silence
    /// is `from`. It hears what a `spectrum` source with the same `audio` and
    /// `audio_source` would draw, and silence while that is silent.
    Audio {
        /// The bus it hears, after its fader. Omitted for the program mix: the master
        /// after its filters, as the outputs hear it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<BusId>,
        /// A source to hear instead of the bus: that source after its own filters,
        /// fader, mute and follow. Omitted to hear the bus.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_source: Option<SourceId>,
        /// What it measures. Default `level`.
        #[serde(default)]
        measure: MotionMeasure,
        /// A `band` measure's lowest frequency, in hertz, `20..=20000`. Only with `band`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        low_hz: Option<Finite32>,
        /// A `band` measure's highest frequency, in hertz, above `low_hz` and at most
        /// 20000. Only with `band`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        high_hz: Option<Finite32>,
        /// The level, in dBFS, that maps to 0: at least -90 and below `ceiling_db`.
        /// Default -60.
        #[serde(default = "default_floor_db")]
        floor_db: Finite32,
        /// The level, in dBFS, that maps to 1: above `floor_db` and at most 0. Default 0.
        #[serde(default)]
        ceiling_db: Finite32,
        /// How fast it rises, in milliseconds, `0..=10000`. Default 10.
        #[serde(default = "default_attack_ms")]
        attack_ms: u32,
        /// How fast it falls, in milliseconds, `0..=10000`. Default 250.
        #[serde(default = "default_release_ms")]
        release_ms: u32,
        /// How the envelope is bent. Default `linear`.
        #[serde(default)]
        curve: MotionCurve,
        /// Its value at silence, in the property's unit, within `-100000..=100000`.
        /// Default 0.
        #[serde(default)]
        from: Finite32,
        /// Its value at the ceiling, within `-100000..=100000`; below `from` to invert.
        /// Default 1.
        #[serde(default = "default_to")]
        to: Finite32,
    },
    /// Follows a MIDI controller (ADR-0168, since 0.151): `control`'s latest value on
    /// `channel` from `input`, as 0 to 1, followed over `smoothing_ms`, bent by `curve`
    /// and mapped onto `from..to`. Before the controller has sent anything it reads 0
    /// (a pitch bend one half), so the item rests at `from`.
    Midi {
        /// What it follows. Default `cc`.
        #[serde(default)]
        control: MotionMidiControl,
        /// The controller number, 0 to 127, with `control` `cc` and only then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<u8>,
        /// The channel, 1 to 16, as a MIDI device's panel shows it, or omitted for any:
        /// then the latest value sent on any channel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// The input, a MIDI port or a JTunnel sender by name, as a binding's trigger
        /// names it, or omitted for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        /// How long it takes to follow a new value, in milliseconds, `0..=10000`: about
        /// two thirds of the way in that time, a one-pole smoothing. 0 jumps at once.
        /// Default 50, which hides a controller's 128 steps.
        #[serde(default = "default_smoothing_ms")]
        smoothing_ms: u32,
        /// How the 0 to 1 value is bent. Default `linear`.
        #[serde(default)]
        curve: MotionCurve,
        /// Its value at the controller's lowest, in the property's unit, within
        /// `-100000..=100000`. Default 0.
        #[serde(default)]
        from: Finite32,
        /// Its value at the controller's highest, within `-100000..=100000`; below
        /// `from` to invert. Default 1.
        #[serde(default = "default_to")]
        to: Finite32,
    },
    /// Follows one field of a JTunnel sender's live data (ADR-0171 decision 3, since
    /// 0.160): `field` of the latest message for `key` from `input`, read as 0 to 1,
    /// followed over `smoothing_ms`, bent by `curve` and mapped onto `from..to`. A number
    /// reads through `range`, `true` 1 and `false` 0, a text its `states` entry or 0, and
    /// an absent field 0, so the item rests at `from` until the sender says otherwise.
    Data {
        /// The sender, a JTunnel sender by name, or omitted for whichever sent the key
        /// last.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        /// The key, 1 to 63 bytes, as the sender names it: `transport`, `track/3`.
        key: String,
        /// A dotted path of object names and list indexes from 0 inside the key's
        /// message (`playing`, `slots.2.state`), at most 255 bytes; empty for the
        /// message itself. Default empty.
        #[serde(default)]
        field: String,
        /// A number at the first reads 0 and at the second 1, clamped between; the
        /// first may be the larger, to invert. They must differ. Default `[0, 1]`.
        #[serde(default = "default_range")]
        range: [Finite32; 2],
        /// What a text reads, 0 to 1, by the text: `{"recording": 1, "playing": 0.5}`.
        /// At most 32 entries, each text 1 to 100 bytes; a text not listed reads 0.
        /// Default none.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        states: BTreeMap<String, Finite32>,
        /// How long it takes to follow a new value, in milliseconds, `0..=10000`, as a
        /// `midi` modifier's. Default 50; 0 jumps, which suits an on and off.
        #[serde(default = "default_smoothing_ms")]
        smoothing_ms: u32,
        /// How the 0 to 1 value is bent. Default `linear`.
        #[serde(default)]
        curve: MotionCurve,
        /// Its value at 0, in the property's unit, within `-100000..=100000`. Default 0.
        #[serde(default)]
        from: Finite32,
        /// Its value at 1, within `-100000..=100000`; below `from` to invert. Default 1.
        #[serde(default = "default_to")]
        to: Finite32,
        /// Since 0.164, only with `property` `effect` or `source`: on a colour setting,
        /// set the colour from the field instead of adding to it. While the field gives
        /// a colour it replaces the setting's own, before other modifiers add to it,
        /// blended over `smoothing_ms`; otherwise the setting's own colour shows. On a
        /// setting that isn't a colour it adds nothing. Omitted to add the value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color_from: Option<MotionColorFrom>,
        /// With `color_from` `palette`: a `#rrggbb` colour by the field's text,
        /// `{"recording": "#ff2020", "playing": "#20ff40"}`. At most 32 entries, each
        /// text 1 to 100 bytes; a text not listed sets nothing. Default none.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        palette: BTreeMap<String, String>,
    },
    /// A one-shot decaying shake, since 0.84 (#1107):
    /// `amplitude · e^(−decay_per_s·t) · sin(2π·frequency_hz·t)` for `t` seconds from
    /// the start of its clock, until `duration_ms`, and 0 after. Usually on the
    /// `triggered` clock, so `item.trigger_motion` (a hotkey, say) fires it.
    Impulse {
        /// Its size at the start, `0..=100000`.
        amplitude: Finite32,
        /// How many times a second it swings, `0..=60` and not 0.
        frequency_hz: Finite32,
        /// How fast it dies away, per second, `0..=1000`: 0 keeps its size to the end.
        /// Default 0.
        #[serde(default)]
        decay_per_s: Finite32,
        /// How long it lasts, in milliseconds, `1..=3600000`.
        duration_ms: u32,
    },
    /// A kind this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

fn one_octave() -> u8 {
    1
}

fn default_floor_db() -> Finite32 {
    Finite32::new(-60.0).unwrap_or(Finite32::ZERO)
}

fn default_attack_ms() -> u32 {
    10
}

fn default_release_ms() -> u32 {
    250
}

fn default_smoothing_ms() -> u32 {
    50
}

fn default_to() -> Finite32 {
    Finite32::ONE
}

fn default_range() -> [Finite32; 2] {
    [Finite32::ZERO, Finite32::ONE]
}

/// The longest key a `data` modifier names, in bytes: JTunnel's.
pub const MAX_DATA_KEY_LEN: usize = 63;
/// The longest field path a `data` modifier names, in bytes.
pub const MAX_DATA_FIELD_LEN: usize = 255;
/// The most entries in a `data` modifier's `states`.
pub const MAX_DATA_STATES: usize = 32;
/// The longest text in a `data` modifier's `states`, in bytes.
pub const MAX_DATA_STATE_LEN: usize = 100;

/// Where a modifier's time starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionClock {
    /// The engine's animation time: every item with the same parameters moves in step.
    #[default]
    Engine,
    /// From when the engine last started drawing the item for program or a video mix,
    /// so it starts fresh each time it is shown. While it isn't shown, and in a scene
    /// or source preview, the modifier is at its start.
    Shown,
    /// From when `item.trigger_motion` last fired it, since 0.84 (#1107). Until it is
    /// fired, and in a scene or source preview, the modifier contributes nothing.
    Triggered,
    /// A clock this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// One motion modifier on an item (ADR-0099 decision 3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MotionModifier {
    /// Its ID, unique in the item's list: 1 to [`MAX_MOTION_ID_LEN`] bytes of ASCII
    /// letters, digits, `_` and `-`.
    pub modifier_id: String,
    /// What it changes.
    pub property: MotionProperty,
    /// The effect setting it changes, with `property` `effect` and only then (since
    /// 0.109, ADR-0138). Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<MotionEffect>,
    /// The source setting it changes, with `property` `source` and only then (since
    /// 0.145, ADR-0171). Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<MotionSetting>,
    /// What it does.
    #[serde(flatten)]
    pub kind: MotionKind,
    /// Where its time starts. Default `engine`.
    #[serde(default)]
    pub clock: MotionClock,
    /// Whether it applies. Default `true`; omitted when `true`.
    #[serde(default = "enabled", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

/// The longest effect parameter name a [`MotionEffect`] takes, in bytes.
pub const MAX_MOTION_PARAM_LEN: usize = 64;

/// The effect setting an `effect` modifier drives (since 0.109, ADR-0138): a number
/// (`float` or `int` in its kind's descriptor) or colour parameter of one of the item's
/// own effects, as the item draws them (a keyframe's overridden effects in that
/// keyframe). One that names no such effect, or a parameter of another type, adds
/// nothing, as an effect that was removed or changed kind since does.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MotionEffect {
    /// The effect, by its `filter_id` in the item's effects.
    pub filter_id: FilterId,
    /// The parameter, by its ID in the kind's descriptor, such as a `border`'s `width`:
    /// 1 to [`MAX_MOTION_PARAM_LEN`] bytes of ASCII letters, digits and `_`.
    pub param: String,
}

/// The longest material name a [`MotionSetting`] takes, in bytes.
pub const MAX_MOTION_MATERIAL_LEN: usize = 128;

/// The source setting a `source` modifier drives (since 0.145, ADR-0171 decision 4): a
/// live number or colour setting of the item's own source kind, as this item draws its
/// source; nothing is stored. A `light` takes `color`, `intensity`, `range_px`,
/// `cone_deg`, `softness` and `shadow_softness`. A `model` takes `color`, `metallic`
/// and `roughness` on every material, or, with `material`, on the file's materials of
/// that name, which also take `emissive` (the glow's colour) and `emissive_strength`
/// (a multiplier of it, 1 at rest, kept within 0 to 64). One that names no such
/// setting, or a material the model doesn't have, adds nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MotionSetting {
    /// The setting, by its ID in the source kind's descriptor (or `emissive` and
    /// `emissive_strength` with `material`): 1 to [`MAX_MOTION_PARAM_LEN`] bytes of ASCII
    /// letters, digits and `_`.
    pub param: String,
    /// A model's materials it drives, by their name in the file: 1 to
    /// [`MAX_MOTION_MATERIAL_LEN`] bytes. Omitted for every material, or for a source
    /// that isn't a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
}

fn enabled() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Why [`check_motion`] refuses a list: the field, `motion` or `motion.<n>.<param>`, and
/// what it must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionProblem {
    /// The field, as an `invalid_argument` error names it.
    pub field: String,
    /// What is wrong, or what the field must be.
    pub reason: String,
}

/// The largest amplitude or wrap, and the largest drift rate either way.
const MAX_MAGNITUDE: f32 = 100_000.0;
/// The highest wiggle frequency, in hertz.
const MAX_FREQUENCY_HZ: f32 = 60.0;
/// The longest oscillator period or impulse, in milliseconds: one hour.
const MAX_PERIOD_MS: u32 = 3_600_000;
/// The fastest impulse decay, per second.
const MAX_DECAY_PER_S: f32 = 1000.0;

/// Checks a motion modifier ID: 1 to [`MAX_MOTION_ID_LEN`] bytes of ASCII letters,
/// digits, `_` and `-`. Returns what it must be if it isn't.
pub fn check_motion_id(id: &str) -> Result<(), &'static str> {
    let well_formed = !id.is_empty()
        && id.len() <= MAX_MOTION_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if well_formed {
        Ok(())
    } else {
        Err("must be 1 to 64 bytes of ASCII letters, digits, `_` and `-`")
    }
}

/// Checks an item's whole motion list as `item.set_motion` does (ADR-0099): at most
/// [`MAX_MOTION_MODIFIERS`], each ID well formed and unique, and every parameter in the
/// range its documentation gives. Returns the first problem.
pub fn check_motion(motion: &[MotionModifier]) -> Result<(), MotionProblem> {
    let problem = |field: String, reason: &str| MotionProblem {
        field,
        reason: reason.to_owned(),
    };
    if motion.len() > MAX_MOTION_MODIFIERS {
        return Err(problem(
            "motion".into(),
            "an item holds at most 32 motion modifiers; put the rest on a group around it, \
             whose motion moves its children too",
        ));
    }
    for (n, m) in motion.iter().enumerate() {
        let field = |param: &str| format!("motion.{n}.{param}");
        let id = &m.modifier_id;
        if let Err(reason) = check_motion_id(id) {
            return Err(problem(field("modifier_id"), reason));
        }
        if motion[..n].iter().any(|other| other.modifier_id == *id) {
            return Err(MotionProblem {
                field: field("modifier_id"),
                reason: format!("modifier `{id}` is in the list twice"),
            });
        }
        let unknown = "is not one this version of the protocol knows";
        if m.property == MotionProperty::Unknown {
            return Err(problem(field("property"), unknown));
        }
        if m.clock == MotionClock::Unknown {
            return Err(problem(field("clock"), unknown));
        }
        if let Some(reason) = effect_problem(m.property, m.effect.as_ref()) {
            return Err(problem(field("effect"), reason));
        }
        if let Some(reason) = source_problem(m.property, m.source.as_ref()) {
            return Err(problem(field("source"), reason));
        }
        if let Some((param, reason)) = m.kind.problem() {
            return Err(problem(field(param), reason));
        }
        if matches!(
            m.kind,
            MotionKind::Data {
                color_from: Some(_),
                ..
            }
        ) && !matches!(m.property, MotionProperty::Effect | MotionProperty::Source)
        {
            return Err(problem(
                field("color_from"),
                "sets a colour setting: it needs property effect or source",
            ));
        }
    }
    Ok(())
}

/// What is wrong with a modifier's `effect` for its `property`, if anything: property
/// `effect` needs one, every other property takes none, and its `filter_id` and `param`
/// must be well formed.
fn effect_problem(property: MotionProperty, effect: Option<&MotionEffect>) -> Option<&'static str> {
    match (property, effect) {
        (MotionProperty::Effect, None) => {
            Some("property `effect` needs an `effect` naming the filter and its parameter")
        }
        (MotionProperty::Effect, Some(effect)) => {
            let param = &effect.param;
            if effect.filter_id.as_str().is_empty() {
                Some("`filter_id` must name one of the item's effects")
            } else if param.is_empty()
                || param.len() > MAX_MOTION_PARAM_LEN
                || !param
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                Some("`param` must be 1 to 64 bytes of ASCII letters, digits and `_`")
            } else {
                None
            }
        }
        (_, Some(_)) => Some("only property `effect` takes an `effect`"),
        (_, None) => None,
    }
}

/// What is wrong with a modifier's `source` for its `property`, if anything: property
/// `source` needs one, every other property takes none, and its `param` and `material`
/// must be well formed.
fn source_problem(
    property: MotionProperty,
    source: Option<&MotionSetting>,
) -> Option<&'static str> {
    match (property, source) {
        (MotionProperty::Source, None) => {
            Some("property `source` needs a `source` naming the setting")
        }
        (MotionProperty::Source, Some(source)) => {
            let param = &source.param;
            if param.is_empty()
                || param.len() > MAX_MOTION_PARAM_LEN
                || !param
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                Some("`param` must be 1 to 64 bytes of ASCII letters, digits and `_`")
            } else if source
                .material
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.len() > MAX_MOTION_MATERIAL_LEN)
            {
                Some("`material` must be 1 to 128 bytes: leave it out for every material")
            } else {
                None
            }
        }
        (_, Some(_)) => Some("only property `source` takes a `source`"),
        (_, None) => None,
    }
}

impl MotionKind {
    /// The first parameter out of range, by wire name, with what it must be.
    pub(crate) fn problem(&self) -> Option<(&'static str, &'static str)> {
        let magnitude = |v: Finite32| (0.0..=MAX_MAGNITUDE).contains(&v.get());
        let frequency = |v: Finite32| v.get() > 0.0 && v.get() <= MAX_FREQUENCY_HZ;
        match *self {
            MotionKind::Audio {
                measure,
                low_hz,
                high_hz,
                floor_db,
                ceiling_db,
                attack_ms,
                release_ms,
                curve,
                from,
                to,
                ..
            } => audio_problem(
                measure,
                (low_hz, high_hz),
                (floor_db, ceiling_db),
                (attack_ms, release_ms),
                curve,
                (from, to),
            ),
            MotionKind::Midi {
                control,
                controller,
                channel,
                ref input,
                smoothing_ms,
                curve,
                from,
                to,
            } => midi_problem(
                control,
                controller,
                channel,
                input.as_deref(),
                smoothing_ms,
                curve,
                (from, to),
            ),
            MotionKind::Data {
                ref input,
                ref key,
                ref field,
                range,
                ref states,
                smoothing_ms,
                curve,
                from,
                to,
                color_from,
                ref palette,
            } => data_problem(
                input.as_deref(),
                key,
                field,
                range,
                states,
                (smoothing_ms, curve),
                (from, to),
            )
            .or_else(|| color_problem(color_from, palette)),
            MotionKind::Wiggle {
                amplitude,
                frequency_hz,
                octaves,
                ..
            } => {
                if !magnitude(amplitude) {
                    Some(("amplitude", "must be within 0..=100000"))
                } else if !frequency(frequency_hz) {
                    Some(("frequency_hz", "must be above 0 and at most 60"))
                } else if !(1..=4).contains(&octaves) {
                    Some(("octaves", "must be within 1..=4"))
                } else {
                    None
                }
            }
            MotionKind::Oscillate {
                shape,
                amplitude,
                period_ms,
                phase,
            } => {
                if !magnitude(amplitude) {
                    Some(("amplitude", "must be within 0..=100000"))
                } else if !(1..=MAX_PERIOD_MS).contains(&period_ms) {
                    Some(("period_ms", "must be within 1..=3600000"))
                } else if shape == MotionShape::Unknown {
                    Some(("shape", "is not one this version of the protocol knows"))
                } else if !(0.0..1.0).contains(&phase.get()) {
                    Some(("phase", "must be at least 0 and below 1"))
                } else {
                    None
                }
            }
            MotionKind::Drift { rate_per_s, wrap } => {
                if rate_per_s.get().abs() > MAX_MAGNITUDE {
                    Some(("rate_per_s", "must be within -100000..=100000"))
                } else if wrap.is_some_and(|w| !(magnitude(w) && w.get() > 0.0)) {
                    Some(("wrap", "must be above 0 and at most 100000"))
                } else {
                    None
                }
            }
            MotionKind::Impulse {
                amplitude,
                frequency_hz,
                decay_per_s,
                duration_ms,
            } => {
                if !magnitude(amplitude) {
                    Some(("amplitude", "must be within 0..=100000"))
                } else if !frequency(frequency_hz) {
                    Some(("frequency_hz", "must be above 0 and at most 60"))
                } else if !(0.0..=MAX_DECAY_PER_S).contains(&decay_per_s.get()) {
                    Some(("decay_per_s", "must be within 0..=1000"))
                } else if !(1..=MAX_PERIOD_MS).contains(&duration_ms) {
                    Some(("duration_ms", "must be within 1..=3600000"))
                } else {
                    None
                }
            }
            MotionKind::Unknown => Some(("kind", "is not one this version of the protocol knows")),
        }
    }
}

/// The longest attack or release, in milliseconds.
const MAX_ENVELOPE_MS: u32 = 10_000;

/// An `audio` modifier's first parameter out of range (ADR-0100 decision 3).
fn audio_problem(
    measure: MotionMeasure,
    (low_hz, high_hz): (Option<Finite32>, Option<Finite32>),
    (floor_db, ceiling_db): (Finite32, Finite32),
    (attack_ms, release_ms): (u32, u32),
    curve: MotionCurve,
    (from, to): (Finite32, Finite32),
) -> Option<(&'static str, &'static str)> {
    let unknown = "is not one this version of the protocol knows";
    let hz = |v: Finite32| (20.0..=20_000.0).contains(&v.get());
    let end = |v: Finite32| v.get().abs() <= MAX_MAGNITUDE;
    match measure {
        MotionMeasure::Unknown => return Some(("measure", unknown)),
        MotionMeasure::Band => match (low_hz, high_hz) {
            (None, _) => return Some(("low_hz", "a band needs it")),
            (Some(low), _) if !hz(low) => return Some(("low_hz", "must be within 20..=20000")),
            (_, None) => return Some(("high_hz", "a band needs it")),
            (Some(low), Some(high)) if !(hz(high) && high.get() > low.get()) => {
                return Some(("high_hz", "must be above low_hz and at most 20000"));
            }
            _ => {}
        },
        _ => {
            if low_hz.is_some() {
                return Some(("low_hz", "only a band measure has it"));
            }
            if high_hz.is_some() {
                return Some(("high_hz", "only a band measure has it"));
            }
        }
    }
    if !(-90.0..0.0).contains(&floor_db.get()) {
        Some(("floor_db", "must be at least -90 and below 0"))
    } else if !(ceiling_db.get() <= 0.0 && ceiling_db.get() > floor_db.get()) {
        Some(("ceiling_db", "must be above floor_db and at most 0"))
    } else if attack_ms > MAX_ENVELOPE_MS {
        Some(("attack_ms", "must be within 0..=10000"))
    } else if release_ms > MAX_ENVELOPE_MS {
        Some(("release_ms", "must be within 0..=10000"))
    } else if curve == MotionCurve::Unknown {
        Some(("curve", unknown))
    } else if !end(from) {
        Some(("from", "must be within -100000..=100000"))
    } else if !end(to) {
        Some(("to", "must be within -100000..=100000"))
    } else {
        None
    }
}

/// A `midi` modifier's first parameter out of range (ADR-0168).
fn midi_problem(
    control: MotionMidiControl,
    controller: Option<u8>,
    channel: Option<u8>,
    input: Option<&str>,
    smoothing_ms: u32,
    curve: MotionCurve,
    (from, to): (Finite32, Finite32),
) -> Option<(&'static str, &'static str)> {
    let unknown = "is not one this version of the protocol knows";
    let end = |v: Finite32| v.get().abs() <= MAX_MAGNITUDE;
    match (control, controller) {
        (MotionMidiControl::Unknown, _) => return Some(("control", unknown)),
        (MotionMidiControl::Cc, None) => {
            return Some(("controller", "a `cc` control needs its controller number"));
        }
        (MotionMidiControl::Cc, Some(n)) if n > 127 => {
            return Some(("controller", "must be 0 to 127"));
        }
        (MotionMidiControl::PitchBend | MotionMidiControl::Pressure, Some(_)) => {
            return Some(("controller", "only a `cc` control has it"));
        }
        _ => {}
    }
    if channel.is_some_and(|c| !(1..=16).contains(&c)) {
        return Some(("channel", "must be 1 to 16, or missing for any channel"));
    }
    if let Some(name) = input {
        if name.is_empty() {
            return Some(("input", "must not be empty: leave it out for any input"));
        }
        if name.len() > MAX_INPUT_NAME_LEN {
            return Some(("input", "must be at most 64 bytes"));
        }
        if name.chars().any(char::is_control) {
            return Some(("input", "must not hold control characters"));
        }
    }
    if smoothing_ms > MAX_ENVELOPE_MS {
        Some(("smoothing_ms", "must be within 0..=10000"))
    } else if curve == MotionCurve::Unknown {
        Some(("curve", unknown))
    } else if !end(from) {
        Some(("from", "must be within -100000..=100000"))
    } else if !end(to) {
        Some(("to", "must be within -100000..=100000"))
    } else {
        None
    }
}

/// A `data` modifier's first parameter out of range (ADR-0171 decision 3).
fn data_problem(
    input: Option<&str>,
    key: &str,
    field: &str,
    range: [Finite32; 2],
    states: &BTreeMap<String, Finite32>,
    (smoothing_ms, curve): (u32, MotionCurve),
    (from, to): (Finite32, Finite32),
) -> Option<(&'static str, &'static str)> {
    let end = |v: Finite32| v.get().abs() <= MAX_MAGNITUDE;
    let controls = |text: &str| text.chars().any(char::is_control);
    if let Some(name) = input {
        if name.is_empty() {
            return Some(("input", "must not be empty: leave it out for any sender"));
        }
        if name.len() > MAX_INPUT_NAME_LEN {
            return Some(("input", "must be at most 64 bytes"));
        }
        if controls(name) {
            return Some(("input", "must not hold control characters"));
        }
    }
    if key.is_empty() || key.len() > MAX_DATA_KEY_LEN || controls(key) {
        return Some(("key", "must be 1 to 63 bytes, with no control characters"));
    }
    if field.len() > MAX_DATA_FIELD_LEN || controls(field) {
        return Some((
            "field",
            "must be at most 255 bytes, with no control characters",
        ));
    }
    if !end(range[0]) || !end(range[1]) {
        return Some(("range", "must be within -100000..=100000"));
    }
    if range[0] == range[1] {
        return Some(("range", "its two ends must differ"));
    }
    if states.len() > MAX_DATA_STATES {
        return Some(("states", "must hold at most 32 entries"));
    }
    if states
        .keys()
        .any(|t| t.is_empty() || t.len() > MAX_DATA_STATE_LEN || controls(t))
    {
        return Some((
            "states",
            "each text must be 1 to 100 bytes, with no control characters",
        ));
    }
    if states.values().any(|v| !(0.0..=1.0).contains(&v.get())) {
        return Some(("states", "each value must be within 0..=1"));
    }
    if smoothing_ms > MAX_ENVELOPE_MS {
        Some(("smoothing_ms", "must be within 0..=10000"))
    } else if curve == MotionCurve::Unknown {
        Some(("curve", "is not one this version of the protocol knows"))
    } else if !end(from) {
        Some(("from", "must be within -100000..=100000"))
    } else if !end(to) {
        Some(("to", "must be within -100000..=100000"))
    } else {
        None
    }
}

/// What is wrong with a `data` modifier's colour from its field, whatever its property.
fn color_problem(
    color_from: Option<MotionColorFrom>,
    palette: &BTreeMap<String, String>,
) -> Option<(&'static str, &'static str)> {
    let is_color = |s: &str| {
        s.strip_prefix('#')
            .is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    match color_from {
        Some(MotionColorFrom::Unknown) => {
            return Some((
                "color_from",
                "is not one this version of the protocol knows",
            ));
        }
        Some(MotionColorFrom::Palette) if palette.is_empty() => {
            return Some(("palette", "must hold at least one entry"));
        }
        Some(MotionColorFrom::Palette) => {}
        _ if !palette.is_empty() => {
            return Some(("palette", "is read only with color_from palette"));
        }
        _ => {}
    }
    if palette.len() > MAX_DATA_STATES {
        return Some(("palette", "must hold at most 32 entries"));
    }
    if palette
        .keys()
        .any(|t| t.is_empty() || t.len() > MAX_DATA_STATE_LEN || t.chars().any(char::is_control))
    {
        return Some((
            "palette",
            "each text must be 1 to 100 bytes, with no control characters",
        ));
    }
    if palette.values().any(|c| !is_color(c)) {
        return Some(("palette", "each colour must be #rrggbb"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiggle(id: &str, amplitude: f32) -> MotionModifier {
        MotionModifier {
            modifier_id: id.into(),
            property: MotionProperty::X,
            effect: None,
            source: None,
            kind: MotionKind::Wiggle {
                amplitude: Finite32::new(amplitude).expect("finite"),
                frequency_hz: Finite32::ONE,
                seed: 0,
                octaves: 1,
            },
            clock: MotionClock::Engine,
            enabled: true,
        }
    }

    fn field_of(motion: &[MotionModifier]) -> Option<String> {
        check_motion(motion).err().map(|p| p.field)
    }

    #[test]
    fn a_valid_list_passes_and_each_problem_names_its_field() {
        assert_eq!(field_of(&[wiggle("a", 3.0), wiggle("b", 0.0)]), None);
        assert_eq!(field_of(&[]), None);
        let most: Vec<_> = (0..MAX_MOTION_MODIFIERS)
            .map(|n| wiggle(&format!("m{n}"), 1.0))
            .collect();
        assert_eq!(field_of(&most), None, "32 since 0.167 (ADR-0176)");
        let over: Vec<_> = (0..=MAX_MOTION_MODIFIERS)
            .map(|n| wiggle(&format!("m{n}"), 1.0))
            .collect();
        assert_eq!(field_of(&over).as_deref(), Some("motion"));
        let full = check_motion(&over).expect_err("one too many");
        assert!(
            full.reason.contains("on a group around it"),
            "the refusal says where the rest can go (#1189): {}",
            full.reason
        );
        assert_eq!(
            field_of(&[wiggle("a", 1.0), wiggle("a", 1.0)]).as_deref(),
            Some("motion.1.modifier_id")
        );
        for bad in ["", "has space", "ünï", &"x".repeat(65)] {
            assert_eq!(
                field_of(&[wiggle(bad, 1.0)]).as_deref(),
                Some("motion.0.modifier_id"),
                "{bad:?}"
            );
        }
        assert_eq!(
            field_of(&[wiggle("a", 1.0), wiggle("b", -1.0)]).as_deref(),
            Some("motion.1.amplitude")
        );
    }

    /// An `effect` modifier (ADR-0138, since 0.109) names the effect setting it drives,
    /// on the wire as `effect: {filter_id, param}`, and only property `effect` takes one.
    #[test]
    fn an_effect_modifier_names_its_setting_and_only_it_takes_one() {
        let pulse: MotionModifier = serde_json::from_value(serde_json::json!({
            "modifier_id": "pulse", "property": "effect",
            "effect": {"filter_id": "border", "param": "width"},
            "kind": "oscillate", "amplitude": 2.0, "period_ms": 500
        }))
        .expect("parses");
        assert_eq!(
            pulse.effect,
            Some(MotionEffect {
                filter_id: FilterId::new("border"),
                param: "width".into(),
            })
        );
        assert_eq!(field_of(std::slice::from_ref(&pulse)), None);
        let json = serde_json::to_value(&pulse).expect("serializes");
        assert_eq!(json["effect"]["param"], "width");
        // A placement modifier writes no `effect`.
        let still = serde_json::to_value(wiggle("a", 1.0)).expect("serializes");
        assert!(still.get("effect").is_none(), "{still}");
        let with = |effect: Option<MotionEffect>, property| MotionModifier {
            property,
            effect,
            ..pulse.clone()
        };
        let target = |filter: &str, param: &str| {
            Some(MotionEffect {
                filter_id: FilterId::new(filter),
                param: param.into(),
            })
        };
        let effect = Some("motion.0.effect");
        assert_eq!(
            field_of(&[with(None, MotionProperty::Effect)]).as_deref(),
            effect
        );
        assert_eq!(
            field_of(&[with(target("border", "width"), MotionProperty::X)]).as_deref(),
            effect
        );
        assert_eq!(
            field_of(&[with(target("", "width"), MotionProperty::Effect)]).as_deref(),
            effect
        );
        for bad in ["", "two words", "wïdth", &"w".repeat(65)] {
            assert_eq!(
                field_of(&[with(target("border", bad), MotionProperty::Effect)]).as_deref(),
                effect,
                "{bad:?}"
            );
        }
        assert!(!MotionProperty::ALL.contains(&MotionProperty::Effect));
    }

    #[test]
    fn a_source_modifier_names_its_setting_and_only_it_takes_one() {
        let glow: MotionModifier = serde_json::from_value(serde_json::json!({
            "modifier_id": "glow", "property": "source",
            "source": {"param": "emissive_strength", "material": "led_red"},
            "kind": "oscillate", "amplitude": 1.0, "period_ms": 1000
        }))
        .expect("parses");
        assert_eq!(
            glow.source,
            Some(MotionSetting {
                param: "emissive_strength".into(),
                material: Some("led_red".into()),
            })
        );
        assert_eq!(field_of(std::slice::from_ref(&glow)), None);
        let json = serde_json::to_value(&glow).expect("serializes");
        assert_eq!(json["source"]["material"], "led_red");
        // A placement modifier writes no `source`, and a setting with no material none.
        let still = serde_json::to_value(wiggle("a", 1.0)).expect("serializes");
        assert!(still.get("source").is_none(), "{still}");
        let with = |source: Option<MotionSetting>, property| MotionModifier {
            property,
            source,
            ..glow.clone()
        };
        let target = |param: &str, material: Option<&str>| {
            Some(MotionSetting {
                param: param.into(),
                material: material.map(Into::into),
            })
        };
        let whole = serde_json::to_value(with(target("color", None), MotionProperty::Source))
            .expect("serializes");
        assert!(whole["source"].get("material").is_none(), "{whole}");
        assert_eq!(
            field_of(&[with(target("intensity", None), MotionProperty::Source)]),
            None
        );
        let source = Some("motion.0.source");
        assert_eq!(
            field_of(&[with(None, MotionProperty::Source)]).as_deref(),
            source
        );
        assert_eq!(
            field_of(&[with(target("color", None), MotionProperty::Opacity)]).as_deref(),
            source
        );
        for bad in ["", "two words", &"w".repeat(65)] {
            assert_eq!(
                field_of(&[with(target(bad, None), MotionProperty::Source)]).as_deref(),
                source,
                "{bad:?}"
            );
        }
        for bad in ["", &"m".repeat(129)] {
            assert_eq!(
                field_of(&[with(target("color", Some(bad)), MotionProperty::Source)]).as_deref(),
                source,
                "{bad:?}"
            );
        }
        assert!(!MotionProperty::ALL.contains(&MotionProperty::Source));
    }

    /// #1125 review N2: a value from a newer server parses as `Unknown`, so an older
    /// client still reads `state.get`, and an edit carrying one is refused.
    #[test]
    fn a_newer_value_parses_as_unknown_and_is_refused() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let base = serde_json::json!({
            "modifier_id": "m", "property": "x", "kind": "drift", "rate_per_s": 1.0,
            "clock": "engine"
        });
        let with = |key: &str, value: serde_json::Value| {
            let mut json = base.clone();
            json[key] = value;
            json
        };
        let glow = parse(with("property", "glow".into()));
        assert_eq!(glow.property, MotionProperty::Unknown);
        assert_eq!(field_of(&[glow]).as_deref(), Some("motion.0.property"));
        let beat = parse(with("clock", "beat".into()));
        assert_eq!(beat.clock, MotionClock::Unknown);
        assert_eq!(field_of(&[beat]).as_deref(), Some("motion.0.clock"));
        let pulse = parse(serde_json::json!({
            "modifier_id": "m", "property": "x", "kind": "pulse", "every_ms": 5
        }));
        assert_eq!(pulse.kind, MotionKind::Unknown);
        assert_eq!(field_of(&[pulse]).as_deref(), Some("motion.0.kind"));
        let blob = parse(serde_json::json!({
            "modifier_id": "m", "property": "x", "kind": "oscillate", "shape": "blob",
            "amplitude": 1.0, "period_ms": 100
        }));
        assert_eq!(field_of(&[blob]).as_deref(), Some("motion.0.shape"));
    }

    #[test]
    fn each_kind_checks_its_ranges() {
        let f = |v: f32| Finite32::new(v).expect("finite");
        let with = |kind| MotionModifier {
            kind,
            ..wiggle("a", 1.0)
        };
        let cases = [
            (
                MotionKind::Wiggle {
                    amplitude: f(1.0),
                    frequency_hz: f(0.0),
                    seed: 0,
                    octaves: 1,
                },
                "frequency_hz",
            ),
            (
                MotionKind::Wiggle {
                    amplitude: f(1.0),
                    frequency_hz: f(1.0),
                    seed: 0,
                    octaves: 5,
                },
                "octaves",
            ),
            (
                MotionKind::Oscillate {
                    shape: MotionShape::Sine,
                    amplitude: f(1.0),
                    period_ms: 0,
                    phase: f(0.0),
                },
                "period_ms",
            ),
            (
                MotionKind::Oscillate {
                    shape: MotionShape::Sine,
                    amplitude: f(1.0),
                    period_ms: 10,
                    phase: f(1.0),
                },
                "phase",
            ),
            (
                MotionKind::Drift {
                    rate_per_s: f(-100_001.0),
                    wrap: None,
                },
                "rate_per_s",
            ),
            (
                MotionKind::Drift {
                    rate_per_s: f(-5.0),
                    wrap: Some(f(0.0)),
                },
                "wrap",
            ),
            (
                MotionKind::Impulse {
                    amplitude: f(1.0),
                    frequency_hz: f(60.5),
                    decay_per_s: f(0.0),
                    duration_ms: 10,
                },
                "frequency_hz",
            ),
            (
                MotionKind::Impulse {
                    amplitude: f(1.0),
                    frequency_hz: f(5.0),
                    decay_per_s: f(-1.0),
                    duration_ms: 10,
                },
                "decay_per_s",
            ),
            (
                MotionKind::Impulse {
                    amplitude: f(1.0),
                    frequency_hz: f(5.0),
                    decay_per_s: f(4.0),
                    duration_ms: 0,
                },
                "duration_ms",
            ),
        ];
        for (kind, param) in cases {
            assert_eq!(
                field_of(&[with(kind.clone())]),
                Some(format!("motion.0.{param}")),
                "{kind:?}"
            );
        }
    }

    /// ADR-0100: an `audio` modifier parses with its defaults, hears a bus or a source,
    /// and each of its parameters is checked by name.
    #[test]
    fn an_audio_modifier_takes_defaults_and_checks_each_parameter() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let light = parse(serde_json::json!({
            "modifier_id": "light", "property": "opacity", "kind": "audio",
            "from": -0.6, "to": 0.0
        }));
        let MotionKind::Audio {
            audio,
            audio_source,
            measure,
            floor_db,
            ceiling_db,
            attack_ms,
            release_ms,
            curve,
            ..
        } = &light.kind
        else {
            panic!("{light:?}");
        };
        assert_eq!((audio, audio_source), (&None, &None), "the program mix");
        assert_eq!(*measure, MotionMeasure::Level);
        assert_eq!((floor_db.get(), ceiling_db.get()), (-60.0, 0.0));
        assert_eq!((*attack_ms, *release_ms), (10, 250));
        assert_eq!(*curve, MotionCurve::Linear);
        assert_eq!(field_of(std::slice::from_ref(&light)), None);
        // It writes back what it read, and only what differs from a default is omitted.
        let back = serde_json::to_value(&light).expect("writes");
        assert_eq!(parse(back), light);

        let bass = parse(serde_json::json!({
            "modifier_id": "bass", "property": "scale", "kind": "audio",
            "audio": "music", "measure": "band", "low_hz": 40.0, "high_hz": 120.0,
            "curve": "ease_out", "attack_ms": 0, "release_ms": 400, "to": 0.1
        }));
        assert_eq!(field_of(std::slice::from_ref(&bass)), None);
        let MotionKind::Audio { audio, .. } = &bass.kind else {
            panic!("{bass:?}");
        };
        assert_eq!(audio.as_ref().map(BusId::as_str), Some("music"));

        let field = |json: serde_json::Value| field_of(&[parse(json)]);
        let audio = |extra: serde_json::Value| {
            let mut json = serde_json::json!({
                "modifier_id": "a", "property": "x", "kind": "audio"
            });
            for (k, v) in extra.as_object().expect("object") {
                json[k] = v.clone();
            }
            json
        };
        let cases = [
            (serde_json::json!({ "measure": "band" }), "low_hz"),
            (
                serde_json::json!({ "measure": "band", "low_hz": 10.0, "high_hz": 100.0 }),
                "low_hz",
            ),
            (
                serde_json::json!({ "measure": "band", "low_hz": 100.0 }),
                "high_hz",
            ),
            (
                serde_json::json!({ "measure": "band", "low_hz": 100.0, "high_hz": 100.0 }),
                "high_hz",
            ),
            (
                serde_json::json!({ "measure": "level", "low_hz": 100.0 }),
                "low_hz",
            ),
            (
                serde_json::json!({ "measure": "peak", "high_hz": 100.0 }),
                "high_hz",
            ),
            (serde_json::json!({ "measure": "loudness" }), "measure"),
            (serde_json::json!({ "floor_db": -91.0 }), "floor_db"),
            (serde_json::json!({ "floor_db": 0.0 }), "floor_db"),
            (
                serde_json::json!({ "floor_db": -20.0, "ceiling_db": -30.0 }),
                "ceiling_db",
            ),
            (serde_json::json!({ "ceiling_db": 1.0 }), "ceiling_db"),
            (serde_json::json!({ "attack_ms": 10_001 }), "attack_ms"),
            (serde_json::json!({ "release_ms": 10_001 }), "release_ms"),
            (serde_json::json!({ "curve": "spring" }), "curve"),
            (serde_json::json!({ "from": -100_001.0 }), "from"),
            (serde_json::json!({ "to": 100_001.0 }), "to"),
        ];
        for (extra, param) in cases {
            assert_eq!(
                field(audio(extra.clone())),
                Some(format!("motion.0.{param}")),
                "{extra}"
            );
        }
    }

    /// ADR-0168: a `midi` modifier parses with its defaults, names one control, and
    /// checks each parameter.
    #[test]
    fn a_midi_modifier_takes_defaults_and_checks_each_parameter() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let lever = parse(serde_json::json!({
            "modifier_id": "lever", "property": "tilt", "kind": "midi", "controller": 7
        }));
        assert_eq!(
            lever.kind,
            MotionKind::Midi {
                control: MotionMidiControl::Cc,
                controller: Some(7),
                channel: None,
                input: None,
                smoothing_ms: 50,
                curve: MotionCurve::Linear,
                from: Finite32::ZERO,
                to: Finite32::ONE,
            }
        );
        assert_eq!(check_motion(std::slice::from_ref(&lever)), Ok(()));
        assert_eq!(
            serde_json::to_value(&lever).expect("serializes"),
            serde_json::json!({
                "modifier_id": "lever", "property": "tilt", "kind": "midi", "control": "cc",
                "controller": 7, "smoothing_ms": 50, "curve": "linear", "from": 0.0, "to": 1.0,
                "clock": "engine"
            })
        );
        let instant = parse(serde_json::json!({
            "modifier_id": "w", "property": "rotation", "kind": "midi",
            "control": "pitch_bend", "channel": 16, "input": "Keys", "smoothing_ms": 0
        }));
        assert_eq!(
            check_motion(&[instant]),
            Ok(()),
            "0 ms jumps, and is allowed"
        );

        let field = |json: serde_json::Value| field_of(&[parse(json)]);
        let midi = |extra: serde_json::Value| {
            let mut json = serde_json::json!({
                "modifier_id": "a", "property": "x", "kind": "midi", "controller": 1
            });
            for (k, v) in extra.as_object().expect("object") {
                json[k] = v.clone();
            }
            json
        };
        let cases = [
            (serde_json::json!({ "controller": null }), "controller"),
            (serde_json::json!({ "controller": 128 }), "controller"),
            (serde_json::json!({ "control": "pressure" }), "controller"),
            (serde_json::json!({ "control": "pitch_bend" }), "controller"),
            (serde_json::json!({ "control": "aftertouch" }), "control"),
            (serde_json::json!({ "channel": 0 }), "channel"),
            (serde_json::json!({ "channel": 17 }), "channel"),
            (serde_json::json!({ "input": "" }), "input"),
            (serde_json::json!({ "input": "x".repeat(65) }), "input"),
            (serde_json::json!({ "input": "Pads\n" }), "input"),
            (
                serde_json::json!({ "smoothing_ms": 10_001 }),
                "smoothing_ms",
            ),
            (serde_json::json!({ "curve": "spring" }), "curve"),
            (serde_json::json!({ "from": -100_001.0 }), "from"),
            (serde_json::json!({ "to": 100_001.0 }), "to"),
        ];
        for (extra, param) in cases {
            assert_eq!(
                field(midi(extra.clone())),
                Some(format!("motion.0.{param}")),
                "{extra}"
            );
        }
    }

    /// ADR-0171 decision 3: a `data` modifier parses with its defaults and checks each
    /// parameter.
    /// ADR-0171 decision 5: a data modifier sets a colour setting from its field, as a
    /// `#rrggbb` text or through a palette, and only on an effect's or a source's
    /// setting.
    #[test]
    fn a_data_modifier_sets_a_colour_from_its_field_or_a_palette() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let tint = |extra: serde_json::Value| {
            let mut json = serde_json::json!({
                "modifier_id": "tint", "property": "source", "kind": "data",
                "source": {"param": "color"}, "key": "track/1", "field": "color"
            });
            for (k, v) in extra.as_object().expect("object") {
                json[k] = v.clone();
            }
            parse(json)
        };
        let named = tint(serde_json::json!({ "color_from": "field" }));
        assert!(matches!(
            named.kind,
            MotionKind::Data {
                color_from: Some(MotionColorFrom::Field),
                ..
            }
        ));
        assert_eq!(check_motion(std::slice::from_ref(&named)), Ok(()));
        let picked = tint(serde_json::json!({
            "field": "slots.0.state", "color_from": "palette",
            "palette": { "recording": "#ff2020", "playing": "#20FF40" }
        }));
        assert_eq!(check_motion(std::slice::from_ref(&picked)), Ok(()));
        let wire = serde_json::to_value(&picked).expect("serializes");
        assert_eq!(wire["color_from"], "palette");
        assert_eq!(wire["palette"]["recording"], "#ff2020");
        assert!(
            serde_json::to_value(tint(serde_json::json!({})))
                .expect("serializes")
                .get("color_from")
                .is_none(),
            "omitted when it adds"
        );

        let many: serde_json::Map<String, serde_json::Value> = (0..33)
            .map(|n| (format!("s{n}"), serde_json::json!("#000000")))
            .collect();
        let cases = [
            (
                serde_json::json!({ "color_from": "spectrum" }),
                "color_from",
            ),
            (serde_json::json!({ "color_from": "palette" }), "palette"),
            (
                serde_json::json!({ "palette": { "on": "#ffffff" } }),
                "palette",
            ),
            (
                serde_json::json!({ "color_from": "palette", "palette": many }),
                "palette",
            ),
            (
                serde_json::json!({ "color_from": "palette", "palette": { "": "#ffffff" } }),
                "palette",
            ),
            (
                serde_json::json!({ "color_from": "palette", "palette": { "on": "red" } }),
                "palette",
            ),
            (
                serde_json::json!({ "color_from": "palette", "palette": { "on": "#ffffff80" } }),
                "palette",
            ),
        ];
        for (extra, param) in cases {
            assert_eq!(
                field_of(&[tint(extra.clone())]),
                Some(format!("motion.0.{param}")),
                "{extra}"
            );
        }
        // On a property that isn't a setting there is no colour to set.
        let mut moved = named.clone();
        moved.property = MotionProperty::X;
        moved.source = None;
        assert_eq!(field_of(&[moved]), Some("motion.0.color_from".to_owned()));
    }

    #[test]
    fn a_data_modifier_takes_defaults_and_checks_each_parameter() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let lamp = parse(serde_json::json!({
            "modifier_id": "play", "property": "opacity", "kind": "data",
            "key": "transport", "field": "playing", "from": -1.0, "to": 0.0,
            "smoothing_ms": 0
        }));
        assert_eq!(
            lamp.kind,
            MotionKind::Data {
                input: None,
                key: "transport".into(),
                field: "playing".into(),
                range: [Finite32::ZERO, Finite32::ONE],
                states: BTreeMap::new(),
                smoothing_ms: 0,
                curve: MotionCurve::Linear,
                from: Finite32::new(-1.0).expect("finite"),
                to: Finite32::ZERO,
                color_from: None,
                palette: BTreeMap::new(),
            }
        );
        assert_eq!(check_motion(std::slice::from_ref(&lamp)), Ok(()));
        assert_eq!(
            serde_json::to_value(&lamp).expect("serializes"),
            serde_json::json!({
                "modifier_id": "play", "property": "opacity", "kind": "data",
                "key": "transport", "field": "playing", "range": [0.0, 1.0],
                "smoothing_ms": 0, "curve": "linear", "from": -1.0, "to": 0.0,
                "clock": "engine"
            })
        );

        let field = |json: serde_json::Value| field_of(&[parse(json)]);
        let data = |extra: serde_json::Value| {
            let mut json = serde_json::json!({
                "modifier_id": "a", "property": "x", "kind": "data", "key": "meters"
            });
            for (k, v) in extra.as_object().expect("object") {
                json[k] = v.clone();
            }
            json
        };
        let many: serde_json::Map<String, serde_json::Value> = (0..33)
            .map(|n| (format!("s{n}"), serde_json::json!(1.0)))
            .collect();
        let cases = [
            (serde_json::json!({ "input": "" }), "input"),
            (serde_json::json!({ "input": "x".repeat(65) }), "input"),
            (serde_json::json!({ "key": "" }), "key"),
            (serde_json::json!({ "key": "k".repeat(64) }), "key"),
            (serde_json::json!({ "key": "a\tb" }), "key"),
            (serde_json::json!({ "field": "f".repeat(256) }), "field"),
            (serde_json::json!({ "range": [0.5, 0.5] }), "range"),
            (serde_json::json!({ "range": [0.0, 100_001.0] }), "range"),
            (serde_json::json!({ "states": many }), "states"),
            (serde_json::json!({ "states": { "": 1.0 } }), "states"),
            (serde_json::json!({ "states": { "on": 1.5 } }), "states"),
            (
                serde_json::json!({ "smoothing_ms": 10_001 }),
                "smoothing_ms",
            ),
            (serde_json::json!({ "curve": "spring" }), "curve"),
            (serde_json::json!({ "from": -100_001.0 }), "from"),
            (serde_json::json!({ "to": 100_001.0 }), "to"),
        ];
        for (extra, param) in cases {
            assert_eq!(
                field(data(extra.clone())),
                Some(format!("motion.0.{param}")),
                "{extra}"
            );
        }
        // An inverted range, a text table and every default but the key are fine.
        assert_eq!(
            field(data(serde_json::json!({
                "range": [-60.0, 6.0], "states": { "recording": 1.0, "stopped": 0.0 }
            }))),
            None
        );
    }
}
