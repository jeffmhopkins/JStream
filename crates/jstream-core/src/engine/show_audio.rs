//! A running show's audio tracks (ADR-0210 decision 3): each audio clip brings its
//! source up for its span, and the show moves the source's fader through the clip's
//! fades.
//!
//! The runner says when a clip comes up and goes ([`Effect::AudioOn`],
//! [`Effect::AudioOff`]); this file does the rest:
//! - **Heard and played.** A source whose audio follows `when_shown` (a webcam's
//!   microphone, a media file, a nested scene) or that is a media file gets an item of
//!   the internal show scene, at opacity 0, shown while a clip of it is up. The show
//!   scene is drawn over program, so the source is shown on the Program mix (heard) and
//!   the plan reaches it (a media file plays, from its start when it comes up). A clip
//!   brought up part way in, or again across a jump or a wrap, seeks the source to its
//!   place, as `source.seek` does. An `always` source needs no item.
//! - **The fader.** While a show runs, every source on its audio tracks is the show's to
//!   move: up to the clip's level and down again over its fades while a clip of it is up
//!   ([`AudioClip::gain_db_at`]), at the floor (−100 dB) while none is, and down over a
//!   short fade when a jump, a wrap or a stop cuts a clip off. The levels are live values
//!   the collection is drawn and heard with (`Graph::with_values`, ADR-0213), under a
//!   live drag's and the value bindings', so the collection's own fader is never written.
//! - **A hand takes over.** A source whose fader a person sets while the show drives it
//!   is left alone until its next clip comes up (latest wins).
//!
//! When the show stops, the clips it had up fade down, then the sources go back to their
//! own faders. A show rehearsed on preview moves no fader and puts no source up.
//!
//! Docs: docs/adr/0210-shows-a-run-order-with-bookmarks.md

use std::collections::BTreeMap;
use std::task::Waker;
use std::time::{Duration, Instant};

use jstream_anim::show::{AUDIO_FLOOR_DB, fade_db};
use jstream_protocol::{AudioFollow, Edit, Finite32, ItemId, SourceId, SourceKind};

use super::State;

/// How often the fader moves while a fade runs.
const FADE_STEP: Duration = Duration::from_millis(33);

/// The prefix of the show scene's items that put an audio clip's source up.
const ITEM_PREFIX: &str = "show-audio";

/// The show's hold on the faders of the sources on its audio tracks.
#[derive(Debug, Default)]
pub(super) struct ShowAudio {
    /// Each source the show drives.
    sources: BTreeMap<SourceId, Driven>,
    /// The levels it last drew the collection with.
    pub(super) edits: Vec<Edit>,
}

/// One source the show drives.
#[derive(Debug)]
struct Driven {
    /// The collection's own fader when the show last took it: one that differs now was
    /// moved by hand.
    base_db: Option<f32>,
    /// A hand moved its fader: the show leaves it until its next clip comes up.
    hand: bool,
    /// A fade down in progress, after a clip went.
    fading: Option<Fade>,
    /// The level it set last.
    level_db: f32,
}

/// A fade down to the floor, on the animation clock.
#[derive(Debug, Clone, Copy)]
struct Fade {
    from_db: f32,
    start_ns: i64,
    length_ns: i64,
}

impl Fade {
    /// The level at `now_ns`, or `None` once it is done.
    fn at(&self, now_ns: i64) -> Option<f32> {
        let gone = now_ns.saturating_sub(self.start_ns).max(0);
        if gone >= self.length_ns {
            return None;
        }
        let fraction = gone as f32 / self.length_ns as f32;
        Some(fade_db(self.from_db, AUDIO_FLOOR_DB, fraction))
    }
}

/// The collection's fader of `source_id`, in dB, if it is an audio source.
fn fader(state: &jstream_graph::State, source_id: &SourceId) -> Option<f32> {
    Some(state.source(source_id)?.audio()?.gain_db.get())
}

impl State {
    /// [`Effect::AudioOn`](jstream_anim::show::Effect::AudioOn): puts `source_id` up
    /// `offset_ms` into its clip.
    pub(super) fn show_audio_on(
        &mut self,
        source_id: &SourceId,
        offset_ms: u32,
        wake: &mut Vec<Waker>,
    ) {
        let base_db = fader(self.graph.state(), source_id);
        let level_db = self
            .show_audio
            .sources
            .get(source_id)
            .map_or(AUDIO_FLOOR_DB, |d| d.level_db);
        self.show_audio.sources.insert(
            source_id.clone(),
            Driven {
                base_db,
                hand: false,
                fading: None,
                level_db,
            },
        );
        let Some(item_id) = self.audio_item(source_id, wake) else {
            return;
        };
        let state = self.graph.state();
        let was_up = state.visible_in(&item_id, state.show_scene()) == Some(true);
        let _ = self.set_show_scene_item_shown(&item_id, true, Some(0), wake);
        // A source that comes up afresh starts from its start; one that comes up part
        // way in, or again across a jump or a wrap, seeks to its place.
        if offset_ms > 0 || was_up {
            let to = jstream_media::MediaTime::new(
                i64::from(offset_ms),
                jstream_media::Timebase::MILLIS,
            );
            self.live.seek(source_id, self.action_now(), to);
        }
    }

    /// [`Effect::AudioOff`](jstream_anim::show::Effect::AudioOff): takes `source_id`
    /// down, over `fade_ms` from where its fader is.
    pub(super) fn show_audio_off(
        &mut self,
        source_id: &SourceId,
        fade_ms: u32,
        wake: &mut Vec<Waker>,
    ) {
        if !self.show_audio.sources.contains_key(source_id) {
            return;
        }
        let start_ns = crate::timelines::ns(self.action_now());
        let Some(driven) = self.show_audio.sources.get_mut(source_id) else {
            return;
        };
        driven.fading = (fade_ms > 0).then(|| Fade {
            from_db: driven.level_db,
            start_ns,
            length_ns: i64::from(fade_ms).saturating_mul(1_000_000),
        });
        if driven.fading.is_none() {
            self.hide_audio_item(source_id, wake);
        } else {
            self.start_ticker();
        }
    }

    /// Moves the faders the running show drives to where they are now, hides the items
    /// of fades that finished, and draws the collection with the new levels. Returns
    /// when it next needs to: while a fader moves, a frame of the fade from now.
    pub(super) fn show_audio_tick(&mut self, wake: &mut Vec<Waker>) -> Option<Instant> {
        let now_ns = self.show_now();
        let plan = self.show_plan();
        // A rehearsal runs on preview and leaves what is heard alone.
        let running = self
            .show
            .as_ref()
            .filter(|run| !run.rehearse)
            .zip(plan.as_ref());
        // The clips up, by source, and how far into each.
        let mut up: BTreeMap<SourceId, f32> = BTreeMap::new();
        let mut moving = false;
        // When the next fade out starts, in show-time milliseconds from now.
        let mut next_fade: Option<f32> = None;
        if let Some((run, plan)) = running {
            let at = run.runner.position_ms(now_ns) as f32;
            let playing = run.runner.status() == jstream_anim::show::Status::Playing;
            for &clip in &run.runner.applied().audio {
                let Some(c) = plan.audio.get(clip) else {
                    continue;
                };
                let offset = at - c.start_ms as f32;
                up.insert(c.source.clone(), c.gain_db_at(offset));
                let fade_out_at = c.length_ms.saturating_sub(c.fade_out_ms) as f32;
                let in_fade = offset < c.fade_in_ms as f32 || offset >= fade_out_at;
                moving |= in_fade && playing;
                if playing && c.fade_out_ms > 0 && offset < fade_out_at {
                    let wait = fade_out_at - offset;
                    next_fade = Some(next_fade.map_or(wait, |n: f32| n.min(wait)));
                }
            }
            // Every source on an audio track is the show's while it runs.
            for c in &plan.audio {
                if !self.show_audio.sources.contains_key(&c.source) {
                    let base_db = fader(self.graph.state(), &c.source);
                    self.show_audio.sources.insert(
                        c.source.clone(),
                        Driven {
                            base_db,
                            hand: false,
                            fading: None,
                            level_db: AUDIO_FLOOR_DB,
                        },
                    );
                }
            }
        }
        let state = self.graph.state();
        let mut finished = Vec::new();
        let mut gone = Vec::new();
        let mut edits = Vec::new();
        for (source_id, driven) in &mut self.show_audio.sources {
            let fade = driven.fading.and_then(|f| f.at(now_ns));
            if driven.fading.is_some() && fade.is_none() {
                driven.fading = None;
                finished.push(source_id.clone());
            }
            // A person set the fader since the show took it: theirs until the next clip.
            if fader(state, source_id) != driven.base_db {
                driven.hand = true;
            }
            let level = match (up.get(source_id), fade) {
                (Some(&level), _) => level,
                (None, Some(level)) => {
                    moving = true;
                    level
                }
                // A stopped show lets go of a source once its fade is done.
                (None, None) if running.is_none() => {
                    gone.push(source_id.clone());
                    continue;
                }
                (None, None) => AUDIO_FLOOR_DB,
            };
            driven.level_db = level;
            if driven.hand || state.source(source_id).and_then(|s| s.audio()).is_none() {
                continue;
            }
            edits.push(Edit::SourceSetAudio {
                source_id: source_id.clone(),
                enabled: None,
                follow: None,
                gain_db: Finite32::new(level),
                muted: None,
            });
        }
        for source_id in &gone {
            self.show_audio.sources.remove(source_id);
        }
        for source_id in finished.iter().filter(|s| !up.contains_key(*s)) {
            self.hide_audio_item(source_id, wake);
        }
        if edits != self.show_audio.edits {
            self.show_audio.edits = edits;
            if self.refresh_shown() {
                self.redraw();
            }
        }
        let now = Instant::now();
        let fade_out = next_fade.map(|ms| now + Duration::from_secs_f32(ms.max(0.0) / 1000.0));
        [moving.then(|| now + FADE_STEP), fade_out]
            .into_iter()
            .flatten()
            .min()
    }

    /// Hides `source_id`'s item of the show scene, if it has one up.
    fn hide_audio_item(&mut self, source_id: &SourceId, wake: &mut Vec<Waker>) {
        if let Some(item_id) = self.find_audio_item(source_id) {
            let _ = self.set_show_scene_item_shown(&item_id, false, Some(0), wake);
        }
    }

    /// Whether an item of the show scene is one that puts an audio clip's source up,
    /// not an overlay.
    pub(super) fn is_audio_item(item_id: &ItemId) -> bool {
        item_id.as_str().starts_with(ITEM_PREFIX)
    }

    /// The show scene's item that puts `source_id` up, if there is one.
    fn find_audio_item(&self, source_id: &SourceId) -> Option<ItemId> {
        let state = self.graph.state();
        let show = state.scene(state.show_scene()?)?;
        show.items()
            .iter()
            .find(|item| Self::is_audio_item(item.id()) && item.source_id() == source_id)
            .map(|item| item.id().clone())
    }

    /// The show scene's item that puts `source_id` up, made (hidden, at opacity 0) if
    /// there is none, as one batch of the engine's own, as an overlay's is. `None` for a
    /// source that needs none (its audio follows `always` and it isn't a media file) or
    /// can't be placed.
    fn audio_item(&mut self, source_id: &SourceId, wake: &mut Vec<Waker>) -> Option<ItemId> {
        if let Some(item) = self.find_audio_item(source_id) {
            return Some(item);
        }
        let state = self.graph.state();
        let source = state.source(source_id)?;
        let media = matches!(source.kind(), SourceKind::Builtin { kind } if kind == "media");
        let when_shown = source
            .audio()
            .is_some_and(|audio| audio.follow == AudioFollow::WhenShown);
        if !media && !when_shown {
            return None;
        }
        let item_id = ItemId::new(
            (1u32..)
                .map(|n| format!("{ITEM_PREFIX}-{n}"))
                .find(|id| state.item(&ItemId::new(id)).is_none())
                .unwrap_or_default(),
        );
        let mut edits = Vec::new();
        let show_scene = self.show_scene_for_items(&mut edits);
        edits.push(Edit::ItemAdd {
            scene_id: show_scene.clone(),
            item_id: item_id.clone(),
            source_id: source_id.clone(),
            transform: None,
            geometry: None,
            opacity: Finite32::new(0.0),
            blend: None,
        });
        edits.push(Edit::ItemSetVisible {
            item_id: item_id.clone(),
            scene_id: None,
            visible: false,
        });
        self.apply_show_scene_edits(show_scene, &edits, wake)
            .then_some(item_id)
    }
}

impl ShowAudio {
    /// Whether the show drives any fader now.
    pub(super) fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}
