// The nearest seed to each pixel, from `scan_rows.wgsl`'s nearest seed along each row:
// the second of the two passes. The nearest seed is the nearest along some row within
// `reach` rows of this one, so of those rows' nearest seeds this keeps the closest. The
// scan runs outward from the pixel's own row and ends once the rows are farther away
// than the best seed found. The result is what the jump flood writes: rg the vector to
// the seed in pixels, b its input alpha, alpha 1 when there is one.

struct Params {
    reach: i32,
}

@group(0) @binding(4) var<uniform> p: Params;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let px = out_px(in.pos);
    let size = vec2<i32>(frame.in_size);
    var best = textureLoad(input_tex, px, 0);
    var best_d = 3.4e38;
    if (best.a > 0.5) {
        best_d = best.x * best.x;
    }
    for (var k = 1; k <= p.reach; k++) {
        let rows = f32(k * k);
        if (rows >= best_d) {
            break;
        }
        if (px.y - k >= 0) {
            let v = textureLoad(input_tex, px - vec2<i32>(0, k), 0);
            let d = v.x * v.x + rows;
            if (v.a > 0.5 && d < best_d) {
                best_d = d;
                best = vec4<f32>(v.x, f32(-k), v.b, 1.0);
            }
        }
        if (px.y + k < size.y) {
            let v = textureLoad(input_tex, px + vec2<i32>(0, k), 0);
            let d = v.x * v.x + rows;
            if (v.a > 0.5 && d < best_d) {
                best_d = d;
                best = vec4<f32>(v.x, f32(k), v.b, 1.0);
            }
        }
    }
    return best;
}
