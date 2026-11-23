// One direction of depth bloom's blur (ADR-0151 decision 8): a Gaussian of `count` taps
// on each side along `dir`, normalised by its full weight (`norm` is 1 over it) so
// a blocked tap takes its share away instead of moving it to others. The weights and
// the taps' whole-texel offsets are made once on the CPU (`bloom.rs`), so no pixel pays
// for an `exp` per tap. With a view depth
// (`focal` above 0), a tap's light reaches the pixel less where
// the pixel is nearer than the tap by more than `depth`, and none by twice that: a glow
// stops at something in front of it. A texel's 1 / w is in its alpha, where the bright
// pass put the view depth's at twice its position (it runs at half the picture's size),
// and each direction hands it on. Where either has nothing drawn, it spreads as
// plain bloom's does. Light from past the frame is none.

struct Params {
    dir_x: i32,
    dir_y: i32,
    count: i32,
    focal: f32,
    depth: f32,
    norm: f32,
    // Tap `i`, for `i` in 0..=count: `(weight, offset in texels)` in `taps[i / 2].xy`
    // or `.zw`.
    taps: array<vec4<f32>, 9>,
}

@group(0) @binding(4) var<uniform> p: Params;

fn tap(i: i32) -> vec2<f32> {
    let two = p.taps[i / 2];
    return select(two.xy, two.zw, i % 2 == 1);
}

// The distance 1 / w `inverse_w` stands for, -1 where nothing was drawn.
fn distance_of(inverse_w: f32) -> f32 {
    if inverse_w <= 0.0 {
        return -1.0;
    }
    return p.focal / inverse_w;
}

// The light of the texel at `at`, weighing `w`, that reaches a pixel at distance `here`.
fn reaching(at: vec2<i32>, w: f32, here: f32, size: vec2<i32>) -> vec3<f32> {
    if any(at < vec2<i32>(0)) || any(at >= size) {
        return vec3<f32>(0.0);
    }
    let c = textureLoad(input_tex, at, 0);
    var k = w;
    if here > 0.0 {
        let there = distance_of(c.a);
        if there > 0.0 {
            k = k * (1.0 - smoothstep(p.depth, 2.0 * p.depth, there - here));
        }
    }
    return c.rgb * k;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    let size = vec2<i32>(frame.in_size);
    let dir = vec2<i32>(p.dir_x, p.dir_y);
    let own = textureLoad(input_tex, clamp(px, vec2<i32>(0), size - 1), 0);
    var here = -1.0;
    if p.focal > 0.0 {
        here = distance_of(own.a);
    }
    var sum = own.rgb * tap(0).x;
    for (var i = 1; i <= p.count; i++) {
        let t = tap(i);
        let off = dir * i32(t.y);
        sum += reaching(px + off, t.x, here, size) + reaching(px - off, t.x, here, size);
    }
    return vec4<f32>(sum * p.norm, own.a);
}
