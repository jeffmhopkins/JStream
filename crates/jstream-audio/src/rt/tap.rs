//! Output taps (docs/adr/0080-program-master-and-output-chains-v2.md): an output's view of
//! one bus after the master chain, through the output's own audio chain.

use super::chain::Chain;

/// Output taps the mixer can hold without growing its list on the mixer thread.
pub(crate) const MAX_OUTPUT_TAPS: usize = 16;

/// One output tap. Built on the control thread, block buffer included; owned by the
/// mixer thread once installed. Small: it moves by value, and its heap parts go to the
/// collector with it.
#[derive(Debug)]
pub(crate) struct Tap {
    pub(crate) id: u64,
    /// The bus slot it reads, and the bus it was added for: a tap whose bus is gone
    /// reads silence.
    pub(crate) slot: usize,
    pub(crate) bus: u64,
    pub(crate) chain: Box<Chain>,
    /// The last block, after the tap's chain: `block_frames` frames.
    pub(crate) block: Box<[[f32; 2]]>,
    /// The last block's presentation time, engine frames; `None` until the tap has
    /// run once.
    pub(crate) pts: Option<i64>,
}

impl Tap {
    pub(crate) fn new(
        id: u64,
        slot: usize,
        bus: u64,
        chain: Box<Chain>,
        block_frames: usize,
    ) -> Self {
        Self {
            id,
            slot,
            bus,
            chain,
            block: vec![[0.0; 2]; block_frames].into_boxed_slice(),
            pts: None,
        }
    }
}
