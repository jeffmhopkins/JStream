//! Shows (alpha.13 show timeline): a running order of scenes in show time, with pausing
//! bookmarks that wait for Continue, loops, overlays, keyframe markers, sequence clips
//! and one-off events. This module is the pure schedule: what a show puts up at any point
//! of show time ([`spanning`]) and the state machine that walks it ([`Runner`]). The
//! engine instantiates it with its protocol types through [`ShowKeys`] and runs the
//! [`Effect`]s it returns, as it runs a timeline's cues.
//!
//! # Show time
//!
//! Show time is in milliseconds from the show's start. It runs with the animation clock
//! while the show plays and stops while it is paused. Every placed thing sits on a whole
//! millisecond, so the runner walks *points*: the times at which something starts, ends
//! or fires.
//!
//! # Spanning things and events
//!
//! A program clip, an overlay clip and a sequence clip *span* `[start, start + length)`.
//! A keyframe marker spans from its time to the end of the program clip it sits in. What
//! spans a point is worked out by [`resolve::spanning`], and the runner puts it up by
//! diffing that against what it last put up ([`Applied`]), so a jump, a loop wrap and
//! plain playing all go through one path. An event fires once, at its time, when playing
//! reaches it. A state event (an action with a [`CueAction::state_key`](crate::CueAction::state_key)) is also state: a
//! jump re-applies the last one per key before the landing point; a one-shot is never
//! replayed.
//!
//! # Points and pausing
//!
//! At a point `p`, in this order:
//! 1. a loop the play head is in, and is not leaving, that ends at `p` wraps to its start
//!    (so a bookmark at a loop's end is outside it, and never pauses a pass); a sequence
//!    clip that ended at `p` stops, and plays again if it spans the loop's start;
//! 2. a pausing bookmark at `p` pauses the show *before* anything at `p`: whatever sits at
//!    `p` happens on Continue. A bookmark the show just resumed from, or a loop's start
//!    it just wrapped to, does not pause again;
//! 3. the spanning state at `p` is put up (program cuts use the incoming clip's
//!    transition);
//! 4. the events at `p` fire, in their order;
//! 5. reaching the show's length ends it ([`EndOfShow`]).
//!
//! # Landing
//!
//! A jump to `t` lands one of two ways. A *pausing* landing (a pausing bookmark, Back,
//! Skip) puts up the state *just before* `t` ([`Edge::Before`]) and pauses at `t`; things
//! at `t` happen on Continue, exactly as playing there would have left it. A *play-on*
//! landing (a non-pausing bookmark, early Continue, an armed spot) puts up the state *at*
//! `t` ([`Edge::At`]) and plays on, firing the events at `t`. Either way the state events
//! before `t` are re-applied, last per key, and only where the runner's own last value for
//! that key differs, so anything set by hand on a key the show never set, or set again by
//! hand after the show set it, is left alone. A program cut caused by a landing uses the
//! bookmark's arrive transition, else the show's default.
//!
//! # Pause by hand
//!
//! [`Runner::pause`] holds show time where the play head is, at a point or between two,
//! and stops the sequences the show started. Continue, or [`Runner::unpause`], plays on
//! from there: the state at the held point is put up as a play-on landing would put it
//! up, so the sequences play again from where they were. Program runs on through a
//! pause, so its clip is not sought back.
//!
//! **Jump equals play:** after landing at `t`, what the show owns (program clip and its
//! media position, keyframe, overlays, sequences and their positions, state-event values)
//! equals what playing from the start to `t` leaves. Property tests hold this.

mod plan;
mod resolve;
mod runner;
#[cfg(test)]
mod tests;

pub use plan::{
    Bookmark, EndOfShow, InvalidPlan, KeyframeMark, Leave, LoopRegion, OverlayClip, Plan,
    ProgramClip, SequenceClip, ShowEvent, ShowKeys,
};
pub use resolve::{Edge, Spanning, spanning, state_events_before};
pub use runner::{Applied, Effect, Landing, Next, Runner, Status, Step};
