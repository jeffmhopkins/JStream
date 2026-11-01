//! Other pictures on the canvas's binary channel. A session has one binary channel, and
//! the canvas reads it; a dialog that shows a picture of its own (the Properties
//! dialog's device preview) opens its stream on a channel number from [`Taps::take`]
//! and registers a handler, and the canvas hands it every frame of that channel.
//! Tap channels start at [`FIRST`], far above the canvas's own, which count up from
//! [`CHANNEL`](super::preview::CHANNEL) one per scene shown.
//!
//! Docs: crates/jstream-ui/README.md#canvas

use jstream_control::protocol::BinaryFrame;
use leptos::prelude::{GetValue, LocalStorage, StoredValue};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// The first channel a tap takes.
pub const FIRST: u32 = 1 << 31;

/// What a tap does with each frame of its channel.
pub type Handler = Box<dyn FnMut(&BinaryFrame)>;

/// The page's taps, shared by the canvas (which delivers) and the dialogs (which tap).
#[derive(Clone)]
pub struct Taps {
    handlers: Rc<RefCell<HashMap<u32, Handler>>>,
    next: Rc<Cell<u32>>,
}

impl Default for Taps {
    fn default() -> Taps {
        Taps {
            handlers: Rc::default(),
            next: Rc::new(Cell::new(FIRST)),
        }
    }
}

impl Taps {
    /// A channel number no tap of this page has used: a stream's channel may only be
    /// reused once it is closed, and a late frame of a closed one must not reach the
    /// next.
    pub fn take(&self) -> u32 {
        let channel = self.next.get();
        self.next.set(channel.wrapping_add(1).max(FIRST));
        channel
    }

    /// Hands the frames of `channel` to `handler`, until [`Self::remove`].
    pub fn insert(&self, channel: u32, handler: Handler) {
        self.handlers.borrow_mut().insert(channel, handler);
    }

    /// Stops handing `channel`'s frames on.
    pub fn remove(&self, channel: u32) {
        self.handlers.borrow_mut().remove(&channel);
    }

    /// Hands `frame` to its channel's handler, if one is registered, and returns
    /// whether one was. The handler is taken out while it runs, so it may add or remove
    /// a tap; an end frame removes it.
    pub fn deliver(&self, frame: &BinaryFrame) -> bool {
        let channel = frame.header.channel;
        let handler = self.handlers.borrow_mut().remove(&channel);
        let Some(mut handler) = handler else {
            return false;
        };
        handler(frame);
        if !frame.header.end {
            self.handlers.borrow_mut().entry(channel).or_insert(handler);
        }
        true
    }
}

/// The page's [`Taps`], provided as context by the shell (`app.rs`).
#[derive(Clone, Copy)]
pub struct PreviewTaps(pub StoredValue<Taps, LocalStorage>);

impl PreviewTaps {
    /// No taps yet.
    pub fn new() -> PreviewTaps {
        PreviewTaps(StoredValue::new_local(Taps::default()))
    }

    /// The taps, while the page has them: a clone shares them.
    pub fn get(&self) -> Option<Taps> {
        self.0.try_get_value()
    }
}

impl Default for PreviewTaps {
    fn default() -> PreviewTaps {
        PreviewTaps::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{FIRST, Taps};
    use jstream_control::protocol::Rev;
    use jstream_control::protocol::{BinaryFrame, StreamKind};
    use std::cell::Cell;
    use std::rc::Rc;

    fn frame(channel: u32, end: bool) -> BinaryFrame {
        if end {
            BinaryFrame::end(StreamKind::Preview, channel, Rev(1), "gone")
        } else {
            let mut frame = BinaryFrame::end(StreamKind::Preview, channel, Rev(1), "");
            frame.header.end = false;
            frame
        }
    }

    #[test]
    fn taps_take_channels_above_the_canvas_and_never_reuse_one() {
        let taps = Taps::default();
        assert_eq!(taps.take(), FIRST);
        assert_eq!(taps.take(), FIRST + 1);
    }

    #[test]
    fn a_frame_reaches_its_tap_until_the_tap_or_the_stream_ends() {
        let taps = Taps::default();
        let seen = Rc::new(Cell::new(0));
        let count = Rc::clone(&seen);
        taps.insert(FIRST, Box::new(move |_| count.set(count.get() + 1)));
        assert!(taps.deliver(&frame(FIRST, false)));
        assert!(!taps.deliver(&frame(FIRST + 1, false)), "no tap there");
        assert!(taps.deliver(&frame(FIRST, true)));
        assert_eq!(seen.get(), 2);
        assert!(
            !taps.deliver(&frame(FIRST, false)),
            "the end frame removed it"
        );

        let count = Rc::clone(&seen);
        taps.insert(FIRST + 2, Box::new(move |_| count.set(count.get() + 1)));
        taps.remove(FIRST + 2);
        assert!(!taps.deliver(&frame(FIRST + 2, false)));
    }
}
