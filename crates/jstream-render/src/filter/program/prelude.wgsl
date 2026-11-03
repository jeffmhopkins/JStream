// Shared by every pass of a shader source (ADR-0093): a fullscreen triangle, the
// source's `sys` uniform, two samplers, and colour and noise helpers (`colour.wgsl` and
// `noise.wgsl`, which the host appends to this file). A pack's generated header comes
// next: its `Params` as `p` at binding 3, and one texture per name the pass reads, from
// binding 4 up, each with functions of its name. The pass's own file defines
// `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` and returns premultiplied
// linear colour.

struct System {
    // The source's size in pixels: its last pass's.
    size: vec2<f32>,
    // This pass's size in pixels (the source's at the pass's `scale`).
    pass_size: vec2<f32>,
    // Seconds since the source was last shown, so a one-shot effect plays from 0.
    time: f32,
    // Media-clock seconds, the same for every source on a frame, wrapped to an hour.
    clock: f32,
    // Frames drawn since the source was last shown, from 0.
    frame: u32,
    // An audio source's level (RMS) and peak, 0 to 1 (0 without audio).
    level: f32,
    peak: f32,
    // An audio source's onset: 1 on a sudden rise of the spectrum, decaying to 0.
    onset: f32,
    _pad: vec2<f32>,
    // How each input stores alpha, by input index (`alpha[i / 4][i % 4]`), as
    // `to_premul_linear` reads it.
    alpha: array<vec4<f32>, 3>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Output UV: (0, 0) top-left, (1, 1) bottom-right.
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var<uniform> sys: System;
// Bilinear, clamped to the edge.
@group(0) @binding(1) var lin: sampler;
// Bilinear, repeating: for tiling noise and sprites.
@group(0) @binding(2) var rep: sampler;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle covering clip space: (-1, -1), (3, -1), (-1, 3).
    var p = vec2<f32>(-1.0, 3.0);
    if (index == 0u) {
        p = vec2<f32>(-1.0, -1.0);
    } else if (index == 1u) {
        p = vec2<f32>(3.0, -1.0);
    }
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

// How input `i` stores alpha.
fn input_alpha(i: u32) -> f32 {
    return sys.alpha[i / 4u][i % 4u];
}
