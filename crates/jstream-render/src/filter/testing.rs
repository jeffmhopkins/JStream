//! Golden-test harness for filters, on whatever adapter the crate's tests open (lavapipe
//! in CI). [`apply`] runs one filter over an image and reads the result back as straight
//! sRGB RGBA8, the same readback the compositor's own golden tests use.

use jstream_protocol::Settings;
use serde_json::Value;

use crate::filter::{FilterClock, VideoFilter, descriptors};
use crate::tests::Opened;
use crate::{Color, Crop, Image, Item, Placement, Scene, Snapshot};

/// `kind_id`'s settings: its descriptor's defaults, overlaid with `overrides` (a JSON
/// object), so a test names only what it changes. Panics on an unknown kind.
pub(crate) fn settings(kind_id: &str, overrides: Value) -> Settings {
    let kind = descriptors()
        .into_iter()
        .find(|k| k.kind_id == kind_id)
        .unwrap_or_else(|| panic!("{kind_id} is not a built-in video filter kind"));
    let mut settings: Settings = Settings(
        kind.params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect(),
    );
    if let Value::Object(map) = overrides {
        for (id, value) in map {
            assert!(kind.param(&id).is_some(), "{kind_id} has no parameter {id}");
            settings.0.insert(id, value);
        }
    }
    settings
}

/// The filter for `kind_id` with [`settings`].
pub(crate) fn filter(kind_id: &str, overrides: Value) -> VideoFilter {
    VideoFilter::new(kind_id, &settings(kind_id, overrides))
        .unwrap_or_else(|| panic!("{kind_id} has no pass"))
}

/// Draws `filter` over `input` at `seconds` and reads the output back.
pub(crate) fn run(
    opened: &mut Opened,
    filter: &mut VideoFilter,
    input: &Image,
    seconds: f64,
) -> Snapshot {
    let output = opened
        .compositor
        .apply_filter(filter, input, FilterClock { seconds })
        .expect("apply_filter");
    read(opened, &output)
}

/// `image` placed 1:1 on a transparent canvas of its size, read back.
pub(crate) fn read(opened: &mut Opened, image: &Image) -> Snapshot {
    let items = [Item {
        image,
        placement: Placement::IDENTITY,
        crop: Crop::NONE,
        opacity: 1.0,
        visible: true,
    }];
    let scene = Scene {
        canvas: jstream_media::FrameSize::new(image.width(), image.height())
            .expect("invariant: images are non-zero"),
        background: Color::TRANSPARENT,
        items: &items,
    };
    opened
        .compositor
        .render(&scene, true)
        .expect("render")
        .pixels()
        .expect("invariant: readback was requested")
        .clone()
}

/// An image from straight-alpha sRGB RGBA8 rows, `width` pixels wide.
pub(crate) fn image(opened: &Opened, width: u32, pixels: &[[u8; 4]]) -> Image {
    let height = u32::try_from(pixels.len()).expect("small") / width;
    let bytes: Vec<u8> = pixels.iter().flatten().copied().collect();
    opened
        .compositor
        .upload_rgba8(width, height, &bytes)
        .expect("invariant: the image bytes match")
}

mod tests {
    use super::{image, read};
    use crate::error::RenderError;
    use crate::filter::{Blend, Ctx, Draw, FilterClock, Pass, Shader, VideoFilter};
    use crate::image::Image;
    use crate::tests::{near, open, unorm_texture};
    use jstream_protocol::Settings;

    /// Copies its input through one scratch image, so the harness's two-draw path and the
    /// prelude's `src` are covered without any real kind.
    struct Copy;

    const COPY: Shader = Shader {
        label: "test-copy",
        source: "@fragment fn fs(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv); }",
        blend: Blend::Replace,
    };

    impl Pass for Copy {
        fn update(&mut self, _: &Settings) {}

        fn encode(
            &mut self,
            ctx: &mut Ctx<'_>,
            input: &Image,
            output: &Image,
        ) -> Result<(), RenderError> {
            let mid = ctx.scratch(input.width(), input.height())?;
            let params = super::super::params::Uniforms::new().finish();
            for (from, to) in [(input, &mid), (&mid, output)] {
                ctx.draw(&Draw {
                    shader: COPY,
                    input: from,
                    aux: None,
                    params: &params,
                    target: to,
                })?;
            }
            Ok(())
        }
    }

    #[test]
    fn a_two_pass_copy_round_trips_through_the_harness() {
        let Some(mut opened) = open() else {
            return;
        };
        const R: [u8; 4] = [255, 0, 0, 255];
        const C: [u8; 4] = [0, 0, 255, 128];
        let input = image(&opened, 2, &[R, C, C, R]);
        let mut filter = VideoFilter::of("test-copy", Box::new(Copy));
        let output = opened
            .compositor
            .apply_filter(&mut filter, &input, FilterClock::default())
            .expect("apply_filter");
        assert_eq!((output.width(), output.height()), (2, 2));
        let snapshot = read(&mut opened, &output);
        near(&snapshot, 0, 0, R, 1);
        near(&snapshot, 1, 0, C, 1);
        near(&snapshot, 0, 1, C, 1);
        near(&snapshot, 1, 1, R, 1);
    }

    /// A browser page's import (sRGB code values already multiplied by alpha) reaches
    /// filter code as premultiplied linear, so copying it through a filter changes nothing.
    #[test]
    fn a_premultiplied_import_filters_like_it_composites() {
        let Some(mut opened) = open() else {
            return;
        };
        // Straight sRGB red 128 at alpha 128, premultiplied in code values. Read as
        // premultiplied linear instead, it would come back as red 188.
        let texture = unorm_texture(&opened.compositor, 2, 2, [64, 0, 0, 128]);
        let input = opened
            .compositor
            .import_premultiplied(texture, None)
            .expect("a Unorm texture imports premultiplied");
        let mut filter = VideoFilter::of("test-copy", Box::new(Copy));
        let output = opened
            .compositor
            .apply_filter(&mut filter, &input, FilterClock::default())
            .expect("apply_filter");
        let snapshot = read(&mut opened, &output);
        near(&snapshot, 1, 1, [128, 0, 0, 128], 2);
    }

    /// `apply_filter_into` redraws a caller's layer frame after frame, and refuses an
    /// output of the wrong size, the input itself, or an image that isn't a layer.
    #[test]
    fn a_filter_redraws_into_the_same_layer() {
        let Some(mut opened) = open() else {
            return;
        };
        const R: [u8; 4] = [255, 0, 0, 255];
        const G: [u8; 4] = [0, 255, 0, 255];
        let mut filter = VideoFilter::of("test-copy", Box::new(Copy));
        let output = opened.compositor.layer(2, 1).expect("layer");
        for colour in [R, G] {
            let input = image(&opened, 2, &[colour, colour]);
            opened
                .compositor
                .apply_filter_into(&mut filter, &input, &output, FilterClock::default())
                .expect("apply_filter_into");
            let snapshot = read(&mut opened, &output);
            near(&snapshot, 0, 0, colour, 1);
            near(&snapshot, 1, 0, colour, 1);
        }
        let input = image(&opened, 2, &[R, R]);
        let wrong = opened.compositor.layer(3, 1).expect("layer");
        let refused = |result: Result<(), RenderError>| {
            assert!(
                matches!(result, Err(RenderError::Invalid { .. })),
                "{result:?}"
            );
        };
        let clock = FilterClock::default();
        refused(
            opened
                .compositor
                .apply_filter_into(&mut filter, &input, &wrong, clock),
        );
        refused(
            opened
                .compositor
                .apply_filter_into(&mut filter, &output, &output, clock),
        );
        let upload = image(&opened, 2, &[R, R]);
        refused(
            opened
                .compositor
                .apply_filter_into(&mut filter, &input, &upload, clock),
        );
    }

    /// The scratch layers `filter` keeps, by texture.
    fn kept(filter: &VideoFilter) -> Vec<wgpu::Texture> {
        filter
            .scratch()
            .kept()
            .iter()
            .map(|layer| layer.texture().clone())
            .collect()
    }

    /// V0 (#369): a filter's scratch layer is kept between calls at one size and redrawn
    /// each call, and an input of another size replaces it.
    #[test]
    fn a_scratch_layer_is_kept_and_redrawn_at_one_size() {
        let Some(mut opened) = open() else {
            return;
        };
        const R: [u8; 4] = [255, 0, 0, 255];
        const G: [u8; 4] = [0, 255, 0, 255];
        let mut filter = VideoFilter::of("test-copy", Box::new(Copy));
        let mut first = None;
        for colour in [R, G] {
            let input = image(&opened, 2, &[colour, colour]);
            let output = opened
                .compositor
                .apply_filter(&mut filter, &input, FilterClock::default())
                .expect("apply_filter");
            let snapshot = read(&mut opened, &output);
            near(&snapshot, 0, 0, colour, 1);
            near(&snapshot, 1, 0, colour, 1);
            let layers = kept(&filter);
            assert_eq!(layers.len(), 1, "one scratch layer");
            match &first {
                None => first = Some(layers),
                Some(first) => assert_eq!(&layers, first, "the same layer, redrawn"),
            }
        }
        let wider = image(&opened, 3, &[R, R, R]);
        let output = opened
            .compositor
            .apply_filter(&mut filter, &wider, FilterClock::default())
            .expect("apply_filter");
        near(&read(&mut opened, &output), 2, 0, R, 1);
        let layers = kept(&filter);
        assert_eq!(layers.len(), 1);
        assert_ne!(
            Some(&layers),
            first.as_ref(),
            "a new size makes a new layer"
        );
        let layer = &filter.scratch().kept()[0];
        assert_eq!((layer.width(), layer.height()), (3, 1));
    }

    /// V0 (#369): a real multi-pass kind's two scratch layers are different, are kept for
    /// the next call, and draw what the first call drew. A video delay's held frames are
    /// its own, not scratch, so a scratch layer can never overwrite one.
    #[test]
    fn a_multi_pass_kind_draws_the_same_from_kept_layers() {
        let Some(mut opened) = open() else {
            return;
        };
        const R: [u8; 4] = [255, 0, 0, 255];
        const T: [u8; 4] = [0, 0, 0, 0];
        let mut outline = super::filter("outline", serde_json::json!({"width": 2.0}));
        let input = image(
            &opened,
            4,
            &[T, T, T, T, T, R, R, T, T, R, R, T, T, T, T, T],
        );
        let first = super::run(&mut opened, &mut outline, &input, 0.0);
        let layers = kept(&outline);
        assert_eq!(layers.len(), 2, "outline uses two scratch layers");
        assert_ne!(layers[0], layers[1], "two layers of one call are different");
        let again = super::run(&mut opened, &mut outline, &input, 0.0);
        assert_eq!(kept(&outline), layers, "kept for the next call");
        assert_eq!(again, first, "drawn the same from the kept layers");

        let mut delay = super::filter("video_delay", serde_json::json!({"delay_ms": 100}));
        for i in 0..4 {
            super::run(&mut opened, &mut delay, &input, f64::from(i) * 0.05);
        }
        assert!(kept(&delay).is_empty(), "held frames aren't scratch layers");
    }

    /// #369: a draw keeps its uniform buffers and bind group between calls. New settings
    /// rewrite the buffers in place and draw the new look; another input texture makes a
    /// new bind group but no new buffers. A multi-pass kind's second call makes nothing.
    #[test]
    fn a_draw_keeps_its_buffers_and_bind_group_between_calls() {
        use crate::filter::pass::Made;
        let Some(mut opened) = open() else {
            return;
        };
        const DARK: [u8; 4] = [64, 64, 64, 255];
        const G: [u8; 4] = [0, 255, 0, 255];
        let made = |filter: &VideoFilter| filter.scratch().made();
        let at = |buffers, bind_groups| Made {
            buffers,
            bind_groups,
        };
        let mut correction = super::filter("color_correction", serde_json::json!({}));
        let input = image(&opened, 2, &[DARK, DARK]);
        let first = super::run(&mut opened, &mut correction, &input, 0.0);
        near(&first, 0, 0, DARK, 1);
        assert_eq!(
            made(&correction),
            at(2, 1),
            "one draw: two buffers, one bind group"
        );
        let again = super::run(&mut opened, &mut correction, &input, 0.5);
        assert_eq!(again, first, "the same draw from the kept slot");
        assert_eq!(
            made(&correction),
            at(2, 1),
            "nothing made for the same input"
        );

        correction.set_settings(&super::settings(
            "color_correction",
            serde_json::json!({"brightness": 0.25}),
        ));
        let brighter = super::run(&mut opened, &mut correction, &input, 0.0);
        let lit = brighter.pixel(0, 0).expect("in the snapshot");
        assert!(lit[0] > DARK[0] + 20, "new settings draw: {lit:?}");
        assert_eq!(
            made(&correction),
            at(2, 1),
            "new settings rewrite the buffers"
        );

        let other = image(&opened, 2, &[G, G]);
        let green = super::run(&mut opened, &mut correction, &other, 0.0);
        let mut fresh = super::filter("color_correction", serde_json::json!({"brightness": 0.25}));
        let expected = super::run(&mut opened, &mut fresh, &other, 0.0);
        assert_eq!(
            green, expected,
            "the kept slot draws the other input as a new one does"
        );
        assert_eq!(
            made(&correction),
            at(2, 2),
            "another input: a new bind group only"
        );

        let mut outline = super::filter("outline", serde_json::json!({"width": 2.0}));
        let _ = super::run(&mut opened, &mut outline, &input, 0.0);
        let after_first = made(&outline);
        assert!(after_first.buffers > 2, "outline draws more than once");
        let _ = super::run(&mut opened, &mut outline, &input, 0.0);
        assert_eq!(
            made(&outline),
            after_first,
            "a steady multi-pass call makes nothing"
        );
    }

    /// Every kind is a unique `video_filter` with valid defaults and a pass that its
    /// defaults build.
    #[test]
    fn every_kind_is_described_and_built_from_its_defaults() {
        use jstream_protocol::KindCategory;
        let kinds = crate::filter::descriptors();
        let mut ids: Vec<&str> = kinds.iter().map(|k| k.kind_id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), kinds.len(), "kind IDs are unique");
        for kind in &kinds {
            assert_eq!(kind.category, KindCategory::VideoFilter, "{}", kind.kind_id);
            for p in &kind.params {
                assert_eq!(p.check(&p.default), Ok(()), "{}.{}", kind.kind_id, p.id);
            }
            let settings = super::settings(&kind.kind_id, serde_json::json!({}));
            assert!(
                VideoFilter::new(&kind.kind_id, &settings).is_some(),
                "{} builds",
                kind.kind_id
            );
        }
    }
}
