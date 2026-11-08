//! Goldens for a source drawn small in 2D (#1217): a camera or test card in a box much
//! smaller than its frame. Drawn with one bilinear level such an item skips texels and
//! loses its fine detail; drawn from a mip chain it is close to the box-filtered image of
//! the same texels, the CPU reference [`super::three_d`] builds, which is what the source
//! looks like at its drawn size.
//!
//! On lavapipe, with one level, the mean error over these boxes was 19 to 62 sRGB steps
//! and the 99th percentile about 100 to 130; from a chain it is under 10 and under 50.

use super::open;
use super::three_d::{
    LARGE_SOURCE, Texels, check, draw, error, geometry, gpu, interior, pattern, reference,
};
use crate::Crop;

/// Boxes of a 512² source at a half to a thirteenth of its size, straight and rotated,
/// each close to the box-filtered reference over its interior.
#[test]
fn golden_2d_a_source_drawn_small_is_box_filtered() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(LARGE_SOURCE);
    let texels = Texels::new(&rgba, LARGE_SOURCE);
    for side in [400.0, 256.0, 160.0, 90.0, 40.0] {
        for rotation in [0.0, 20.0] {
            let items = [geometry(
                (120.0, 68.0),
                (side, side * 0.5),
                rotation,
                0.0,
                0.0,
                0.0,
                None,
            )];
            let got = gpu(
                &mut opened.compositor,
                &items,
                &rgba,
                LARGE_SOURCE,
                Crop::NONE,
            );
            let want = reference(&items, &texels, 16, Crop::NONE);
            let name = format!("2D {side} px, {rotation}°");
            check(&name, error(&got, &want, interior), 10.0, 50.0);
        }
    }
}

/// An item drawn slightly small, here at 1 / 1.2, samples a chain too: with one level
/// the mean was 5.5 and the 99th percentile 46.
#[test]
fn golden_2d_a_source_drawn_slightly_small_is_box_filtered() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(LARGE_SOURCE);
    let side = LARGE_SOURCE as f32 / 1.2;
    let items = [geometry(
        (120.0, 68.0),
        (side, side),
        0.0,
        0.0,
        0.0,
        0.0,
        None,
    )];
    let got = gpu(
        &mut opened.compositor,
        &items,
        &rgba,
        LARGE_SOURCE,
        Crop::NONE,
    );
    let want = reference(&items, &Texels::new(&rgba, LARGE_SOURCE), 16, Crop::NONE);
    check("2D at 1 / 1.2", error(&got, &want, interior), 4.0, 40.0);
}

/// Which chain a 2D item gets: none at or near its size, a full one when one axis is
/// minified, and one starting `k` levels down when both are minified `2^k` times or more.
#[test]
fn a_2d_item_drawn_small_gets_a_chain_and_a_much_smaller_one_skips_levels() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let rgba = pattern(LARGE_SOURCE);
    let size = LARGE_SOURCE as f32;
    let image = compositor
        .upload_rgba8(LARGE_SOURCE, LARGE_SOURCE, &rgba)
        .expect("the pattern uploads");
    let at = |w: f32, h: f32| [geometry((120.0, 68.0), (w, h), 0.0, 0.0, 0.0, 0.0, None)];

    // At its size, and a little under (1.1 texels per pixel): one level, no chain.
    draw(compositor, &at(size, size), &image, Crop::NONE);
    draw(compositor, &at(size / 1.1, size / 1.1), &image, Crop::NONE);
    assert_eq!(image.chain_skips(), Vec::<u32>::new());

    // Squeezed along one axis only: a full chain.
    draw(compositor, &at(size, size / 3.0), &image, Crop::NONE);
    assert_eq!(image.chain_skips(), [0]);

    // A quarter of its size each way: a chain from a quarter size, built once while
    // the upload is unchanged.
    draw(compositor, &at(size / 4.0, size / 4.0), &image, Crop::NONE);
    draw(compositor, &at(size / 4.0, size / 4.0), &image, Crop::NONE);
    assert_eq!(image.chain_skips(), [2, 0]);
    assert_eq!(image.chain_builds(), (2, 2));

    // Far smaller: never more than MAX_SKIP levels down.
    draw(
        compositor,
        &at(size / 40.0, size / 40.0),
        &image,
        Crop::NONE,
    );
    assert_eq!(image.chain_skips(), [crate::mips::MAX_SKIP, 2, 0]);
}

/// Sources whose mip levels have odd sizes (1080 halves to 135, then 67), drawn at 1 / 14
/// and 1 / 20 of their size, slightly rotated, are as close to the reference as a
/// power-of-two one. Each level is the exact box average of what it covers: with one
/// linear tap per texel, a level of odd size only half covered the texels at its edge,
/// and these measured a mean of 13 to 17 and a p99 of 50 to 72, against a mean of 3.5
/// and 2.7 (at 1 / 14 and 1 / 20) for a 512² source.
#[test]
fn golden_2d_a_source_with_odd_mip_levels_is_box_filtered() {
    let Some(mut opened) = open() else {
        return;
    };
    for size in [600u32, 1080] {
        let rgba = pattern(size);
        let texels = Texels::new(&rgba, size);
        for ratio in [14.0f32, 20.0] {
            let side = size as f32 / ratio;
            let items = [geometry(
                (120.0, 68.0),
                (side, side),
                7.0,
                0.0,
                0.0,
                0.0,
                None,
            )];
            let got = gpu(&mut opened.compositor, &items, &rgba, size, Crop::NONE);
            let want = reference(&items, &texels, 16, Crop::NONE);
            let name = format!("2D {size}² at 1 / {ratio}");
            check(&name, error(&got, &want, interior), 5.0, 20.0);
        }
    }
}
