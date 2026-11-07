//! Shader sources (ADR-0093): a program of fullscreen passes that makes a picture of its
//! own, drawn through the filter harness so the chain runtime keeps it, updates it in
//! place and retires it like any filter. Its input (the source's stand-in frame) is not
//! read: the program's last pass is the picture, at the size the program reports.
//!
//! The pack side ([`ExternalProgram`], from `jstream-effects`) supplies each pass's WGSL
//! (its generated header and the pack's files), the names it reads as numbered inputs,
//! and the pack's decoded images. This side owns the bindings, the pipelines and the
//! layers: each pass draws into a scratch layer of its size, or, when a pass reads it
//! back as feedback, into one of two layers it keeps and swaps every frame.
//!
//! A pass reads, through its inputs: another pass (listed earlier, this frame's picture;
//! the pass itself or one listed later, the frame before's), one of the pack's images, or
//! a slot the chain runtime fills ([`super::VideoFilter::set_input`]: a scene's
//! composite, the audio data image). An empty slot reads transparent.

use std::sync::Arc;

use jstream_protocol::Settings;

use crate::error::RenderError;
use crate::filter::pass::{Ctx, ShaderRef};
use crate::image::{self, Image};

/// The WGSL every shader-source pass starts with: the vertex stage, `sys`, the samplers,
/// and the colour and noise helpers. Shader-source packs validate against it.
pub const PROGRAM_PRELUDE: &str = concat!(
    include_str!("prelude.wgsl"),
    "\n",
    include_str!("../colour.wgsl"),
    "\n",
    include_str!("noise.wgsl")
);

/// The most passes a program has.
pub const MAX_PASSES: usize = 8;

/// The most inputs one pass reads.
pub const MAX_INPUTS: usize = 12;

/// The binding of a program's `Params` uniform, at group 0.
pub const PROGRAM_PARAMS_BINDING: u32 = 3;

/// The binding of a pass's first input texture, at group 0; input `i` is at this plus `i`.
pub const FIRST_INPUT_BINDING: u32 = 4;

/// The most GPU memory one program's layers may take: its passes' layers (two for a
/// pass read back as feedback) at its size, as `Rgba16Float`. A program over it draws
/// nothing (ADR-0093): eight full-size passes at 4096 by 4096 would take gigabytes.
pub const MAX_PROGRAM_BYTES: u64 = 512 * 1024 * 1024;

/// Bytes of the `System` uniform in the program prelude.
const SYSTEM_BYTES: usize = 96;

/// `sys.clock` is media-clock seconds wrapped to this, so an `f32` keeps better than a
/// millisecond.
const CLOCK_WRAP_SECONDS: f64 = 3600.0;

/// What one input of a pass reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramInput {
    /// The picture of pass `n`: this frame's when `n` is listed before the reading pass,
    /// the frame before's otherwise (feedback, transparent on the first frame).
    Pass(usize),
    /// The program's image `n` ([`ExternalProgram::images`]).
    Image(usize),
    /// Slot `n`, which the chain runtime fills; transparent while empty.
    Slot(usize),
    /// The audio data image: [`AUDIO_BANDS`] by 2 texels, row 0 the spectrum's bands
    /// and row 1 the waveform as `0.5 + 0.5 * sample`, each in every channel with alpha
    /// 1, from [`super::VideoFilter::set_audio`]; silence until the first.
    Audio,
}

/// The bands of a program's spectrum, and the samples of its waveform, in the audio data
/// image.
pub const AUDIO_BANDS: usize = 64;

/// The audio data image's bytes: 2 rows of [`AUDIO_BANDS`] `f16` RGBA texels.
const AUDIO_BYTES: usize = AUDIO_BANDS * 2 * 8;

/// One pass of a program. Two are equal when they draw the same: the same WGSL, scale
/// and inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramPass {
    /// WGSL appended to [`PROGRAM_PRELUDE`]: the generated header and the pass's file,
    /// defining `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>`. It declares its
    /// inputs in order, from [`FIRST_INPUT_BINDING`] up.
    pub wgsl: Arc<str>,
    /// The pass's size as a fraction of the program's, 1/16 to 1. The last pass draws at
    /// the program's size whatever this says.
    pub scale: f32,
    /// What its inputs read, in binding order: at most [`MAX_INPUTS`].
    pub inputs: Vec<ProgramInput>,
}

/// An image a program reads: straight-alpha sRGB RGBA8, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Arc<[u8]>,
}

/// What a program hears for a frame: levels for `sys`, and the spectrum and waveform for
/// [`ProgramInput::Audio`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgramAudio {
    /// RMS level, 0 to 1.
    pub level: f32,
    /// Peak level, 0 to 1.
    pub peak: f32,
    /// 1 on a sudden rise of the level, decaying to 0.
    pub onset: f32,
    /// Each band's level, 0 to 1, lowest frequency first.
    pub bands: [f32; AUDIO_BANDS],
    /// The latest waveform, [`AUDIO_BANDS`] samples from -1 to 1, oldest first.
    pub wave: [f32; AUDIO_BANDS],
}

impl Default for ProgramAudio {
    /// Silence.
    fn default() -> Self {
        Self {
            level: 0.0,
            peak: 0.0,
            onset: 0.0,
            bands: [0.0; AUDIO_BANDS],
            wave: [0.0; AUDIO_BANDS],
        }
    }
}

/// A shader-source program defined outside this crate (a pack, ADR-0093).
pub trait ExternalProgram: Send + Sync {
    /// The pipeline cache key's stem: unique per kind. Pass `n` is cached as
    /// `<label>#<n>`, and rebuilt when its WGSL changes.
    fn label(&self) -> &str;

    /// The passes, the picture last: 1 to [`MAX_PASSES`].
    fn passes(&self) -> &[ProgramPass];

    /// The images [`ProgramInput::Image`] reads.
    fn images(&self) -> &[ProgramImage];

    /// The `Params` uniform's bytes for `settings`: a multiple of 16, at least 16.
    fn uniforms(&self, settings: &Settings) -> Vec<u8>;

    /// The picture's size in pixels for `settings`, each side at least 1.
    fn size(&self, settings: &Settings) -> (u32, u32);
}

/// The two layers a pass read back as feedback keeps: the one drawn this frame and the
/// one drawn the frame before.
struct History {
    layers: [Image; 2],
}

/// The pass of a shader-source program.
pub(crate) struct Program {
    program: Arc<dyn ExternalProgram>,
    /// Each pass's pipeline cache key.
    labels: Vec<String>,
    params: Vec<u8>,
    size: (u32, u32),
    /// Whether each pass is read back as feedback, so keeps a [`History`].
    feedback: Vec<bool>,
    history: Vec<Option<History>>,
    /// Which layer of each history this frame draws.
    parity: usize,
    /// The program's images, uploaded on the first draw (and again for another
    /// compositor).
    images: Vec<Image>,
    slots: Vec<Option<Arc<Image>>>,
    /// A transparent texel for an empty slot.
    blank: Option<Image>,
    /// The media-clock time of the first frame since the program was last restarted.
    start: Option<f64>,
    frame: u32,
    /// Whether the feedback layers are clear since the last restart: new ones are, and
    /// a restart clears the kept ones on the next draw.
    cleared: bool,
    audio: ProgramAudio,
    /// The audio data image, when a pass reads [`ProgramInput::Audio`], and whether
    /// `audio` changed since it was written.
    audio_image: Option<Image>,
    audio_written: bool,
    /// Each pass's picture this frame, and one pass's inputs: kept, so a steady frame
    /// allocates no list.
    drawn: Vec<Option<Image>>,
    inputs: Vec<Image>,
}

impl Program {
    pub(crate) fn new(program: Arc<dyn ExternalProgram>, settings: &Settings) -> Self {
        let passes = program.passes();
        let labels = (0..passes.len())
            .map(|n| format!("{}#{n}", program.label()))
            .collect();
        let mut feedback = vec![false; passes.len()];
        for (reader, pass) in passes.iter().enumerate() {
            for input in &pass.inputs {
                if let ProgramInput::Pass(read) = *input
                    && read >= reader
                    && let Some(flag) = feedback.get_mut(read)
                {
                    *flag = true;
                }
            }
        }
        let slots = passes
            .iter()
            .flat_map(|p| &p.inputs)
            .filter_map(|input| match input {
                ProgramInput::Slot(n) => Some(n + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        Self {
            params: program.uniforms(settings),
            size: clamp_size(program.size(settings)),
            history: passes.iter().map(|_| None).collect(),
            labels,
            feedback,
            parity: 0,
            images: Vec::new(),
            slots: vec![None; slots],
            blank: None,
            start: None,
            frame: 0,
            cleared: true,
            audio: ProgramAudio::default(),
            audio_image: None,
            audio_written: false,
            drawn: Vec::with_capacity(passes.len()),
            inputs: Vec::with_capacity(MAX_INPUTS),
            program,
        }
    }

    /// The size pass `n` draws at.
    fn pass_size(&self, n: usize) -> (u32, u32) {
        let passes = self.program.passes();
        if n + 1 >= passes.len() {
            return self.size;
        }
        let scale = passes[n].scale.clamp(1.0 / 16.0, 1.0);
        let side = |v: u32| ((v as f32 * scale).round() as u32).max(1);
        (side(self.size.0), side(self.size.1))
    }

    /// The bytes of the layers the program draws into at its size.
    fn layer_bytes(&self) -> u64 {
        (0..self.feedback.len())
            .map(|n| {
                let (w, h) = self.pass_size(n);
                let layers = if self.feedback[n] { 2 } else { 1 };
                u64::from(w) * u64::from(h) * 8 * layers
            })
            .sum()
    }

    /// Uploads the program's images and the blank texel for this compositor, once.
    fn upload(&mut self, ctx: &Ctx<'_>) -> Result<(), RenderError> {
        let stale = |image: &Image| !image.owner().is(ctx.maker.owner);
        if self.blank.as_ref().is_none_or(stale) {
            self.blank = Some(image::upload(&ctx.maker, 1, 1, &[0; 4])?);
        }
        if self.images.len() != self.program.images().len() || self.images.iter().any(stale) {
            self.images = self
                .program
                .images()
                .iter()
                .map(|i| image::upload(&ctx.maker, i.width, i.height, &i.rgba))
                .collect::<Result<_, _>>()?;
        }
        let hears = self
            .program
            .passes()
            .iter()
            .any(|p| p.inputs.contains(&ProgramInput::Audio));
        if hears && self.audio_image.as_ref().is_none_or(stale) {
            let silence = [[0.0, 0.0, 0.0, 1.0]; AUDIO_BANDS * 2];
            self.audio_image = Some(image::upload_unit_f16(
                &ctx.maker,
                AUDIO_BANDS as u32,
                2,
                &silence,
            )?);
            self.audio_written = false;
        }
        if let Some(audio) = &self.audio_image
            && !self.audio_written
        {
            image::rewrite_unit_f16(ctx.gpu, audio, &audio_bytes(&self.audio));
            self.audio_written = true;
        }
        Ok(())
    }

    /// Makes each feedback pass's two layers at its size, transparent, unless it has
    /// them already.
    fn keep_history(&mut self, ctx: &Ctx<'_>) -> Result<(), RenderError> {
        for n in 0..self.history.len() {
            if !self.feedback[n] {
                continue;
            }
            let (width, height) = self.pass_size(n);
            let fits = self.history[n].as_ref().is_some_and(|h| {
                h.layers.iter().all(|l| {
                    (l.width(), l.height()) == (width, height) && l.owner().is(ctx.maker.owner)
                })
            });
            if !fits {
                // A new texture reads as zero (wgpu clears what it hands out): feedback
                // starts transparent.
                self.history[n] = Some(History {
                    layers: [ctx.layer(width, height)?, ctx.layer(width, height)?],
                });
            }
        }
        Ok(())
    }

    /// The `sys` uniform's bytes for a pass of `pass` size, little-endian as WGSL reads
    /// them, in a fixed array so a frame allocates none.
    fn system(
        &self,
        pass: (u32, u32),
        time: f64,
        clock: f64,
        alpha: &[f32; MAX_INPUTS],
    ) -> [u8; SYSTEM_BYTES] {
        let mut bytes = [0u8; SYSTEM_BYTES];
        let mut at = 0;
        let mut put = |word: [u8; 4]| {
            bytes[at..at + 4].copy_from_slice(&word);
            at += 4;
        };
        put((self.size.0 as f32).to_le_bytes());
        put((self.size.1 as f32).to_le_bytes());
        put((pass.0 as f32).to_le_bytes());
        put((pass.1 as f32).to_le_bytes());
        put((time as f32).to_le_bytes());
        put((clock.rem_euclid(CLOCK_WRAP_SECONDS) as f32).to_le_bytes());
        put(self.frame.to_le_bytes());
        put(self.audio.level.to_le_bytes());
        put(self.audio.peak.to_le_bytes());
        put(self.audio.onset.to_le_bytes());
        // `_pad`.
        put([0; 4]);
        put([0; 4]);
        for value in alpha {
            put(value.to_le_bytes());
        }
        debug_assert_eq!(at, SYSTEM_BYTES);
        bytes
    }
}

impl super::pass::Pass for Program {
    fn timed(&self) -> Option<&str> {
        Some(self.program.label())
    }

    fn update(&mut self, settings: &Settings) {
        self.params = self.program.uniforms(settings);
        self.size = clamp_size(self.program.size(settings));
    }

    fn output_size(&self, _width: u32, _height: u32) -> (u32, u32) {
        self.size
    }

    fn set_input(&mut self, slot: usize, image: Option<Arc<Image>>) {
        if let Some(kept) = self.slots.get_mut(slot) {
            *kept = image;
        }
    }

    fn set_audio(&mut self, audio: ProgramAudio) {
        if audio != self.audio {
            self.audio = audio;
            self.audio_written = false;
        }
    }

    fn restart(&mut self) {
        self.start = None;
        self.frame = 0;
        // Cleared on the next draw, not dropped: the layers stay for reuse.
        self.cleared = false;
    }

    fn encode(
        &mut self,
        ctx: &mut Ctx<'_>,
        _input: &Image,
        output: &Image,
    ) -> Result<(), RenderError> {
        let program = Arc::clone(&self.program);
        let label = program.label();
        if self.params.len() < 16 || !self.params.len().is_multiple_of(16) {
            return Err(RenderError::Invalid {
                what: "shader source uniforms",
                detail: format!(
                    "{label}: {} bytes; the Params uniform is a multiple of 16, at least 16",
                    self.params.len()
                ),
            });
        }
        let bytes = self.layer_bytes();
        if bytes > MAX_PROGRAM_BYTES {
            return Err(RenderError::Invalid {
                what: "shader source",
                detail: format!(
                    "{label}: its layers at {}x{} would take {} MiB, over the {} MiB a source may use",
                    self.size.0,
                    self.size.1,
                    bytes >> 20,
                    MAX_PROGRAM_BYTES >> 20
                ),
            });
        }
        self.upload(ctx)?;
        self.keep_history(ctx)?;
        if !self.cleared {
            for layer in self.history.iter().flatten().flat_map(|h| &h.layers) {
                ctx.clear(layer)?;
            }
            self.cleared = true;
        }
        self.start.get_or_insert(ctx.clock().seconds);
        let current = self.parity;
        let previous = 1 - current;
        // Kept between frames, so a steady frame allocates no list.
        let mut drawn = std::mem::take(&mut self.drawn);
        drawn.clear();
        drawn.resize(program.passes().len(), None);
        let mut inputs = std::mem::take(&mut self.inputs);
        let result = self.draw_passes(ctx, output, (current, previous), &mut drawn, &mut inputs);
        drawn.clear();
        inputs.clear();
        self.drawn = drawn;
        self.inputs = inputs;
        result?;
        self.parity = previous;
        self.frame = self.frame.saturating_add(1);
        Ok(())
    }
}

impl Program {
    /// Draws every pass of this frame, each into its layer, `drawn[n]` holding pass
    /// `n`'s once drawn, and the picture into `output`. `inputs` is a kept list for
    /// each pass's inputs.
    fn draw_passes(
        &mut self,
        ctx: &mut Ctx<'_>,
        output: &Image,
        (current, previous): (usize, usize),
        drawn: &mut [Option<Image>],
        inputs: &mut Vec<Image>,
    ) -> Result<(), RenderError> {
        let program = Arc::clone(&self.program);
        let label = program.label();
        let passes = program.passes();
        let last = passes.len().saturating_sub(1);
        let now = ctx.clock().seconds;
        let time = (now - self.start.unwrap_or(now)).max(0.0);
        for (n, pass) in passes.iter().enumerate() {
            let size = self.pass_size(n);
            let target = match &self.history[n] {
                Some(history) => history.layers[current].clone(),
                None if n == last => output.clone(),
                None => ctx.scratch(size.0, size.1)?,
            };
            inputs.clear();
            for input in &pass.inputs {
                let image = match *input {
                    ProgramInput::Pass(read) if read < n => drawn.get(read).and_then(Clone::clone),
                    ProgramInput::Pass(read) => self
                        .history
                        .get(read)
                        .and_then(Option::as_ref)
                        .map(|h| h.layers[previous].clone()),
                    ProgramInput::Image(k) => self.images.get(k).cloned(),
                    ProgramInput::Slot(k) => self
                        .slots
                        .get(k)
                        .and_then(Option::as_ref)
                        .map(|i| Image::clone(i)),
                    ProgramInput::Audio => self.audio_image.clone(),
                };
                let image = match image {
                    Some(image) => image,
                    None => self.blank.clone().ok_or_else(|| RenderError::Invalid {
                        what: "shader source",
                        detail: format!("{label}: no blank texel"),
                    })?,
                };
                inputs.push(image);
            }
            let mut alpha = [0.0; MAX_INPUTS];
            for (slot, input) in alpha.iter_mut().zip(inputs.iter()) {
                *slot = input.alpha().shader_mode();
            }
            let system = self.system(size, time, now, &alpha);
            let shader = ShaderRef {
                label: &self.labels[n],
                source: &pass.wgsl,
                blend: super::pass::Blend::Replace,
            };
            ctx.draw_program(shader, inputs, &system, &self.params, &target)?;
            drawn[n] = Some(target);
        }
        if self.history[last].is_some()
            && let Some(picture) = &drawn[last]
        {
            super::multipass::copy(ctx, picture, output)?;
        }
        Ok(())
    }
}

/// `audio`'s data image bytes ([`ProgramInput::Audio`]), in a fixed array.
fn audio_bytes(audio: &ProgramAudio) -> [u8; AUDIO_BYTES] {
    let mut bytes = [0u8; AUDIO_BYTES];
    let rows = audio
        .bands
        .iter()
        .copied()
        .chain(audio.wave.iter().map(|s| 0.5 + 0.5 * s));
    for (texel, value) in bytes.chunks_exact_mut(8).zip(rows) {
        for (channel, out) in texel.chunks_exact_mut(2).enumerate() {
            image::unit_f16_bytes(if channel == 3 { 1.0 } else { value }, out);
        }
    }
    bytes
}

/// The most pixels a program's side may be: what `jstream-effects` allows a shader
/// source (`SOURCE_SIDE`), so the output layer `apply_filter` makes before the memory
/// cap is checked is at most 128 MiB.
const MAX_SIDE: u32 = 4096;

/// Each side of a program's size, from 1 to [`MAX_SIDE`].
fn clamp_size((width, height): (u32, u32)) -> (u32, u32) {
    (width.clamp(1, MAX_SIDE), height.clamp(1, MAX_SIDE))
}

#[cfg(test)]
mod tests;
