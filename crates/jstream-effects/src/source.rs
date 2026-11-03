//! Shader sources (ADR-0093): a pack's `[[source]]` tables, each a `source` kind whose
//! picture is a program of passes ([`jstream_render::ExternalProgram`]).
//!
//! Each pass's WGSL is the generated header, the source's `common` file and the pass's
//! own file. The header declares `p` (the parameters with a uniform form, at binding 3)
//! and one texture per name the pass reads, in its `inputs` order from binding 4, each
//! with four functions of its name `n`:
//! - `n(uv)`: bilinear, clamped to the edge, premultiplied linear;
//! - `n_wrap(uv)`: the same, repeating;
//! - `n_texel(px)`: one texel, clamped to the image;
//! - `n_size()`: the image's size in pixels.
//!
//! An `audio` source's `audio` input also gets `audio_band(i)` (band `i` of
//! [`AUDIO_BANDS`], 0 to 1) and `audio_wave(i)` (sample `i` of the latest waveform, −1
//! to 1).

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jstream_protocol::{KindCategory, KindDescriptor, Media, ParamDescriptor, ParamType, Settings};
use jstream_render::{
    ExternalProgram, FIRST_INPUT_BINDING, PROGRAM_PARAMS_BINDING, ProgramImage, ProgramInput,
    ProgramPass,
};
use serde_json::Value;

use crate::error::PackError;
use crate::manifest::{self, AUDIO_INPUT, ParamKind, SOURCE_SIDE, SourceSection};
use crate::pack::{MAX_SHADER_BYTES, read, real_path};
use crate::shader::{self, Layout};

/// How many spectrum bands, and waveform samples, the `audio` input holds: its width.
pub const AUDIO_BANDS: usize = jstream_render::PROGRAM_AUDIO_BANDS;

/// The largest image file a source reads.
pub const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

/// The longest side of an image a source reads, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 2048;

/// One shader source: a `source` kind with the program that draws it.
#[derive(Clone, Debug)]
pub struct ShaderSource {
    descriptor: KindDescriptor,
    passes: Vec<ProgramPass>,
    images: Vec<ProgramImage>,
    /// The parameters with a uniform form, in declaration order.
    uniform_params: Vec<ParamDescriptor>,
    layout: Layout,
    renames: Vec<(String, String)>,
    /// The `scene` parameters, by the slot each fills, in declaration order.
    scenes: Vec<String>,
    audio: bool,
    files: Vec<PathBuf>,
}

impl ShaderSource {
    /// Loads `section` from the pack in `dir` (whose canonical path is `root`), with
    /// each pass validated against `prelude`.
    pub(crate) fn load(
        pack_id: &str,
        dir: &Path,
        root: &Path,
        manifest_path: &Path,
        section: SourceSection,
        prelude: &str,
    ) -> Result<Self, PackError> {
        let at = |field: String| format!("source.{}.{field}", section.id);
        let mut files = Vec::new();
        let load_text = |rel: &str, field: String| -> Result<(PathBuf, String), PackError> {
            let path = real_path(dir, root, rel, manifest_path, &field)?;
            let text = read(&path, MAX_SHADER_BYTES)?;
            Ok((path, text))
        };
        let common = match &section.common {
            Some(rel) => Some(load_text(rel, at("common".into()))?),
            None => None,
        };
        files.extend(common.as_ref().map(|(path, _)| path.clone()));
        let mut params: Vec<ParamDescriptor> = vec![
            side("width", "Width", section.width),
            side("height", "Height", section.height),
        ];
        if section.audio {
            params.push(audio_param());
            params.push(audio_source_param());
        }
        let declared: Vec<ParamDescriptor> =
            section.params.iter().map(manifest::descriptor).collect();
        let uniform_params: Vec<ParamDescriptor> = section
            .params
            .iter()
            .zip(&declared)
            .filter(|(p, _)| !matches!(p.ty, ParamKind::Scene))
            .map(|(_, d)| d.clone())
            .collect();
        let scenes: Vec<String> = section
            .params
            .iter()
            .filter(|p| matches!(p.ty, ParamKind::Scene))
            .map(|p| p.id.clone())
            .collect();
        params.extend(declared);
        let layout = Layout::new(&uniform_params);
        let params_header = layout.header_at(PROGRAM_PARAMS_BINDING);
        // The images, by name, in the manifest's (sorted) order.
        let mut images = Vec::with_capacity(section.images.len());
        let mut image_index = BTreeMap::new();
        for (name, rel) in &section.images {
            let field = format!("images.{name}");
            let path = real_path(dir, root, rel, manifest_path, &at(field))?;
            images.push(decode(&path)?);
            files.push(path);
            image_index.insert(name.as_str(), images.len() - 1);
        }
        let pass_index: BTreeMap<&str, usize> = section
            .passes
            .iter()
            .enumerate()
            .map(|(n, p)| (p.id.as_str(), n))
            .collect();
        let mut passes = Vec::with_capacity(section.passes.len());
        for pass in &section.passes {
            let (path, text) = load_text(&pass.shader, at(format!("pass.{}.shader", pass.id)))?;
            files.push(path.clone());
            let mut header = params_header.clone();
            let mut inputs = Vec::with_capacity(pass.inputs.len());
            for (n, name) in pass.inputs.iter().enumerate() {
                let input = if let Some(&k) = pass_index.get(name.as_str()) {
                    ProgramInput::Pass(k)
                } else if let Some(&k) = image_index.get(name.as_str()) {
                    ProgramInput::Image(k)
                } else if let Some(k) = scenes.iter().position(|s| s == name) {
                    ProgramInput::Slot(k)
                } else if name == AUDIO_INPUT && section.audio {
                    ProgramInput::Audio
                } else {
                    // The manifest check names every input.
                    return Err(PackError::Manifest {
                        path: manifest_path.to_owned(),
                        at: at(format!("pass.{}.inputs", pass.id)),
                        message: format!("`{name}` names nothing"),
                    });
                };
                header.push_str(&input_header(name, n as u32));
                if name == AUDIO_INPUT && section.audio {
                    header.push_str(AUDIO_HELPERS);
                }
                inputs.push(input);
            }
            let mut parts: Vec<&str> = Vec::with_capacity(2);
            if let Some((_, common)) = &common {
                parts.push(common);
            }
            parts.push(&text);
            shader::validate_parts(prelude, &header, &parts).map_err(|(part, message)| {
                let path = match (&common, part) {
                    (Some((common_path, _)), 0) => common_path.clone(),
                    _ => path.clone(),
                };
                PackError::Shader { path, message }
            })?;
            let mut wgsl = header;
            wgsl.push('\n');
            for part in &parts {
                wgsl.push_str(part);
                wgsl.push('\n');
            }
            passes.push(ProgramPass {
                wgsl: wgsl.into(),
                scale: pass.scale as f32,
                inputs,
            });
        }
        let descriptor = KindDescriptor {
            kind_id: format!("{pack_id}/{}", section.id),
            category: KindCategory::Source,
            name: section.name,
            version: section.version,
            video: true,
            audio: false,
            max_outset_px: None,
            params,
        };
        Ok(Self {
            descriptor,
            passes,
            images,
            uniform_params,
            layout,
            renames: section.renames.into_iter().collect(),
            scenes,
            audio: section.audio,
            files,
        })
    }

    /// The kind ID, `<pack>/<source>`.
    pub fn kind_id(&self) -> &str {
        &self.descriptor.kind_id
    }

    /// The kind's descriptor (ADR-0025): a `source`, every parameter live.
    pub fn descriptor(&self) -> &KindDescriptor {
        &self.descriptor
    }

    /// The `scene` parameters, in declaration order: the `n`th fills slot `n`.
    pub fn scene_params(&self) -> &[String] {
        &self.scenes
    }

    /// Whether it hears audio (`audio = true`): it then has the `audio` and
    /// `audio_source` settings, and a pass may read [`ProgramInput::Audio`].
    pub fn hears(&self) -> bool {
        self.audio
    }

    /// Where each parameter with a uniform form sits in the `Params` uniform.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The files it was loaded from besides the manifest: its WGSL and its images.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The `Params` uniform's bytes for `settings`, complete as the graph stores them.
    pub fn uniforms(&self, settings: &Settings) -> Vec<u8> {
        self.layout.write(&self.uniform_params, settings)
    }

    /// The picture's size for `settings`: its `width` and `height`, each read as its
    /// default when missing or out of range.
    pub fn size(&self, settings: &Settings) -> (u32, u32) {
        let side = |id: &str| {
            let default = self
                .descriptor
                .param(id)
                .and_then(|p| p.default.as_u64())
                .unwrap_or(u64::from(*SOURCE_SIDE.start()));
            let v = settings
                .0
                .get(id)
                .and_then(Value::as_u64)
                .unwrap_or(default);
            let v = u32::try_from(v).unwrap_or(*SOURCE_SIDE.end());
            if SOURCE_SIDE.contains(&v) {
                v
            } else {
                u32::try_from(default).unwrap_or(*SOURCE_SIDE.start())
            }
        };
        (side("width"), side("height"))
    }

    /// Migrates settings stored by version `from` of this kind, as [`crate::Effect::migrate`].
    pub fn migrate(&self, stored: &Settings, from: u32) -> Option<Settings> {
        if from > self.descriptor.version {
            return None;
        }
        let mut out = stored.clone();
        if from < self.descriptor.version {
            for (old, new) in &self.renames {
                if !out.0.contains_key(new)
                    && let Some(value) = out.0.remove(old)
                {
                    out.0.insert(new.clone(), value);
                }
            }
        }
        Some(out)
    }

    /// The program's passes, the picture last.
    pub fn passes(&self) -> &[ProgramPass] {
        &self.passes
    }

    /// A render filter drawing this source's program with `settings`: the first filter
    /// of the source's chain (ADR-0093).
    pub fn video_filter(&self, settings: &Settings) -> jstream_render::VideoFilter {
        jstream_render::VideoFilter::program(self.kind_id(), Arc::new(self.clone()), settings)
    }
}

impl ExternalProgram for ShaderSource {
    fn label(&self) -> &str {
        self.kind_id()
    }

    fn passes(&self) -> &[ProgramPass] {
        &self.passes
    }

    fn images(&self) -> &[ProgramImage] {
        &self.images
    }

    fn uniforms(&self, settings: &Settings) -> Vec<u8> {
        ShaderSource::uniforms(self, settings)
    }

    fn size(&self, settings: &Settings) -> (u32, u32) {
        ShaderSource::size(self, settings)
    }
}

/// The header lines for input `n` of a pass, read as `name`.
fn input_header(name: &str, n: u32) -> String {
    let binding = FIRST_INPUT_BINDING + n;
    format!(
        "@group(0) @binding({binding}) var {name}_tex: texture_2d<f32>;\n\
         fn {name}(uv: vec2<f32>) -> vec4<f32> {{ return to_premul_linear(textureSampleLevel({name}_tex, lin, uv, 0.0), input_alpha({n}u)); }}\n\
         fn {name}_wrap(uv: vec2<f32>) -> vec4<f32> {{ return to_premul_linear(textureSampleLevel({name}_tex, rep, uv, 0.0), input_alpha({n}u)); }}\n\
         fn {name}_texel(px: vec2<i32>) -> vec4<f32> {{ let m = vec2<i32>(textureDimensions({name}_tex)) - vec2<i32>(1); return to_premul_linear(textureLoad({name}_tex, clamp(px, vec2<i32>(0), m), 0), input_alpha({n}u)); }}\n\
         fn {name}_size() -> vec2<f32> {{ return vec2<f32>(textureDimensions({name}_tex)); }}\n"
    )
}

/// The audio input's helpers: its first row is the spectrum, its second the waveform
/// stored as `0.5 + 0.5 * sample`.
const AUDIO_HELPERS: &str = "fn audio_band(i: i32) -> f32 { return audio_texel(vec2<i32>(i, 0)).r; }\n\
     fn audio_wave(i: i32) -> f32 { return audio_texel(vec2<i32>(i, 1)).r * 2.0 - 1.0; }\n";

/// The `width` or `height` setting, from 16 to 4096 pixels.
fn side(id: &str, label: &str, default: u32) -> ParamDescriptor {
    ParamDescriptor {
        id: id.into(),
        ty: ParamType::Int {
            min: Some(i64::from(*SOURCE_SIDE.start())),
            max: Some(i64::from(*SOURCE_SIDE.end())),
            step: Some(1),
            unit: Some("px".into()),
        },
        default: Value::from(default),
        label: label.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: true,
    }
}

/// The `audio` setting, as the `spectrum` kind has it.
fn audio_param() -> ParamDescriptor {
    ParamDescriptor {
        id: "audio".into(),
        ty: ParamType::Bus,
        default: Value::Null,
        label: "Audio".into(),
        description: Some(
            "What it hears. None hears the program mix: the master after its filters, as \
             the outputs hear it. A bus is that bus after its fader. Either is silent \
             while no output runs."
                .into(),
        ),
        group: None,
        advanced: false,
        visible_if: None,
        live: false,
    }
}

/// The `audio_source` setting, as the `spectrum` kind has it.
fn audio_source_param() -> ParamDescriptor {
    ParamDescriptor {
        id: "audio_source".into(),
        ty: ParamType::Source {
            media: Some(Media::Audio),
        },
        default: Value::Null,
        label: "Audio source".into(),
        description: Some(
            "A source to hear instead of the audio above: that source after its own \
             filters, fader, mute and follow, before any bus. Silent while that source \
             isn't playing into the mixer, and while no output runs."
                .into(),
        ),
        group: None,
        advanced: false,
        visible_if: None,
        live: false,
    }
}

/// Decodes the PNG or JPEG at `path` into straight-alpha sRGB RGBA8.
fn decode(path: &Path) -> Result<ProgramImage, PackError> {
    let io = |source| PackError::Io {
        path: path.to_owned(),
        source,
    };
    let size = std::fs::metadata(path).map_err(io)?.len();
    if size > MAX_IMAGE_BYTES {
        return Err(PackError::TooLarge {
            path: path.to_owned(),
            size,
            limit: MAX_IMAGE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(io)?;
    let bad = |message: String| PackError::Image {
        path: path.to_owned(),
        message,
    };
    let (width, height, rgba) = if bytes.starts_with(b"\x89PNG") {
        png_rgba(&bytes).map_err(bad)?
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        jpeg_rgba(&bytes).map_err(bad)?
    } else {
        return Err(bad("is neither a PNG nor a JPEG".into()));
    };
    Ok(ProgramImage {
        width,
        height,
        rgba: rgba.into(),
    })
}

fn check_side(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err(format!(
            "is {width}x{height}; a source's images are 1 to {MAX_IMAGE_SIDE} pixels each way"
        ));
    }
    Ok(())
}

fn png_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = (reader.info().width, reader.info().height);
    check_side(width, height)?;
    let len = reader
        .output_buffer_size()
        .ok_or_else(|| "its frame size overflows".to_owned())?;
    let mut buf = vec![0; len];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("its palette was not expanded".into()),
    };
    let row = width as usize * channels;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for line in buf.chunks(info.line_size.max(1)).take(height as usize) {
        let pixels = line
            .get(..row)
            .ok_or_else(|| "a scanline is short".to_owned())?;
        for px in pixels.chunks_exact(channels) {
            rgba.extend_from_slice(&match *px {
                [l] => [l, l, l, 0xff],
                [l, a] => [l, l, l, a],
                [r, g, b] => [r, g, b, 0xff],
                [r, g, b, a] => [r, g, b, a],
                _ => return Err("a pixel has no channels".into()),
            });
        }
    }
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("it is truncated".into());
    }
    Ok((width, height, rgba))
}

fn jpeg_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let max = MAX_IMAGE_SIDE as usize;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(max)
        .set_max_height(max);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(bytes), options);
    decoder.decode_headers().map_err(|e| e.to_string())?;
    let info = decoder
        .info()
        .ok_or_else(|| "it has no image header".to_owned())?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_side(width, height)?;
    let rgba = decoder.decode().map_err(|e| e.to_string())?;
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("the decoder returned the wrong number of pixels".into());
    }
    Ok((width, height, rgba))
}
