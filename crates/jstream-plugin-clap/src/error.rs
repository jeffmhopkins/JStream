//! The crate's error type.

/// Why loading, creating, activating or running a CLAP plugin failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClapError {
    /// The plugin file could not be loaded as a native library.
    #[error("could not load the plugin file: {0}")]
    Load(String),
    /// The library exports no `clap_entry`.
    #[error("the file is not a CLAP plugin: it has no clap_entry")]
    NoEntry,
    /// The plugin was built against a CLAP major version this host doesn't speak.
    #[error("the plugin's CLAP version {major}.{minor} is not supported")]
    Version {
        /// The plugin's CLAP major version.
        major: u32,
        /// The plugin's CLAP minor version.
        minor: u32,
    },
    /// The entry's `init` returned false.
    #[error("the plugin file failed to initialise")]
    EntryInit,
    /// The file offers no plugin factory.
    #[error("the plugin file has no plugin factory")]
    NoFactory,
    /// The file has no plugin with this id.
    #[error("the plugin file has no plugin with id {0:?}")]
    NoSuchPlugin(String),
    /// The plugin isn't an audio effect (an instrument or a note effect, say), which
    /// ADR-0035 leaves out.
    #[error("the plugin is not an audio effect")]
    NotAnEffect,
    /// The factory returned no plugin, or the plugin's `init` returned false.
    #[error("the plugin could not be created")]
    Create,
    /// The plugin has no main input and main output of two channels each.
    #[error("the plugin has no stereo main input and output")]
    NotStereo,
    /// The plugin refused to activate at this rate and block size.
    #[error("the plugin refused to activate")]
    Activate,
    /// The call needs the plugin active, or inactive, and it isn't.
    #[error("the plugin is {0}")]
    WrongState(&'static str),
    /// The plugin's [`Processor`](crate::Processor) still exists, so it can't be
    /// deactivated.
    #[error("the plugin's processor still exists")]
    Busy,
    /// The plugin doesn't support saving and loading its state.
    #[error("the plugin has no saved state")]
    NoState,
    /// The plugin's saved state, or the state given to it, is over
    /// [`MAX_STATE_BYTES`](crate::MAX_STATE_BYTES).
    #[error("the plugin's saved state is over 2 MiB")]
    StateTooLarge,
    /// The plugin failed to save or load its state.
    #[error("the plugin could not save or load its state")]
    State,
    /// A block's length was odd, zero, uneven between input and output, or over the
    /// block size the plugin was activated with.
    #[error("block of {frames} frames does not fit the plugin's 1..={max} frames")]
    BlockSize {
        /// The block's frames.
        frames: usize,
        /// The most the plugin was activated for.
        max: usize,
    },
    /// The plugin refused to start processing, or reported a processing error.
    #[error("the plugin failed to process")]
    Process,
}
