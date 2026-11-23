// What the sky backdrop's two shaders share (ADR-0167, ADR-0174): `sky.wgsl`, which draws
// a frame's sky, and `sky_deep.wgsl`, which bakes its nebulae and galaxy band. Each
// defines `seed()`, the sky's seed, from its own uniform.

fn hash(x: u32) -> u32 {
    var h = x * 747796405u + 2891336453u;
    h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
    return (h >> 22u) ^ h;
}

fn unit(x: u32) -> f32 {
    return f32(hash(x) & 0xffffffu) / 16777216.0;
}

// The band's pole: the seed tilts it, so each sky's band crosses it somewhere else.
fn galaxy_axis() -> vec3<f32> {
    let k = hash(seed() ^ 0x27d4eb2du);
    let tilt = vec3<f32>(unit(k) - 0.5, unit(k ^ 0x9u) - 0.5, unit(k ^ 0x7u) - 0.5) * 1.2;
    return normalize(vec3<f32>(0.32, -0.86, 0.4) + tilt);
}
