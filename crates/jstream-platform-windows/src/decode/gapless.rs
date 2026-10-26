//! Gapless audio: a decoded stream cut to its content, without the encoder's priming
//! at the start or its padding at the end (#445).
//!
//! An AAC or MP3 encoder adds frames of priming before the first real sample and pads
//! the last packet out to a whole one. The container says how many (an MP4's edit list
//! or `iTunSMPB`, an MP3's LAME tag), but the Source Reader hands both on decoded, as
//! near-silence: a looping file then has a 25-40 ms hole at each wrap. This drops them.
//! Frames are counted at the stream's rate, from its start as decoded.

/// Frames of priming an AAC-LC encoder adds when the file doesn't say: the decoder's
/// one-frame delay, as FFmpeg's encoder writes it (Apple's adds 2112, and writes
/// `iTunSMPB` saying so).
pub(super) const AAC_PRIMING: u32 = 1024;

/// The frame at `hns` (100 ns units) in a stream of `rate` frames a second, to the
/// nearest.
pub(super) fn frames_at(hns: i64, rate: u32) -> i64 {
    let scaled = i128::from(hns) * i128::from(rate);
    let half = if scaled < 0 { -5_000_000 } else { 5_000_000 };
    i64::try_from((scaled + half) / 10_000_000).unwrap_or(i64::MAX)
}

/// One decoded block: where its first frame falls in the stream as decoded, and its
/// interleaved samples.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Block {
    pub(super) start: i64,
    pub(super) samples: Vec<f32>,
}

/// Cuts each block of one stream to its content, renumbered so the content's first
/// frame is 0. With padding to drop it holds one block back, since only the end of the
/// stream says which block is the last.
#[derive(Debug)]
pub(super) struct Gapless {
    channels: usize,
    /// Priming frames at the start.
    delay: i64,
    /// Padding frames at the end.
    padding: i64,
    /// Where the content ends, from the container's duration, when it says.
    end: Option<i64>,
    /// The block held back until the next one shows it isn't the last.
    held: Option<Block>,
}

impl Gapless {
    /// `delay` frames of priming and `padding` frames of padding, in a stream of
    /// `channels` channels whose content lasts `end` frames when the container says.
    pub(super) fn new(channels: usize, delay: u32, padding: u32, end: Option<i64>) -> Self {
        Self {
            channels: channels.max(1),
            delay: i64::from(delay),
            padding: i64::from(padding),
            end: end.filter(|&end| end > 0),
            held: None,
        }
    }

    /// Cuts `delay` frames of priming from here on.
    pub(super) fn set_delay(&mut self, delay: u32) {
        self.delay = i64::from(delay);
    }

    /// Forgets the block held back, for a seek.
    pub(super) fn reset(&mut self) {
        self.held = None;
    }

    /// Takes the next block decoded, and returns the one before it, cut, when there is
    /// one with anything left; with no padding to drop, `block` itself.
    pub(super) fn push(&mut self, block: Block) -> Option<Block> {
        if self.padding == 0 {
            return self.cut(block, None);
        }
        let out = self.held.replace(block)?;
        self.cut(out, None)
    }

    /// The stream ended: the block held back, cut and without the padding.
    pub(super) fn finish(&mut self) -> Option<Block> {
        let block = self.held.take()?;
        let frames = self.frames(&block);
        let last = block.start + frames - self.padding;
        self.cut(block, Some(last))
    }

    fn frames(&self, block: &Block) -> i64 {
        i64::try_from(block.samples.len() / self.channels).unwrap_or(i64::MAX)
    }

    /// `block` on the content's numbering, without what lies before its start or at or
    /// after its end (`last`, as decoded, and the container's). `None` when nothing is
    /// left.
    fn cut(&self, block: Block, last: Option<i64>) -> Option<Block> {
        let frames = self.frames(&block);
        let start = block.start - self.delay;
        let mut end = start + frames;
        if let Some(last) = last {
            end = end.min(last - self.delay);
        }
        if let Some(content) = self.end {
            end = end.min(content);
        }
        let from = start.max(0);
        if end <= from {
            return None;
        }
        let skip = usize::try_from(from - start).unwrap_or(0) * self.channels;
        let keep = usize::try_from(end - from).unwrap_or(0) * self.channels;
        let mut samples = block.samples;
        samples.truncate(skip + keep);
        samples.drain(..skip);
        Some(Block {
            start: from,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono block of `frames` frames from `start`, each sample its own frame number.
    fn block(start: i64, frames: i64) -> Block {
        Block {
            start,
            samples: (start..start + frames).map(|f| f as f32).collect(),
        }
    }

    /// The issue's `audio.m4a`: 5 s of 44.1 kHz mono from FFmpeg, 1024 frames of
    /// priming, 220,500 of content and 684 of padding in 217 AAC frames. Cut, the
    /// content runs from 0 to its last frame and nothing more, so a loop joins on.
    #[test]
    fn priming_and_padding_are_dropped() {
        let content = 220_500;
        for (padding, end) in [(684, None), (0, Some(content))] {
            let mut gapless = Gapless::new(1, AAC_PRIMING, padding, end);
            let mut out: Vec<Block> = (0..217)
                .filter_map(|i| gapless.push(block(i * 1024, 1024)))
                .collect();
            out.extend(gapless.finish());
            let first = out.first().expect("a block");
            assert_eq!(first.start, 0);
            assert_eq!(
                first.samples[0], 1024.0,
                "the first frame after the priming"
            );
            let mut next = 0;
            for b in &out {
                assert_eq!(b.start, next, "blocks follow on");
                next += i64::try_from(b.samples.len()).expect("a length");
            }
            assert_eq!(next, content, "padding {padding}, end {end:?}");
            let last = out.last().expect("a block");
            assert_eq!(last.samples.last(), Some(&(1024.0 + 220_499.0)));
        }
    }

    #[test]
    fn a_block_inside_the_priming_is_dropped_and_one_across_it_is_cut() {
        let mut gapless = Gapless::new(2, 1500, 0, None);
        let stereo = |start: i64, frames: usize| Block {
            start,
            samples: vec![0.5; frames * 2],
        };
        assert_eq!(gapless.push(stereo(0, 1024)), None);
        let cut = gapless.push(stereo(1024, 1024)).expect("the rest of it");
        assert_eq!((cut.start, cut.samples.len()), (0, 548 * 2));
        let whole = gapless.push(stereo(2048, 1024)).expect("a whole block");
        assert_eq!((whole.start, whole.samples.len()), (548, 2048));
    }

    #[test]
    fn times_are_counted_in_frames() {
        // 1024 frames at 44.1 kHz is 232,199.5 ticks, which the reader rounds either way.
        assert_eq!(frames_at(232_199, 44_100), 1024);
        assert_eq!(frames_at(232_200, 44_100), 1024);
        assert_eq!(frames_at(0, 48_000), 0);
        assert_eq!(frames_at(-213_333, 48_000), -1024);
    }

    #[test]
    fn a_seek_forgets_the_block_held_back() {
        let mut gapless = Gapless::new(1, 0, 100, None);
        assert_eq!(gapless.push(block(0, 1024)), None);
        gapless.reset();
        assert_eq!(gapless.push(block(5000, 1024)), None, "nothing from before");
        let last = gapless.finish().expect("the last block");
        assert_eq!((last.start, last.samples.len()), (5000, 924));
    }

    #[test]
    fn a_new_delay_applies_from_the_next_pass() {
        let mut gapless = Gapless::new(1, 1500, 0, None);
        assert_eq!(
            gapless.push(block(0, 1024)),
            None,
            "inside the first pass's priming"
        );
        gapless.reset();
        gapless.set_delay(500);
        let cut = gapless.push(block(0, 1024)).expect("past the new priming");
        assert_eq!(cut.start, 0);
        assert_eq!(cut.samples.first().copied(), Some(500.0));
        assert_eq!(cut.samples.len(), 524);
    }
}
