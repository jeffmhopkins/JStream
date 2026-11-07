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
/// minified, and one starting at half size when both are minified twice or more.
#[test]
fn a_2d_item_drawn_small_gets_a_chain_and_a_much_smaller_one_skips_level_0() {
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

    // A quarter of its size each way: a chain from half size, built once while the
    // upload is unchanged.
    draw(compositor, &at(size / 4.0, size / 4.0), &image, Crop::NONE);
    draw(compositor, &at(size / 4.0, size / 4.0), &image, Crop::NONE);
    assert_eq!(image.chain_skips(), [1, 0]);
    assert_eq!(image.chain_builds(), (2, 2));
}
