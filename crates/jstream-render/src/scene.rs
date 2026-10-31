//! The snapshot the engine hands the compositor: one canvas, a clear colour, a stack of items.

use jstream_media::FrameSize;
use jstream_protocol::BlendMode;

use crate::image::Image;

/// Straight-alpha sRGB colour. Components are in `0..=1`; [`crate::Compositor::render`]
/// rejects anything else, including non-finite values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    /// Red, sRGB.
    pub r: f32,
    /// Green, sRGB.
    pub g: f32,
    /// Blue, sRGB.
    pub b: f32,
    /// Straight alpha. Not sRGB-encoded.
    pub a: f32,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    /// Opaque white.
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    /// Fully transparent. The RGB channels are ignored once premultiplied.
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// An opaque colour. Alpha is 1. The components are not checked here.
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }
}

/// Source pixels cut off each edge before the placement is applied.
///
/// The same fields as the protocol's `Crop`. Cutting away the whole source draws nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Crop {
    /// Pixels cut from the left.
    pub left: u32,
    /// Pixels cut from the top.
    pub top: u32,
    /// Pixels cut from the right.
    pub right: u32,
    /// Pixels cut from the bottom.
    pub bottom: u32,
}

impl Crop {
    /// No cropping.
    pub const NONE: Self = Self {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
}

/// Legacy 0.3 placement: top-left, scale against the source's current size, clockwise
/// rotation about that top-left.
///
/// The same fields as the protocol's `ItemTransform`. `place` maps them onto that type
/// and calls `legacy_quad`, every frame, against the image's size (ADR-0022 decision 5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegacyTransform {
    /// Top-left corner, in canvas pixels. x right, y down.
    pub position: [f32; 2],
    /// Scale of the cropped source. `1.0` is one source pixel per canvas pixel.
    /// A negative component flips that axis.
    pub scale: [f32; 2],
    /// Clockwise rotation in degrees, about [`Self::position`].
    pub rotation_deg: f32,
}

impl LegacyTransform {
    /// Top-left at the origin, native size, no rotation.
    pub const IDENTITY: Self = Self {
        position: [0.0, 0.0],
        scale: [1.0, 1.0],
        rotation_deg: 0.0,
    };
}

impl Default for LegacyTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Where an item sits on the canvas: one of ADR-0022's two placement forms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// A 0.3 transform, evaluated against the image's size on every render.
    Legacy(LegacyTransform),
    /// A box on the canvas, with the protocol's own type. The cropped image is fitted
    /// into the box by its fit mode, and `fill` is clipped to the box. Non-neutral 3D
    /// draws ADR-0050's drawn polygon.
    Geometry(jstream_protocol::ItemGeometry),
}

impl Placement {
    /// The legacy identity: top-left at the origin, native size, no rotation.
    pub const IDENTITY: Self = Self::Legacy(LegacyTransform::IDENTITY);
}

impl Default for Placement {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// One image in the stack.
#[derive(Clone, Copy, Debug)]
pub struct Item<'a> {
    /// The image it places. It must have been made by the [`crate::Compositor`] that
    /// renders the scene; any other is [`crate::RenderError::Invalid`].
    pub image: &'a Image,
    /// Where it sits.
    pub placement: Placement,
    /// Source pixels removed before placement.
    pub crop: Crop,
    /// Straight-alpha multiplier in `0..=1`.
    pub opacity: f32,
    /// How it composites onto what is below it (ADR-0082). [`BlendMode::Normal`] is
    /// source-over.
    pub blend: BlendMode,
    /// When false, the item is not drawn and its frame sync is not staged.
    pub visible: bool,
}

/// One scene: a canvas, its clear colour, and items bottom-first.
#[derive(Clone, Copy, Debug)]
pub struct Scene<'a> {
    /// Canvas size in pixels. Both dimensions are non-zero (`FrameSize`).
    pub canvas: FrameSize,
    /// Clear colour, straight sRGB, drawn under every item.
    pub background: Color,
    /// Stacking order. The first item is at the bottom.
    pub items: &'a [Item<'a>],
}

/// Tightly packed straight-alpha sRGB RGBA8, the snapshot readback.
///
/// Rows have no padding. Alpha is linear (not sRGB-encoded), which is how an sRGB
/// target stores it. RGB has been un-premultiplied and encoded with the sRGB transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Snapshot {
    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Tightly packed RGBA8, length `width * height * 4`.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The pixel at `(x, y)`, or `None` when that point is outside the snapshot.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ])
    }

    pub(crate) fn from_padded(width: u32, height: u32, padded: &[u8], stride: usize) -> Self {
        let row = width as usize * 4;
        let mut rgba = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            let start = y * stride;
            rgba.extend_from_slice(&padded[start..start + row]);
        }
        Self {
            width,
            height,
            rgba,
        }
    }
}

/// The sRGB target of one [`crate::Compositor::render`].
///
/// [`Self::texture`] is the compositor's current target. The next `render` overwrites
/// those texels when the canvas size is unchanged, and drops them when the size changes
/// only after every clone is gone. [`Self::pixels`] is an owned copy and is unaffected.
pub struct Output {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    pixels: Option<Snapshot>,
}

impl Output {
    /// The straight-alpha sRGB target (`Rgba8UnormSrgb`).
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Canvas width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Canvas height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The CPU copy, present when readback was requested.
    pub fn pixels(&self) -> Option<&Snapshot> {
        self.pixels.as_ref()
    }

    pub(crate) fn new(
        texture: wgpu::Texture,
        width: u32,
        height: u32,
        pixels: Option<Snapshot>,
    ) -> Self {
        Self {
            texture,
            width,
            height,
            pixels,
        }
    }
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("readback", &self.pixels.is_some())
            .finish_non_exhaustive()
    }
}
