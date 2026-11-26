//! Model goldens (ADR-0107): built-in shapes drawn into a layer and placed with
//! [`Placement::Layer`], read back and checked pixel by pixel against where the shared
//! placement ([`jstream_protocol::Solid`]) says each face lands.

use jstream_media::FrameSize;
use jstream_media::model::shapes::Shape;
use jstream_media::model::{AlphaMode, Material, Model, ModelTexture, TextureColor, TextureRef};
use jstream_protocol::{Canvas, Finite32, ItemGeometry, SceneCamera, Size, Vec2, Vec3};

use super::{LightKind, Lighting, MAX_SCENE_LIGHTS, ModelDraw, SceneLight};
use crate::tests::open;
use crate::{Color, Compositor, Crop, Item, Placement, Scene, Snapshot};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 100;

fn canvas() -> Canvas {
    Canvas {
        width: WIDTH,
        height: HEIGHT,
    }
}

fn boxed(x: f32, y: f32, size: f32) -> ItemGeometry {
    ItemGeometry::try_new(
        Vec2::new(x, y).expect("finite"),
        Size::try_new(size, size).expect("positive"),
    )
    .expect("a box")
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).expect("finite")
}

/// A cube whose six faces are six unlit colours, by their outward normals.
const FRONT: [u8; 3] = [255, 0, 0];
const BACK: [u8; 3] = [255, 255, 0];
const LEFT: [u8; 3] = [0, 255, 0];
const RIGHT: [u8; 3] = [0, 255, 255];
const TOP: [u8; 3] = [0, 0, 255];
const BOTTOM: [u8; 3] = [255, 0, 255];

fn coloured_cube() -> Model {
    let cube = Shape::Cube
        .model(Material {
            unlit: true,
            ..Material::default()
        })
        .expect("the cube builds");
    let mut primitive = cube.primitives()[0].clone();
    for v in &mut primitive.vertices {
        let colour = match v.normal.map(|c| c.round() as i32) {
            [0, 0, 1] => FRONT,
            [0, 0, -1] => BACK,
            [-1, 0, 0] => LEFT,
            [1, 0, 0] => RIGHT,
            [0, -1, 0] => TOP,
            _ => BOTTOM,
        };
        let linear = colour.map(|c| if c == 0 { 0.0 } else { 1.0 });
        v.color = [linear[0], linear[1], linear[2], 1.0];
    }
    Model::new(vec![primitive], cube.materials().to_vec(), vec![]).expect("the cube builds")
}

/// Draws `model` placed by `geometry` into a layer, and the layer into a scene over black
/// (with `camera` if it is a 3D scene), and reads the scene back.
fn draw(
    compositor: &mut Compositor,
    model: &Model,
    geometry: ItemGeometry,
    camera: Option<SceneCamera>,
    lighting: Lighting,
) -> Snapshot {
    draw_lit(compositor, model, geometry, camera, lighting, None)
}

/// [`draw`], lit by `lights` (ADR-0143 decision 3).
fn draw_lit(
    compositor: &mut Compositor,
    model: &Model,
    geometry: ItemGeometry,
    camera: Option<SceneCamera>,
    lighting: Lighting,
    lights: Option<&[SceneLight]>,
) -> Snapshot {
    let uploaded = compositor.upload_model(model).expect("the model uploads");
    draw_uploaded(compositor, &uploaded, geometry, camera, lighting, lights)
}

/// [`draw_lit`], of a model uploaded already.
fn draw_uploaded(
    compositor: &mut Compositor,
    uploaded: &crate::GpuModel,
    geometry: ItemGeometry,
    camera: Option<SceneCamera>,
    lighting: Lighting,
    lights: Option<&[SceneLight]>,
) -> Snapshot {
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(
            &ModelDraw {
                model: uploaded,
                geometry,
                camera,
                lighting,
                lights,
                shadows: &[],
                environment: None,
                floor: None,
                materials: &[],
                textures: &[],
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [Item {
        camera,
        ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
    }];
    render(compositor, &items)
}

fn render(compositor: &mut Compositor, items: &[Item<'_>]) -> Snapshot {
    let scene = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::rgb(0.0, 0.0, 0.0),
        items,
    };
    compositor
        .render(&scene, true)
        .expect("the scene renders")
        .pixels()
        .expect("readback was requested")
        .clone()
}

fn rgb(snapshot: &Snapshot, x: u32, y: u32) -> [u8; 3] {
    let p = snapshot.pixel(x, y).expect("inside the canvas");
    [p[0], p[1], p[2]]
}

fn assert_near(got: [u8; 3], want: [u8; 3], at: (u32, u32)) {
    let far = (0..3).any(|c| got[c].abs_diff(want[c]) > 6);
    assert!(!far, "at {at:?}: got {got:?}, want {want:?}");
}

fn count(snapshot: &Snapshot, colour: [u8; 3]) -> usize {
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let got = rgb(snapshot, x, y);
            (0..3).all(|c| got[c].abs_diff(colour[c]) <= 6)
        })
        .count()
}

/// ADR-0107 decision 3: in a 2D scene a cube is seen orthographically, straight on, so its
/// front face fits the box exactly and nothing else shows.
#[test]
fn golden_model_a_cube_in_a_2d_scene_shows_its_front_face_on_the_box() {
    let Some(mut opened) = open() else {
        return;
    };
    let shot = draw(
        &mut opened.compositor,
        &coloured_cube(),
        boxed(80.0, 50.0, 40.0),
        None,
        Lighting::Studio,
    );
    for at in [(62, 32), (80, 50), (97, 67)] {
        assert_near(rgb(&shot, at.0, at.1), FRONT, at);
    }
    for at in [(57, 50), (103, 50), (80, 27), (80, 73)] {
        assert_near(rgb(&shot, at.0, at.1), [0, 0, 0], at);
    }
    let front = count(&shot, FRONT);
    assert!(
        (1520..=1680).contains(&front),
        "{front} front pixels, want about 1600"
    );
}

/// ADR-0107 decisions 3 and 6: turned, the cube shows the side that turns toward the
/// viewer, where the shared placement puts it, and its own depth test hides its back and
/// the side turned away, which a painter's order would get wrong.
#[test]
fn golden_model_a_turned_cube_shows_its_near_side_and_hides_its_far_faces() {
    let Some(mut opened) = open() else {
        return;
    };
    let mut geometry = boxed(80.0, 50.0, 40.0);
    // Positive turn moves +x away, so the left side turns toward the viewer. In
    // orthographic view the left face spans x' from -27.3 to -7.3 px, the front -7.3 to
    // 27.3.
    geometry.turn_deg = finite(30.0);
    let shot = draw(
        &mut opened.compositor,
        &coloured_cube(),
        geometry,
        None,
        Lighting::Studio,
    );
    assert_near(rgb(&shot, 80 - 17, 50), LEFT, (63, 50));
    assert_near(rgb(&shot, 80 + 10, 50), FRONT, (90, 50));
    for (name, hidden) in [
        ("back", BACK),
        ("right", RIGHT),
        ("top", TOP),
        ("bottom", BOTTOM),
    ] {
        assert_eq!(count(&shot, hidden), 0, "the {name} face shows");
    }
}

/// ADR-0107 decision 3: seen through a scene camera from above, the near faces (front and
/// top) hide the far ones, and the picture agrees with the shared placement's corners.
#[test]
fn golden_model_through_a_camera_the_near_faces_hide_the_far_ones() {
    let Some(mut opened) = open() else {
        return;
    };
    let framing = SceneCamera::framing(canvas());
    let camera = SceneCamera {
        position: Vec3::new(80.0, -60.0, framing.position.z.get()).expect("finite"),
        ..framing
    };
    let geometry = boxed(80.0, 50.0, 40.0);
    let shot = draw(
        &mut opened.compositor,
        &coloured_cube(),
        geometry,
        Some(camera),
        Lighting::Studio,
    );
    assert!(count(&shot, FRONT) > 500, "the front face is missing");
    assert!(count(&shot, TOP) > 100, "the top face is missing");
    for (name, hidden) in [("back", BACK), ("bottom", BOTTOM)] {
        assert_eq!(count(&shot, hidden), 0, "the {name} face shows");
    }
    // The front face's centre lands where the shared placement says.
    let view = camera.view(canvas()).expect("a valid camera");
    let bounds = coloured_cube().bounds();
    let bounds = jstream_protocol::SolidBounds {
        min: bounds.min,
        max: bounds.max,
    };
    let solid = geometry.solid(bounds, Some(&view)).expect("placed");
    let jstream_protocol::Placed::At(centre) = solid.place([0.0, 0.0, 0.5]) else {
        panic!("behind the camera");
    };
    let at = (centre.x.round() as u32, centre.y.round() as u32);
    assert_near(rgb(&shot, at.0, at.1), FRONT, at);
}

/// A model's layer knows the box its model can cover (#1070), and so does the depth
/// layer drawn beside it, so a scene placing them draws only there: the box is well
/// inside the layer for a small model, and the scene looks as it does when the layer
/// is drawn whole. A model whose bounds reach behind the viewer has no box.
#[test]
fn a_models_layer_is_bounded_by_its_models_box() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = Some(SceneCamera::framing(canvas()));
    let geometry = boxed(80.0, 50.0, 30.0);
    let uploaded = compositor
        .upload_model(&coloured_cube())
        .expect("the model uploads");
    let model_draw = |geometry| ModelDraw {
        model: &uploaded,
        geometry,
        camera,
        lighting: Lighting::Studio,
        lights: None,
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(&model_draw(geometry), &layer)
        .expect("the model draws");
    let content = layer.content().expect("the model's box is known");
    assert!(
        content.width > 20 && content.width < WIDTH / 2 && content.height < HEIGHT,
        "{content:?}"
    );
    let items = [Item {
        camera,
        ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
    }];
    let bounded = render(compositor, &items);
    // Forgetting the box draws the whole layer.
    layer.mark_changed();
    assert_eq!(layer.content(), None);
    let whole = render(compositor, &items);
    assert_eq!(bounded.rgba(), whole.rgba());
    assert!(count(&bounded, FRONT) > 300, "the cube shows");

    // Its depth, over a cleared layer, has the same box.
    let depth = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
    let empty = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &[],
    };
    compositor
        .render_view_depth(&empty, &depth)
        .expect("it clears");
    compositor
        .draw_model_depth(&model_draw(geometry), &depth)
        .expect("the depth draws");
    assert_eq!(depth.content(), Some(content));

    // Large enough to reach past the viewer, the model's bounds have no box.
    compositor
        .draw_model(&model_draw(boxed(80.0, 50.0, 5000.0)), &layer)
        .expect("the model draws");
    assert_eq!(layer.content(), None);
}

/// ADR-0107 decision 3: a flipped placement mirrors the model but still draws its front.
#[test]
fn golden_model_a_mirrored_cube_still_shows_its_front() {
    let Some(mut opened) = open() else {
        return;
    };
    let mut geometry = boxed(80.0, 50.0, 40.0);
    geometry.flip_h = true;
    geometry.turn_deg = finite(30.0);
    let shot = draw(
        &mut opened.compositor,
        &coloured_cube(),
        geometry,
        None,
        Lighting::Studio,
    );
    // Mirrored, the cube's right face is on its left, so it is the side the turn shows,
    // and the front is still drawn rather than discarded as a back face.
    assert_near(rgb(&shot, 80 - 17, 50), RIGHT, (63, 50));
    assert_near(rgb(&shot, 80 + 10, 50), FRONT, (90, 50));
    for (name, hidden) in [("back", BACK), ("left", LEFT)] {
        assert_eq!(count(&shot, hidden), 0, "the {name} face shows");
    }
}

/// ADR-0107 decision 3: a texture reads upright, (0, 0) at its top-left, on a plane.
#[test]
fn golden_model_a_textured_plane_reads_upright() {
    let Some(mut opened) = open() else {
        return;
    };
    // 8x8 texels, a solid colour in each 4x4 quadrant: red, green, blue, white.
    let quadrant = |x: u32, y: u32| match (x < 4, y < 4) {
        (true, true) => [255, 0, 0, 255],
        (false, true) => [0, 255, 0, 255],
        (true, false) => [0, 0, 255, 255],
        (false, false) => [255, 255, 255, 255],
    };
    let rgba: Vec<u8> = (0..8)
        .flat_map(|y| (0..8).flat_map(move |x| quadrant(x, y)))
        .collect();
    let texture = ModelTexture::new(8, 8, rgba, TextureColor::Srgb).expect("a texture");
    let plane = Shape::Plane
        .model(Material::default())
        .expect("the plane builds");
    let material = Material {
        base_color_texture: Some(TextureRef { texture: 0 }),
        unlit: true,
        ..plane.materials()[0].clone()
    };
    let model = Model::new(plane.primitives().to_vec(), vec![material], vec![texture])
        .expect("the plane builds");
    let shot = draw(
        &mut opened.compositor,
        &model,
        boxed(80.0, 50.0, 80.0),
        None,
        Lighting::Studio,
    );
    assert_near(rgb(&shot, 60, 30), [255, 0, 0], (60, 30));
    assert_near(rgb(&shot, 100, 30), [0, 255, 0], (100, 30));
    assert_near(rgb(&shot, 60, 70), [0, 0, 255], (60, 70));
    assert_near(rgb(&shot, 100, 70), [255, 255, 255], (100, 70));
}

/// ADR-0177 decision 1: a model another thread uploaded for a compositor draws as one
/// the compositor uploaded itself, and a texture level written in more than one band
/// (2048 px wide, so four of 512 rows) lands whole.
#[test]
fn golden_model_one_uploaded_on_another_thread_draws_the_same() {
    let Some(mut opened) = open() else {
        return;
    };
    const SIDE: u32 = 2048;
    // A solid colour in each quadrant: red, green, blue, white.
    let quadrant = |x: u32, y: u32| match (x < SIDE / 2, y < SIDE / 2) {
        (true, true) => [255, 0, 0, 255],
        (false, true) => [0, 255, 0, 255],
        (true, false) => [0, 0, 255, 255],
        (false, false) => [255, 255, 255, 255],
    };
    let rgba: Vec<u8> = (0..SIDE)
        .flat_map(|y| (0..SIDE).flat_map(move |x| quadrant(x, y)))
        .collect();
    let texture = ModelTexture::new(SIDE, SIDE, rgba, TextureColor::Srgb).expect("a texture");
    let plane = Shape::Plane
        .model(Material::default())
        .expect("the plane builds");
    let material = Material {
        base_color_texture: Some(TextureRef { texture: 0 }),
        unlit: true,
        ..plane.materials()[0].clone()
    };
    let model = Model::new(plane.primitives().to_vec(), vec![material], vec![texture])
        .expect("the plane builds");
    let uploader = opened.compositor.model_uploader();
    let aside = std::thread::scope(|scope| {
        scope
            .spawn(|| uploader.upload(&model))
            .join()
            .expect("the upload doesn't panic")
    })
    .expect("the model uploads");
    let geometry = boxed(80.0, 50.0, 80.0);
    let compositor = &mut opened.compositor;
    let shot = draw_uploaded(compositor, &aside, geometry, None, Lighting::Studio, None);
    assert_near(rgb(&shot, 60, 30), [255, 0, 0], (60, 30));
    assert_near(rgb(&shot, 100, 30), [0, 255, 0], (100, 30));
    assert_near(rgb(&shot, 60, 70), [0, 0, 255], (60, 70));
    assert_near(rgb(&shot, 100, 70), [255, 255, 255], (100, 70));
    let here = draw(compositor, &model, geometry, None, Lighting::Studio);
    assert_eq!(shot.rgba(), here.rgba(), "uploaded here, it draws the same");
}

/// ADR-0107 decision 4: the studio key light is above and to the left, so a sphere is
/// brighter there than below and to the right; unlit, it is the same everywhere.
#[test]
fn golden_model_the_studio_key_light_is_upper_left() {
    let Some(mut opened) = open() else {
        return;
    };
    let sphere = Shape::Sphere
        .model(Material {
            base_color: [0.5, 0.5, 0.5, 1.0],
            roughness: 0.8,
            ..Material::default()
        })
        .expect("the sphere builds");
    let geometry = boxed(80.0, 50.0, 80.0);
    let lit = draw(
        &mut opened.compositor,
        &sphere,
        geometry,
        None,
        Lighting::Studio,
    );
    let luma = |p: [u8; 3]| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]);
    let upper_left = luma(rgb(&lit, 65, 35));
    let lower_right = luma(rgb(&lit, 95, 65));
    assert!(
        upper_left > lower_right + 60,
        "{upper_left} against {lower_right}"
    );
    let flat = draw(
        &mut opened.compositor,
        &sphere,
        geometry,
        None,
        Lighting::Unlit,
    );
    assert_eq!(rgb(&flat, 65, 35), rgb(&flat, 95, 65));
    // Linear 0.5 is sRGB 188.
    assert_near(rgb(&flat, 80, 50), [188, 188, 188], (80, 50));
}

/// ADR-0107 decision 6: a blended material's alpha composites it over what is behind.
#[test]
fn golden_model_a_blended_material_is_see_through() {
    let Some(mut opened) = open() else {
        return;
    };
    let plane = Shape::Plane
        .model(Material {
            base_color: [1.0, 1.0, 1.0, 0.5],
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..Material::default()
        })
        .expect("the plane builds");
    let shot = draw(
        &mut opened.compositor,
        &plane,
        boxed(80.0, 50.0, 40.0),
        None,
        Lighting::Studio,
    );
    assert_near(rgb(&shot, 80, 50), [188, 188, 188], (80, 50));
}

/// ADR-0050's near plane holds for models: a double-sided floor that runs under the camera
/// draws its part in front and nothing of the part behind, without failing.
#[test]
fn golden_model_the_near_plane_clips_a_floor_under_the_camera() {
    let Some(mut opened) = open() else {
        return;
    };
    let framing = SceneCamera::framing(canvas());
    let floor = Shape::Plane
        .model(Material {
            unlit: true,
            double_sided: true,
            ..Material::default()
        })
        .expect("the plane builds");
    // A floor below the camera's eye, far longer than the distance to the camera, laid
    // flat (tilted 90°) and reaching past it.
    let mut geometry = boxed(80.0, 80.0, 4000.0);
    geometry.tilt_deg = finite(90.0);
    let shot = draw(
        &mut opened.compositor,
        &floor,
        geometry,
        Some(framing),
        Lighting::Studio,
    );
    // The floor fills the bottom of the picture and stops at its horizon.
    assert_near(
        rgb(&shot, 80, HEIGHT - 2),
        [255, 255, 255],
        (80, HEIGHT - 2),
    );
    assert_near(rgb(&shot, 80, 2), [0, 0, 0], (80, 2));
}

/// ADR-0107 decision 6: a model's layer sorts with a scene's cards at the model's centre,
/// whatever the stacking order.
#[test]
fn golden_model_a_layer_sorts_with_cards_by_the_models_depth() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = SceneCamera::framing(canvas());
    let white = compositor
        .upload_rgba8(1, 1, &[255, 255, 255, 255])
        .expect("a pixel uploads");
    let cube = compositor.upload_model(&coloured_cube()).expect("uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    for (model_depth, card_wins) in [(40.0, false), (-40.0, true)] {
        let mut geometry = boxed(80.0, 50.0, 30.0);
        geometry.depth_px = finite(model_depth);
        compositor
            .draw_model(
                &ModelDraw {
                    model: &cube,
                    geometry,
                    camera: Some(camera),
                    lighting: Lighting::Unlit,
                    lights: None,
                    shadows: &[],
                    environment: None,
                    floor: None,
                    materials: &[],
                    textures: &[],
                    reflection: None,
                    probe: None,
                },
                &layer,
            )
            .expect("the model draws");
        let card = boxed(80.0, 50.0, 60.0);
        // The model is first in stacking order, so only its depth can put it on top.
        let items = [
            Item {
                camera: Some(camera),
                ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
            },
            Item {
                camera: Some(camera),
                ..crate::tests::item(&white, Placement::Geometry(card), Crop::NONE, 1.0)
            },
        ];
        let shot = render(compositor, &items);
        let want = if card_wins { [255, 255, 255] } else { FRONT };
        assert_near(rgb(&shot, 80, 50), want, (80, 50));
    }
}

/// ADR-0107 decision 7 (#1601): a card seen through the camera, drawn after a nearer
/// model, is hidden where the model is. An item on the screen itself (no camera) is not
/// in the model's space and is never hidden by it.
#[test]
fn golden_model_depth_hides_a_camera_card_but_not_a_screen_item() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = SceneCamera::framing(canvas());
    let white = compositor
        .upload_rgba8(1, 1, &[255, 255, 255, 255])
        .expect("a pixel uploads");
    let cube = compositor.upload_model(&coloured_cube()).expect("uploads");
    let mut geometry = boxed(80.0, 50.0, 30.0);
    geometry.depth_px = finite(40.0);
    let draw = ModelDraw {
        model: &cube,
        geometry,
        camera: Some(camera),
        lighting: Lighting::Unlit,
        lights: None,
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let own_depth = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(&draw, &layer)
        .expect("the model draws");
    compositor
        .draw_model_depth(&draw, &own_depth)
        .expect("the model's depth draws");
    for (card_camera, want) in [(Some(camera), FRONT), (None, [255, 255, 255])] {
        let items = [
            Item {
                camera: Some(camera),
                depth: Some(&own_depth),
                ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
            },
            Item {
                camera: card_camera,
                ..crate::tests::item(
                    &white,
                    Placement::Geometry(boxed(80.0, 50.0, 60.0)),
                    Crop::NONE,
                    1.0,
                )
            },
        ];
        let shot = render(compositor, &items);
        assert_near(rgb(&shot, 80, 50), want, (80, 50));
    }
}

/// A model drawn by one compositor is refused by another.
#[test]
fn a_model_from_another_compositor_is_refused() {
    let Some(mut opened) = open() else {
        return;
    };
    let other = opened.compositor.sibling();
    let model = other.upload_model(&coloured_cube()).expect("uploads");
    let layer = opened.compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let drawn = opened.compositor.draw_model(
        &ModelDraw {
            model: &model,
            geometry: boxed(80.0, 50.0, 40.0),
            camera: None,
            lighting: Lighting::Studio,
            lights: None,
            shadows: &[],
            environment: None,
            floor: None,
            materials: &[],
            textures: &[],
            reflection: None,
            probe: None,
        },
        &layer,
    );
    assert!(matches!(
        drawn,
        Err(crate::RenderError::Invalid { what: "model", .. })
    ));
}

/// ADR-0107 decisions 2 and 3: a normal map's green channel tilts the surface up its
/// texture, toward the key light above, whether or not the placement mirrors the model;
/// a double-sided surface seen from behind is turned over whole, its tilt with it.
#[test]
fn golden_model_a_normal_map_tilts_the_same_way_mirrored_and_from_behind() {
    let Some(mut opened) = open() else {
        return;
    };
    let plane = Shape::Plane
        .model(Material {
            base_color: [0.5, 0.5, 0.5, 1.0],
            roughness: 0.9,
            double_sided: true,
            ..Material::default()
        })
        .expect("the plane builds");
    // A plane whose every texel has one tangent-space normal, `green` its y.
    let mapped = |green: u8| {
        let rgba: Vec<u8> = (0..16).flat_map(|_| [128u8, green, 184, 255]).collect();
        let normals = ModelTexture::new(4, 4, rgba, TextureColor::Linear).expect("a texture");
        Model::new(
            plane.primitives().to_vec(),
            vec![Material {
                normal_texture: Some(TextureRef { texture: 0 }),
                ..plane.materials()[0].clone()
            }],
            vec![normals],
        )
        .expect("the plane builds")
    };
    // (0, 0.9, 0.44) tilts up the texture, toward the key light; (0, -0.9, 0.44) down.
    let (up, down) = (mapped(242), mapped(13));
    let luma = |shot: &Snapshot| {
        let p = rgb(shot, 80, 50);
        u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])
    };
    let geometry = boxed(80.0, 50.0, 80.0);
    let mut mirrored = geometry;
    mirrored.flip_h = true;
    let compositor = &mut opened.compositor;
    let plain = luma(&draw(compositor, &up, geometry, None, Lighting::Studio));
    let away = luma(&draw(compositor, &down, geometry, None, Lighting::Studio));
    let flipped = luma(&draw(compositor, &up, mirrored, None, Lighting::Studio));
    assert!(plain > away + 150, "tilted up {plain} against down {away}");
    assert!(
        plain.abs_diff(flipped) <= 6,
        "{plain} against {flipped} mirrored"
    );
    // Turned to show its back, the whole surface is turned over: a bump tilted up the
    // front tilts down at the back, so the back of `up` is lit like the front of `down`.
    let mut behind = geometry;
    behind.turn_deg = finite(180.0);
    let back_up = luma(&draw(compositor, &up, behind, None, Lighting::Studio));
    let back_down = luma(&draw(compositor, &down, behind, None, Lighting::Studio));
    assert!(
        back_up.abs_diff(away) <= 6,
        "back of up {back_up}, front of down {away}"
    );
    assert!(
        back_down.abs_diff(plain) <= 6,
        "back of down {back_down}, front of up {plain}"
    );
    // With no normal map the back of a flat plane is lit as its front is.
    let front = luma(&draw(compositor, &plane, geometry, None, Lighting::Studio));
    let back = luma(&draw(compositor, &plane, behind, None, Lighting::Studio));
    assert!(front.abs_diff(back) <= 6, "front {front}, back {back}");
}

/// Layers of different sizes each get a depth target of their own size: a pass's
/// attachments must match, so a smaller layer after a larger one still draws.
#[test]
fn a_model_draws_into_layers_of_different_sizes() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let model = compositor.upload_model(&coloured_cube()).expect("uploads");
    for (w, h) in [(WIDTH, HEIGHT), (WIDTH / 2, HEIGHT / 2), (HEIGHT, WIDTH)] {
        let layer = compositor.layer(w, h).expect("a layer");
        compositor
            .draw_model(
                &ModelDraw {
                    model: &model,
                    geometry: boxed(w as f32 / 2.0, h as f32 / 2.0, 30.0),
                    camera: None,
                    lighting: Lighting::Unlit,
                    lights: None,
                    shadows: &[],
                    environment: None,
                    floor: None,
                    materials: &[],
                    textures: &[],
                    reflection: None,
                    probe: None,
                },
                &layer,
            )
            .expect("the model draws");
        // A bad pass would surface as an uncaptured error on the next render.
        render(compositor, &[]);
    }
    let shot = draw(
        compositor,
        &coloured_cube(),
        boxed(80.0, 50.0, 40.0),
        None,
        Lighting::Unlit,
    );
    assert_near(rgb(&shot, 80, 50), FRONT, (80, 50));
}

/// A primitive bigger than one of the adapter's buffers is refused, not a wgpu panic.
#[test]
fn a_primitive_past_the_buffer_limit_is_refused() {
    let Some(mut opened) = open() else {
        return;
    };
    opened.compositor.cap_buffer_size(1024);
    let sphere = Shape::Sphere
        .model(Material::default())
        .expect("the sphere builds");
    assert!(matches!(
        opened.compositor.upload_model(&sphere),
        Err(crate::RenderError::Invalid { what: "model", .. })
    ));
}

/// A grey, rough sphere filling an 80 px box in the middle of the canvas.
fn grey_sphere() -> Model {
    Shape::Sphere
        .model(Material {
            base_color: [0.5, 0.5, 0.5, 1.0],
            roughness: 0.8,
            ..Material::default()
        })
        .expect("the sphere builds")
}

fn luma(p: [u8; 3]) -> u32 {
    u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])
}

fn point_light(position: [f64; 3], color: Color, intensity: f32) -> SceneLight {
    SceneLight {
        kind: LightKind::Point,
        position,
        aim: [0.0, 0.0, -1.0],
        color,
        intensity,
        range_px: 0.0,
        cone_deg: 45.0,
        softness: 0.2,
    }
}

fn ambient(intensity: f32) -> SceneLight {
    SceneLight {
        kind: LightKind::Ambient,
        intensity,
        ..point_light([0.0; 3], Color::WHITE, 1.0)
    }
}

/// Draws the grey sphere through the framing camera, so it lands on its 2D box, lit by
/// `lights`.
fn lit_sphere(compositor: &mut Compositor, lights: &[SceneLight]) -> Snapshot {
    draw_lit(
        compositor,
        &grey_sphere(),
        boxed(80.0, 50.0, 80.0),
        Some(SceneCamera::framing(canvas())),
        Lighting::Studio,
        Some(lights),
    )
}

/// ADR-0143 decision 3: scene lights replace the studio rig. A point light to the right
/// and in front lights the sphere's right side and leaves its left side dark; with no
/// light at all, a sphere that emits nothing is black.
#[test]
fn golden_model_a_point_light_lights_the_side_that_faces_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let light = point_light([400.0, 50.0, 100.0], Color::WHITE, 3.0);
    let shot = lit_sphere(&mut opened.compositor, &[light]);
    let right = luma(rgb(&shot, 110, 50));
    let left = luma(rgb(&shot, 50, 50));
    assert!(right > 150, "the lit side reads {right}");
    assert!(left < 10, "the far side reads {left}");
    let dark = lit_sphere(&mut opened.compositor, &[]);
    assert_near(rgb(&dark, 80, 50), [0, 0, 0], (80, 50));
    assert_near(rgb(&dark, 110, 50), [0, 0, 0], (110, 50));
}

/// ADR-0143 decision 3: a light's colour tints what it lights.
#[test]
fn golden_model_a_light_colour_tints_the_model() {
    let Some(mut opened) = open() else {
        return;
    };
    let red = point_light([80.0, 50.0, 400.0], Color::rgb(1.0, 0.0, 0.0), 3.0);
    let shot = lit_sphere(&mut opened.compositor, &[red]);
    let [r, g, b] = rgb(&shot, 80, 50);
    assert!(r > 120 && g < 8 && b < 8, "{:?}", [r, g, b]);
}

/// ADR-0143 decision 3: past its range a point light lights nothing, and inside it the
/// light fades with distance.
#[test]
fn golden_model_a_point_light_fades_to_nothing_at_its_range() {
    let Some(mut opened) = open() else {
        return;
    };
    let at = [80.0, 50.0, 400.0];
    let far = lit_sphere(
        &mut opened.compositor,
        &[SceneLight {
            range_px: 200.0,
            ..point_light(at, Color::WHITE, 3.0)
        }],
    );
    assert_near(rgb(&far, 80, 50), [0, 0, 0], (80, 50));
    let near = lit_sphere(
        &mut opened.compositor,
        &[SceneLight {
            range_px: 800.0,
            ..point_light(at, Color::WHITE, 3.0)
        }],
    );
    let unfaded = lit_sphere(
        &mut opened.compositor,
        &[point_light(at, Color::WHITE, 3.0)],
    );
    let (near, unfaded) = (luma(rgb(&near, 80, 50)), luma(rgb(&unfaded, 80, 50)));
    assert!(near > 30 && near + 30 < unfaded, "{near} against {unfaded}");
}

/// ADR-0143 decision 3: a spot light lights what is inside its cone and not what is
/// outside it, and a directional light lights from its aim.
#[test]
fn golden_model_a_spot_light_lights_only_inside_its_cone() {
    let Some(mut opened) = open() else {
        return;
    };
    let plane = Shape::Plane
        .model(Material {
            base_color: [0.6, 0.6, 0.6, 1.0],
            roughness: 1.0,
            double_sided: true,
            ..Material::default()
        })
        .expect("the plane builds");
    let mut wide = boxed(80.0, 50.0, 100.0);
    wide.size = Size::try_new(160.0, 100.0).expect("positive");
    wide.fit_mode = jstream_protocol::FitMode::Stretch;
    let spot = SceneLight {
        kind: LightKind::Spot,
        aim: [0.0, 0.0, -1.0],
        cone_deg: 20.0,
        softness: 0.0,
        ..point_light([80.0, 50.0, 200.0], Color::WHITE, 3.0)
    };
    let camera = Some(SceneCamera::framing(canvas()));
    let shot = draw_lit(
        &mut opened.compositor,
        &plane,
        wide,
        camera,
        Lighting::Studio,
        Some(&[spot]),
    );
    // tan(10°) · 200 ≈ 35 px: the middle is lit, a point 60 px off it is not.
    assert!(luma(rgb(&shot, 80, 50)) > 150, "{:?}", rgb(&shot, 80, 50));
    assert_near(rgb(&shot, 140, 50), [0, 0, 0], (140, 50));
    let sun = SceneLight {
        kind: LightKind::Directional,
        aim: [0.0, 0.0, -1.0],
        ..point_light([0.0; 3], Color::WHITE, 3.0)
    };
    let shot = draw_lit(
        &mut opened.compositor,
        &plane,
        wide,
        camera,
        Lighting::Studio,
        Some(&[sun]),
    );
    assert!(luma(rgb(&shot, 140, 50)) > 150, "{:?}", rgb(&shot, 140, 50));
    let away = SceneLight {
        aim: [0.0, 0.0, 1.0],
        ..sun
    };
    let shot = draw_lit(
        &mut opened.compositor,
        &plane,
        wide,
        camera,
        Lighting::Studio,
        Some(&[away]),
    );
    assert_near(rgb(&shot, 140, 50), [0, 0, 0], (140, 50));
}

/// ADR-0143 decision 3: ambient lights add up and scale the environment light, with no
/// direction of their own.
#[test]
fn golden_model_ambient_lights_add_up() {
    let Some(mut opened) = open() else {
        return;
    };
    let one = lit_sphere(&mut opened.compositor, &[ambient(1.0)]);
    let two = lit_sphere(&mut opened.compositor, &[ambient(1.0), ambient(1.0)]);
    let doubled = lit_sphere(&mut opened.compositor, &[ambient(2.0)]);
    assert!(luma(rgb(&one, 80, 50)) > 30, "{:?}", rgb(&one, 80, 50));
    assert!(luma(rgb(&two, 80, 50)) > luma(rgb(&one, 80, 50)) + 20);
    assert_near(rgb(&two, 80, 50), rgb(&doubled, 80, 50), (80, 50));
}

/// ADR-0143 decision 3: only the first [`MAX_SCENE_LIGHTS`] lights shade a model, and a
/// light at zero intensity (an item faded out) still takes its slot, so the ninth stays
/// off while one of the first eight fades. Ambient lights take no slot, and a light with
/// a non-finite value lights nothing.
#[test]
fn golden_model_lights_past_the_limit_are_ignored() {
    let Some(mut opened) = open() else {
        return;
    };
    let off = point_light([400.0, 50.0, 100.0], Color::WHITE, 0.0);
    let on = point_light([400.0, 50.0, 100.0], Color::WHITE, 3.0);
    for first in [off, ambient(0.0)] {
        let mut lights = vec![first; MAX_SCENE_LIGHTS];
        lights.push(on);
        let shot = lit_sphere(&mut opened.compositor, &lights);
        let lit = luma(rgb(&shot, 110, 50));
        if first.kind == LightKind::Ambient {
            assert!(lit > 150, "ambient lights take no slot: {lit}");
        } else {
            assert!(lit < 10, "the ninth stays off: {lit}");
        }
    }
    let broken = [
        SceneLight {
            position: [f64::NAN, 50.0, 100.0],
            ..on
        },
        SceneLight {
            intensity: f32::INFINITY,
            ..on
        },
        SceneLight {
            range_px: f32::NAN,
            ..on
        },
    ];
    for light in broken {
        let shot = lit_sphere(&mut opened.compositor, &[light]);
        assert_near(rgb(&shot, 110, 50), [0, 0, 0], (110, 50));
    }
}

/// ADR-0143 decision 3: lights belong to a 3D scene. Without a camera, and for an unlit
/// model, they are ignored.
#[test]
fn golden_model_lights_are_ignored_without_a_camera_or_unlit() {
    let Some(mut opened) = open() else {
        return;
    };
    let geometry = boxed(80.0, 50.0, 80.0);
    let lights = [point_light([400.0, 50.0, 100.0], Color::WHITE, 3.0)];
    for lighting in [Lighting::Studio, Lighting::Unlit] {
        let plain = draw(
            &mut opened.compositor,
            &grey_sphere(),
            geometry,
            None,
            lighting,
        );
        let with = draw_lit(
            &mut opened.compositor,
            &grey_sphere(),
            geometry,
            None,
            lighting,
            Some(&lights),
        );
        assert_eq!(rgb(&plain, 65, 35), rgb(&with, 65, 35), "{lighting:?}");
    }
    let camera = Some(SceneCamera::framing(canvas()));
    let unlit = draw_lit(
        &mut opened.compositor,
        &grey_sphere(),
        geometry,
        camera,
        Lighting::Unlit,
        Some(&lights),
    );
    assert_near(rgb(&unlit, 50, 50), [188, 188, 188], (50, 50));
}

/// ADR-0151 decision 3: a `scene`-lit wall behind a cube, lit by a directional light
/// from the left, is shadowed where the cube's shadow map puts the cube between it and
/// the light, and lit elsewhere and without the map; a card's map, drawn as the view
/// depth draws cards, shadows it the same way.
#[test]
fn golden_model_a_shadow_map_shadows_a_scene_lit_model() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = SceneCamera::framing(canvas());
    let white = |shape: Shape| {
        shape
            .model(Material {
                base_color: [1.0; 4],
                roughness: 1.0,
                ..Material::default()
            })
            .expect("the shape builds")
    };
    let wall = compositor
        .upload_model(&white(Shape::Plane))
        .expect("uploads");
    let cube = compositor
        .upload_model(&white(Shape::Cube))
        .expect("uploads");
    let mut wall_at = boxed(80.0, 50.0, 200.0);
    wall_at.fit_mode = jstream_protocol::FitMode::Stretch;
    wall_at.depth_px = finite(-60.0);
    let cube_at = boxed(60.0, 50.0, 20.0);
    // Shining right and into the scene at 45°: the cube's shadow falls 60 px to its
    // right on the wall, at x = 120 there, which the camera sees nearer the centre, at
    // about x = 105, since the wall is behind the canvas plane.
    let aim = [1.0f64, 0.0, -1.0].map(|c| c / 2f64.sqrt());
    let light = SceneLight {
        kind: LightKind::Directional,
        position: [0.0; 3],
        aim,
        color: Color::rgb(1.0, 1.0, 1.0),
        intensity: 2.0,
        range_px: 0.0,
        cone_deg: 45.0,
        softness: 0.0,
    };
    let lights = [light];
    let centre = [80.0f32, 50.0, -30.0];
    let from = [0, 1, 2].map(|i| centre[i] - aim[i] as f32 * 600.0);
    let light_camera = SceneCamera {
        position: Vec3::new(from[0], from[1], from[2]).expect("finite"),
        target: Vec3::new(centre[0], centre[1], centre[2]).expect("finite"),
        fov_deg: finite(30.0),
        roll_deg: Finite32::ZERO,
        depth_sort: false,
        focus_px: None,
        blur_px: Finite32::ZERO,
        autofocus: false,
        fixed_aperture: false,
    };
    let map = compositor.depth_layer(256, 256).expect("a map");
    let clear = Scene {
        canvas: FrameSize::new(256, 256).expect("non-zero"),
        background: Color::rgb(0.0, 0.0, 0.0),
        items: &[],
    };
    compositor
        .render_view_depth(&clear, &map)
        .expect("the map clears");
    compositor
        .draw_model_depth(
            &ModelDraw {
                model: &cube,
                geometry: cube_at,
                camera: Some(light_camera),
                lighting: Lighting::Scene,
                lights: None,
                shadows: &[],
                environment: None,
                floor: None,
                materials: &[],
                textures: &[],
                reflection: None,
                probe: None,
            },
            &map,
        )
        .expect("the cube casts");
    let card = compositor
        .upload_rgba8(1, 1, &[255, 255, 255, 255])
        .expect("a pixel uploads");
    let card_map = compositor.depth_layer(256, 256).expect("a map");
    let mut card_at = boxed(60.0, 50.0, 20.0);
    card_at.fit_mode = jstream_protocol::FitMode::Stretch;
    let cards = [Item {
        camera: Some(light_camera),
        ..crate::tests::item(&card, Placement::Geometry(card_at), Crop::NONE, 1.0)
    }];
    compositor
        .render_view_depth(
            &Scene {
                canvas: FrameSize::new(256, 256).expect("non-zero"),
                background: Color::rgb(0.0, 0.0, 0.0),
                items: &cards,
            },
            &card_map,
        )
        .expect("the card casts");
    let card_shadows = [super::ShadowMap {
        light: 0,
        image: &card_map,
        camera: light_camera,
        softness: 0.0,
    }];
    let shadows = [super::ShadowMap {
        light: 0,
        image: &map,
        camera: light_camera,
        softness: 0.0,
    }];
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let mut shot = |shadows: &[super::ShadowMap<'_>]| {
        compositor
            .draw_model(
                &ModelDraw {
                    model: &wall,
                    geometry: wall_at,
                    camera: Some(camera),
                    lighting: Lighting::Scene,
                    lights: Some(&lights),
                    shadows,
                    environment: None,
                    floor: None,
                    materials: &[],
                    textures: &[],
                    reflection: None,
                    probe: None,
                },
                &layer,
            )
            .expect("the wall draws");
        let items = [Item {
            camera: Some(camera),
            ..crate::tests::item(&layer, Placement::Layer(wall_at), Crop::NONE, 1.0)
        }];
        render(compositor, &items)
    };
    let lit = shot(&[]);
    let shadowed = shot(&shadows);
    let carded = shot(&card_shadows);
    let luma = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>();
    let (open_lit, open_shadowed) = (luma(rgb(&lit, 20, 50)), luma(rgb(&shadowed, 20, 50)));
    let (under_lit, under) = (luma(rgb(&lit, 105, 50)), luma(rgb(&shadowed, 105, 50)));
    assert!(under_lit > 150, "the wall is lit: {under_lit}");
    assert!(
        open_lit.abs_diff(open_shadowed) <= 6,
        "out of the shadow nothing changes: {open_lit} against {open_shadowed}"
    );
    assert!(
        under + 100 < under_lit,
        "the cube's shadow darkens the wall: {under} against {under_lit}"
    );
    let under_card = luma(rgb(&carded, 105, 50));
    assert!(
        under_card + 100 < under_lit,
        "a card's shadow darkens the wall: {under_card} against {under_lit}"
    );
}

/// Draws the grey sphere as [`lit_sphere`] does, lit by `lights` and `environment`.
fn sky_sphere(
    compositor: &mut Compositor,
    lights: &[SceneLight],
    environment: super::Environment<'_>,
) -> Snapshot {
    let uploaded = compositor
        .upload_model(&grey_sphere())
        .expect("the model uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let geometry = boxed(80.0, 50.0, 80.0);
    let camera = Some(SceneCamera::framing(canvas()));
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera,
                lighting: Lighting::Scene,
                lights: Some(lights),
                shadows: &[],
                environment: Some(environment),
                floor: None,
                materials: &[],
                textures: &[],
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [Item {
        camera,
        ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
    }];
    render(compositor, &items)
}

/// ADR-0151 decision 9: a scene's environment lights a scene-lit model on its own: a
/// blue sky over a black ground lights the sphere's top blue and leaves its underside
/// dark; a panorama red above and green below lights the top red and the bottom green;
/// an ambient light at 0 scales the sky to nothing.
#[test]
fn golden_model_an_environment_lights_a_model_from_its_sky() {
    let Some(mut opened) = open() else {
        return;
    };
    let black = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    let gradient = super::Environment {
        sky: Color {
            r: 0.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        },
        horizon: black,
        ground: black,
        intensity: 2.0,
        turn_deg: 0.0,
        tilt_deg: 0.0,
        roll_deg: 0.0,
        image: None,
    };
    let shot = sky_sphere(&mut opened.compositor, &[], gradient);
    let (top, bottom) = (rgb(&shot, 80, 16), rgb(&shot, 80, 86));
    assert!(
        top[2] > 40 && top[2] > top[0] + 30,
        "the sky lights the top: {top:?}"
    );
    assert!(
        luma(bottom) < luma(top) / 2,
        "the ground leaves the bottom dark: {bottom:?}"
    );
    let dark = sky_sphere(&mut opened.compositor, &[ambient(0.0)], gradient);
    assert!(luma(rgb(&dark, 80, 16)) < 6, "{:?}", rgb(&dark, 80, 16));
    let mut rgba = Vec::new();
    for y in 0..4 {
        for _ in 0..8 {
            rgba.extend_from_slice(if y < 2 {
                &[255, 0, 0, 255]
            } else {
                &[0, 255, 0, 255]
            });
        }
    }
    let picture = opened
        .compositor
        .upload_rgba8(8, 4, &rgba)
        .expect("the panorama uploads");
    let panorama = super::Environment {
        image: Some(&picture),
        ..gradient
    };
    let shot = sky_sphere(&mut opened.compositor, &[], panorama);
    let (top, bottom) = (rgb(&shot, 80, 16), rgb(&shot, 80, 86));
    assert!(top[0] > top[1] + 30, "red above: {top:?}");
    assert!(bottom[1] > bottom[0] + 30, "green below: {bottom:?}");
}

/// A panorama imported as premultiplied sRGB code values, as a GPU-painted page is, is
/// decoded as an upload of the same colour is: a mid-grey code lights the model as dimly
/// as the uploaded grey, not as if its code value were linear light.
#[test]
fn golden_model_a_premultiplied_srgb_panorama_decodes_as_an_upload() {
    let Some(mut opened) = open() else {
        return;
    };
    let grey = super::Environment {
        sky: Color::WHITE,
        horizon: Color::WHITE,
        ground: Color::WHITE,
        intensity: 1.0,
        turn_deg: 0.0,
        tilt_deg: 0.0,
        roll_deg: 0.0,
        image: None,
    };
    let uploaded = opened
        .compositor
        .upload_rgba8(8, 4, &[128, 128, 128, 255].repeat(32))
        .expect("the panorama uploads");
    let texture = crate::tests::unorm_texture(&opened.compositor, 8, 4, [128, 128, 128, 255]);
    let painted = opened
        .compositor
        .import_premultiplied(texture, None)
        .expect("a Unorm texture imports premultiplied");
    let lit_by = |compositor: &mut Compositor, image| {
        let shot = sky_sphere(
            compositor,
            &[],
            super::Environment {
                image: Some(image),
                ..grey
            },
        );
        rgb(&shot, 80, 16)
    };
    let from_upload = lit_by(&mut opened.compositor, &uploaded);
    let from_page = lit_by(&mut opened.compositor, &painted);
    assert!(
        luma(from_upload) > 10,
        "the grey lights it: {from_upload:?}"
    );
    for c in 0..3 {
        assert!(
            from_upload[c].abs_diff(from_page[c]) <= 3,
            "{from_upload:?} vs {from_page:?}"
        );
    }
}

/// ADR-0170 decision 3: a rough surface blurs a panorama's bright points into soft
/// light, not one sharp copy per tap of its blur. A half-rough metal sphere under a
/// black sky scattered with single bright texels shows the stars as a smooth glow: no
/// two neighbouring pixels across its middle differ much, though the stars light it.
#[test]
fn golden_model_a_rough_surface_blurs_a_starry_sky_smoothly() {
    let Some(mut opened) = open() else {
        return;
    };
    let (width, height) = (256u32, 128u32);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in (5..height).step_by(16) {
        for x in (3..width).step_by(16) {
            let at = ((y * width + x) * 4) as usize;
            rgba[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    for at in (3..rgba.len()).step_by(4) {
        rgba[at] = 255;
    }
    let stars = opened
        .compositor
        .upload_rgba8(width, height, &rgba)
        .expect("the panorama uploads");
    let sphere = Shape::Sphere
        .model(Material {
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 1.0,
            roughness: 0.5,
            ..Material::default()
        })
        .expect("the sphere builds");
    let uploaded = opened
        .compositor
        .upload_model(&sphere)
        .expect("the model uploads");
    let layer = opened.compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let geometry = boxed(80.0, 50.0, 80.0);
    let camera = Some(SceneCamera::framing(canvas()));
    let environment = super::Environment {
        sky: Color::WHITE,
        horizon: Color::WHITE,
        ground: Color::WHITE,
        intensity: 16.0,
        turn_deg: 0.0,
        tilt_deg: 0.0,
        roll_deg: 0.0,
        image: Some(&stars),
    };
    opened
        .compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera,
                lighting: Lighting::Scene,
                lights: Some(&[]),
                shadows: &[],
                environment: Some(environment),
                floor: None,
                materials: &[],
                textures: &[],
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [Item {
        camera,
        ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
    }];
    let shot = render(&mut opened.compositor, &items);
    let row: Vec<u32> = (56..104).map(|x| luma(rgb(&shot, x, 50))).collect();
    let jump = row
        .windows(2)
        .map(|pair| pair[0].abs_diff(pair[1]))
        .max()
        .unwrap_or(0);
    let lit = row.iter().sum::<u32>() / row.len() as u32;
    assert!(lit > 6, "the stars light the sphere: {row:?}");
    assert!(
        jump < 24,
        "the stars blur smoothly: a jump of {jump} in {row:?}"
    );
}

/// ADR-0151 decision 4: a model cut at a floor draws nothing at or below it, in its
/// picture or its view depth, so a scene drawn mirrored in its floor shows only what
/// stands above it. The cube spans y 30 to 70; a floor at 50 keeps its upper half.
#[test]
fn golden_model_a_floor_cuts_what_is_below_it() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let uploaded = compositor
        .upload_model(&coloured_cube())
        .expect("the model uploads");
    let camera = SceneCamera::framing(canvas());
    let draw = ModelDraw {
        model: &uploaded,
        geometry: boxed(80.0, 50.0, 40.0),
        camera: Some(camera),
        lighting: Lighting::Unlit,
        lights: None,
        shadows: &[],
        environment: None,
        floor: Some(50.0),
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(&draw, &layer)
        .expect("the model draws");
    let items = [Item {
        camera: Some(camera),
        ..crate::tests::item(&layer, Placement::Layer(draw.geometry), Crop::NONE, 1.0)
    }];
    let shot = render(compositor, &items);
    assert_near(rgb(&shot, 80, 40), FRONT, (80, 40));
    assert_near(rgb(&shot, 80, 60), [0, 0, 0], (80, 60));
    let depth = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model_depth(&draw, &depth)
        .expect("the depth draws");
    let read = |compositor: &mut Compositor| {
        let items = [crate::tests::item(
            &depth,
            Placement::IDENTITY,
            Crop::NONE,
            1.0,
        )];
        render(compositor, &items)
    };
    let seen = read(compositor);
    assert!(rgb(&seen, 80, 40)[0] > 0, "the upper half has depth");
    assert_eq!(rgb(&seen, 80, 60)[0], 0, "the lower half has none");
}

/// ADR-0151 decision 7: a rigged model is drawn where its pose places it. At rest its
/// one slot is the identity and the cube shows its front; posed half a turn about the
/// vertical, its back faces the viewer, lit the right way out, in the same box (a pose
/// never rescales it, since the box fits the rest pose).
#[test]
fn golden_model_a_pose_turns_a_rigged_model_on_the_gpu() {
    use jstream_media::model::rig::{Rest, Rig, RigNode, Slot};
    use std::sync::Arc;
    let Some(mut opened) = open() else {
        return;
    };
    let cube = coloured_cube();
    let mut primitive = cube.primitives()[0].clone();
    for v in &mut primitive.vertices {
        v.weights = [1.0, 0.0, 0.0, 0.0];
    }
    let identity = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let rig = Rig::new(
        vec![RigNode {
            parent: None,
            rest: Rest::Matrix(identity),
        }],
        vec![Slot {
            node: 0,
            inverse_bind: identity,
        }],
        vec![],
    )
    .expect("a rig");
    let rigged = Model::rigged(
        vec![primitive],
        cube.materials().to_vec(),
        vec![],
        Some(Arc::new(rig)),
    )
    .expect("the cube builds");
    let compositor = &mut opened.compositor;
    let uploaded = compositor.upload_model(&rigged).expect("the model uploads");
    let geometry = boxed(80.0, 50.0, 40.0);
    let shoot = |compositor: &mut Compositor| {
        let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
        compositor
            .draw_model(
                &ModelDraw {
                    model: &uploaded,
                    geometry,
                    camera: None,
                    lighting: Lighting::Studio,
                    lights: None,
                    shadows: &[],
                    environment: None,
                    floor: None,
                    materials: &[],
                    textures: &[],
                    reflection: None,
                    probe: None,
                },
                &layer,
            )
            .expect("the model draws");
        let items = [crate::tests::item(
            &layer,
            Placement::Layer(geometry),
            Crop::NONE,
            1.0,
        )];
        render(compositor, &items)
    };
    let rest = shoot(compositor);
    assert_near(rgb(&rest, 80, 50), FRONT, (80, 50));
    let half_turn: [f32; 16] = [
        -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    compositor.pose_model(&uploaded, &[half_turn]);
    let turned = shoot(compositor);
    for at in [(62, 32), (80, 50), (97, 67)] {
        assert_near(rgb(&turned, at.0, at.1), BACK, at);
    }
    let back = count(&turned, BACK);
    assert!(
        (1520..=1680).contains(&back),
        "{back} back pixels, want about 1600"
    );
    // A palette of another length is ignored.
    compositor.pose_model(&uploaded, &[identity_f32(), identity_f32()]);
    assert_near(rgb(&shoot(compositor), 80, 50), BACK, (80, 50));
    // One layer drawn again of the same pose is kept; of a new pose, drawn (ADR-0246).
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let draw = ModelDraw {
        model: &uploaded,
        geometry,
        camera: None,
        lighting: Lighting::Studio,
        lights: None,
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let draw_again = |compositor: &mut Compositor| {
        let was = layer.generation();
        compositor
            .draw_model(&draw, &layer)
            .expect("the model draws");
        layer.generation() != was
    };
    assert!(draw_again(compositor), "first drawn");
    compositor.pose_model(&uploaded, &[half_turn]);
    assert!(!draw_again(compositor), "the same pose written again");
    compositor.pose_model(&uploaded, &[identity_f32()]);
    assert!(draw_again(compositor), "a new pose");
}

fn identity_f32() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

/// Draws `uploaded` unlit, flat at the middle of the canvas, with `materials`' drives,
/// and reads back the middle pixel.
fn driven_middle(
    compositor: &mut Compositor,
    uploaded: &super::GpuModel,
    materials: &[super::MaterialDrive<'_>],
) -> [u8; 3] {
    let geometry = boxed(80.0, 50.0, 80.0);
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(
            &ModelDraw {
                model: uploaded,
                geometry,
                camera: None,
                lighting: Lighting::Unlit,
                lights: None,
                shadows: &[],
                environment: None,
                floor: None,
                materials,
                textures: &[],
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [crate::tests::item(
        &layer,
        Placement::Layer(geometry),
        Crop::NONE,
        1.0,
    )];
    rgb(&render(compositor, &items), 80, 50)
}

/// ADR-0171 decision 4: a draw changes the materials it names, by name or all of them,
/// lightening the base colour and scaling the glow, and the next draw that doesn't
/// drive them shows them as the file has them.
#[test]
fn golden_model_a_draw_drives_its_materials_by_name_and_only_for_itself() {
    let Some(mut opened) = open() else {
        return;
    };
    let plane = Shape::Plane
        .model(Material::default())
        .expect("the plane builds");
    let panel = Material {
        name: "panel".into(),
        base_color: [0.05, 0.05, 0.05, 1.0],
        emissive: [0.2, 0.0, 0.0],
        unlit: true,
        ..plane.materials()[0].clone()
    };
    let model =
        Model::new(plane.primitives().to_vec(), vec![panel], vec![]).expect("the plane builds");
    let compositor = &mut opened.compositor;
    let uploaded = compositor.upload_model(&model).expect("the model uploads");
    let rest = driven_middle(compositor, &uploaded, &[]);
    let lighter = super::MaterialDrive {
        material: Some("panel"),
        color: 0.5,
        ..super::MaterialDrive::default()
    };
    let lit = driven_middle(compositor, &uploaded, &[lighter]);
    assert!(
        lit.iter().zip(rest).all(|(l, r)| *l > r + 60),
        "lightened {lit:?} from {rest:?}"
    );
    // Another material's name changes nothing.
    let elsewhere = super::MaterialDrive {
        material: Some("trim"),
        ..lighter
    };
    assert_near(
        driven_middle(compositor, &uploaded, &[elsewhere]),
        rest,
        (80, 50),
    );
    // Every material, with no name: the glow at three times its strength.
    let glow = super::MaterialDrive {
        emissive_strength: 2.0,
        ..super::MaterialDrive::default()
    };
    let glowing = driven_middle(compositor, &uploaded, &[glow]);
    assert!(glowing[0] > rest[0] + 40, "glow {glowing:?} from {rest:?}");
    assert_near([glowing[1], glowing[2], 0], [rest[1], rest[2], 0], (80, 50));
    // Nothing stored: an undriven draw is as the file has it.
    assert_near(driven_middle(compositor, &uploaded, &[]), rest, (80, 50));
}

#[test]
fn a_material_drive_adds_within_ranges_and_nothing_when_it_adds_nothing() {
    let values = [
        0.5, 0.5, 0.5, 1.0, 2.0, 1.0, 0.0, 0.0, 0.3, 0.6, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ];
    assert_eq!(super::driven_values(&values, "a", &[]), None);
    let drive = |d: super::MaterialDrive<'static>| {
        super::driven_values(&values, "a", &[d]).expect("it drives")
    };
    let none = super::MaterialDrive::default();
    // Metalness and roughness add, within 0 and 1.
    let rough = drive(super::MaterialDrive {
        metallic: 0.5,
        roughness: -2.0,
        ..none
    });
    assert!(
        (rough[8] - 0.8).abs() < 1e-6 && rough[9] == 0.0,
        "{rough:?}"
    );
    // The glow keeps its brightness above 1 and is scaled, never past 64.
    let strong = drive(super::MaterialDrive {
        emissive_strength: 100.0,
        ..none
    });
    assert!((strong[4] - 128.0).abs() < 1e-3, "{strong:?}");
    let off = drive(super::MaterialDrive {
        emissive_strength: -1.0,
        ..none
    });
    assert_eq!(&off[4..7], &[0.0, 0.0, 0.0]);
    // Black is as dark as a colour goes, and alpha stays.
    let dark = drive(super::MaterialDrive {
        color: -1.0,
        ..none
    });
    assert_eq!(&dark[0..4], &[0.0, 0.0, 0.0, 1.0]);
    // A drive for another material adds nothing.
    let other = super::MaterialDrive {
        material: Some("b"),
        color: 1.0,
        ..none
    };
    assert_eq!(super::driven_values(&values, "a", &[other]), None);
    // A set colour replaces the base colour in linear, before a drive adds to it, and a
    // set glow keeps the glow's brightness (ADR-0171 decision 5).
    let red = drive(super::MaterialDrive {
        set_color: Some([1.0, 0.0, 0.0]),
        ..none
    });
    assert_eq!(&red[0..4], &[1.0, 0.0, 0.0, 1.0]);
    let pink = drive(super::MaterialDrive {
        set_color: Some([1.0, 0.0, 0.0]),
        color: 0.5,
        ..none
    });
    assert!(pink[1] > 0.0 && pink[0] == 1.0, "{pink:?}");
    let green = drive(super::MaterialDrive {
        set_emissive: Some([0.0, 1.0, 0.0]),
        ..none
    });
    assert!(
        (green[5] - 2.0).abs() < 1e-3 && green[4] == 0.0 && green[6] == 0.0,
        "{green:?}"
    );
}

/// A framing camera (ADR-0094 decision 3) with depth of field focused `focus_px` along
/// its axis and `blur_px` far blur (ADR-0108).
fn focused_camera(focus_px: f32, blur_px: f32) -> SceneCamera {
    SceneCamera {
        focus_px: Finite32::new(focus_px),
        blur_px: finite(blur_px),
        ..SceneCamera::framing(canvas())
    }
}

/// The framing camera's distance to the canvas plane: its focal length in pixels.
fn framing_distance() -> f32 {
    SceneCamera::framing(canvas())
        .view(canvas())
        .expect("a view")
        .focal_px()
}

/// The red channel of row `y`.
fn red_row(shot: &Snapshot, y: u32) -> Vec<u8> {
    (0..WIDTH).map(|x| rgb(shot, x, y)[0]).collect()
}

/// One unlit model of two boxes, each `[centre, half size]` in model units with a colour:
/// cut from the built-in cube and moved.
fn two_boxes(boxes: [([f32; 3], [f32; 3], [u8; 3]); 2]) -> Model {
    let cube = Shape::Cube
        .model(Material {
            unlit: true,
            ..Material::default()
        })
        .expect("the cube builds");
    let primitives = boxes
        .iter()
        .map(|&(centre, half, colour)| {
            let mut primitive = cube.primitives()[0].clone();
            let linear = colour.map(|c| if c == 0 { 0.0 } else { 1.0 });
            for v in &mut primitive.vertices {
                // The built-in cube spans -0.5..0.5 on each axis.
                v.position = [0, 1, 2].map(|i| centre[i] + v.position[i] * 2.0 * half[i]);
                v.color = [linear[0], linear[1], linear[2], 1.0];
            }
            primitive
        })
        .collect();
    Model::new(primitives, cube.materials().to_vec(), vec![]).expect("the model builds")
}

/// The opaque primitives draw their depth first and are shaded where it is equal, so
/// the nearest shows whatever order they are in, and of two in one plane the first.
#[test]
fn golden_model_opaque_primitives_show_the_nearest_in_any_order_and_the_first_in_a_plane() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let geometry = boxed(80.0, 50.0, 60.0);
    let camera = Some(SceneCamera::framing(canvas()));
    let near = ([0.0, 0.0, 0.3], [0.5, 0.5, 0.01], [0, 0, 255]);
    let far = ([0.0, 0.0, -0.3], [1.0, 1.0, 0.01], [255, 0, 0]);
    let a = draw(
        compositor,
        &two_boxes([near, far]),
        geometry,
        camera,
        Lighting::Unlit,
    );
    let b = draw(
        compositor,
        &two_boxes([far, near]),
        geometry,
        camera,
        Lighting::Unlit,
    );
    assert_eq!(a.rgba(), b.rgba(), "the order of the primitives shows");
    assert_near(rgb(&a, 80, 50), [0, 0, 255], (80, 50));
    assert!(
        count(&a, [255, 0, 0]) > 500,
        "the far box shows around the near one"
    );
    // In one plane: the first primitive's colour, whole, not a mix of the two.
    let red = ([0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [255, 0, 0]);
    let blue = ([0.0, 0.0, 0.0], [0.5, 0.5, 0.01], [0, 0, 255]);
    let first_red = draw(
        compositor,
        &two_boxes([red, blue]),
        geometry,
        camera,
        Lighting::Unlit,
    );
    assert_near(rgb(&first_red, 80, 50), [255, 0, 0], (80, 50));
    let first_blue = draw(
        compositor,
        &two_boxes([blue, red]),
        geometry,
        camera,
        Lighting::Unlit,
    );
    assert_near(rgb(&first_blue, 80, 50), [0, 0, 255], (80, 50));
}

/// ADR-0165 decision 2: a model whose surfaces sit at the focus draws as it does with no
/// depth of field.
#[test]
fn golden_model_in_focus_draws_as_without_depth_of_field() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    // A thin slab facing the camera on the canvas plane, where the framing camera's
    // distance is its focal length.
    let model = two_boxes([
        ([0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [255, 0, 0]),
        ([0.0, 0.0, 0.0], [0.5, 0.5, 0.01], [0, 0, 255]),
    ]);
    let geometry = boxed(80.0, 50.0, 60.0);
    let plain = draw(
        compositor,
        &model,
        geometry,
        Some(SceneCamera::framing(canvas())),
        Lighting::Unlit,
    );
    let focused = draw(
        compositor,
        &model,
        geometry,
        Some(focused_camera(framing_distance(), 16.0)),
        Lighting::Unlit,
    );
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            assert_near(rgb(&focused, x, y), rgb(&plain, x, y), (x, y));
        }
    }
}

/// ADR-0165 decisions 1 and 2: a model far from the focus is soft. Its light is kept,
/// its silhouette spreads past where it was, and its edge ramps over several pixels.
#[test]
fn golden_model_out_of_focus_is_soft_and_keeps_its_light() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let model = two_boxes([
        ([0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [255, 0, 0]),
        ([0.0, 0.0, 0.0], [0.1, 0.1, 0.01], [255, 0, 0]),
    ]);
    let geometry = boxed(80.0, 50.0, 40.0);
    let plain = draw(
        compositor,
        &model,
        geometry,
        Some(SceneCamera::framing(canvas())),
        Lighting::Unlit,
    );
    // Focused four times as far: the slab blurs by 16 * 3/4 = 12 px.
    let soft = draw(
        compositor,
        &model,
        geometry,
        Some(focused_camera(framing_distance() * 4.0, 16.0)),
        Lighting::Unlit,
    );
    let light = |shot: &Snapshot| -> f64 {
        (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| {
                let c = f64::from(rgb(shot, x, y)[0]) / 255.0;
                // sRGB to linear, roughly: the light a pixel holds.
                c.powf(2.2)
            })
            .sum()
    };
    let (before, after) = (light(&plain), light(&soft));
    assert!(
        (after - before).abs() < before * 0.12,
        "light before {before}, after {after}"
    );
    let row = red_row(&plain, 50);
    let left = row
        .iter()
        .position(|&r| r > 128)
        .expect("the slab is drawn");
    let soft_row = red_row(&soft, 50);
    // The silhouette spreads outward: four pixels outside the sharp edge, some red.
    assert!(
        soft_row[left - 4] > 20,
        "outside the edge: {:?}",
        &soft_row[..left + 8]
    );
    // And the edge ramps: at least eight pixels between dark and full red.
    let ramp = soft_row[..80]
        .iter()
        .filter(|&&r| r > 10 && r < 245)
        .count();
    assert!(ramp >= 8, "row: {soft_row:?}");
    // The sharp draw's edge is one pixel.
    let sharp_ramp = row[..80].iter().filter(|&&r| r > 10 && r < 245).count();
    assert!(sharp_ramp <= 2, "row: {row:?}");
}

/// ADR-0165 decision 2, occlusion: within one model, a sharp near part keeps its edge
/// over a soft far part, and a soft near part spreads over a sharp far part.
#[test]
fn golden_model_a_near_part_blurs_over_a_far_one_but_never_the_reverse() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    // A wide blue slab at the back, a small red block at the front, both one model. The
    // box fits the model's 2 units across into 80 px, so a unit is 40 px.
    let model = two_boxes([
        ([0.0, 0.0, -0.95], [1.0, 1.0, 0.05], [0, 0, 255]),
        ([0.0, 0.0, 0.95], [0.25, 0.25, 0.05], [255, 0, 0]),
    ]);
    let geometry = boxed(80.0, 50.0, 80.0);
    let f = framing_distance();
    let (near, far) = (f - 40.0, f + 36.0);
    let plain = draw(
        compositor,
        &model,
        geometry,
        Some(SceneCamera::framing(canvas())),
        Lighting::Unlit,
    );
    let row = red_row(&plain, 50);
    let edge = row
        .iter()
        .position(|&r| r > 128)
        .expect("the block is drawn");
    // Focused on the block: the slab is soft, the block's edge stays sharp, and no blue
    // runs into the block nor red out of it.
    let on_near = draw(
        compositor,
        &model,
        geometry,
        Some(focused_camera(near, 24.0)),
        Lighting::Unlit,
    );
    let inside = rgb(&on_near, edge as u32 + 1, 50);
    assert_near(inside, [255, 0, 0], (edge as u32 + 1, 50));
    let outside = rgb(&on_near, edge as u32 - 3, 50);
    assert!(
        outside[0] < 16,
        "red ran out of the sharp block: {outside:?}"
    );
    // Focused on the slab: the block spreads over it.
    let on_far = draw(
        compositor,
        &model,
        geometry,
        Some(focused_camera(far, 24.0)),
        Lighting::Unlit,
    );
    let outside = rgb(&on_far, edge as u32 - 3, 50);
    assert!(outside[0] > 40, "the soft block didn't spread: {outside:?}");
    // Past the block's blur, the in-focus slab is untouched. The blur's rim on this row
    // is at x = 54, and a wide blur's rim is soft by a few pixels (ADR-0193).
    assert_near(rgb(&on_far, 40, 20), rgb(&plain, 40, 20), (40, 20));
}
/// ADR-0165: one non-finite pixel in a model's layer (a NaN from a degenerate normal, or a
/// channel past the half-float range) stays one pixel. The gather spreads each pixel over
/// up to 48 others, and the smoothing and the resolve over more, so unchecked it would mark a whole disc of
/// a defocused model with black dots.
#[test]
fn golden_model_depth_of_field_keeps_a_non_finite_pixel_to_itself() {
    let Some(mut opened) = open() else {
        return;
    };
    let camera = focused_camera(framing_distance() * 4.0, 16.0);
    let view = camera.view(canvas()).expect("a view");
    let (cx, cy) = (WIDTH / 2, HEIGHT / 2);
    let colour = vec![[0.5, 0.5, 0.5, 1.0]; (WIDTH * HEIGHT) as usize];
    // Every pixel at the framing distance, 1 / w = 1: a circle of confusion of 48 px.
    let depth = vec![[1.0, 0.0, 0.0, 1.0]; (WIDTH * HEIGHT) as usize];
    let maker = opened.compositor.maker();
    let colour =
        crate::image::upload_unit_f16(&maker, WIDTH, HEIGHT, &colour).expect("the colour uploads");
    let depth =
        crate::image::upload_unit_f16(&maker, WIDTH, HEIGHT, &depth).expect("the depth uploads");
    // The centre texel becomes a half-float NaN in every channel.
    maker.gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: colour.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d { x: cx, y: cy, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &[0x00, 0x7e].repeat(4),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let mut focus = crate::filter::VideoFilter::of(
        super::focus::KIND,
        Box::new(super::focus::ModelFocus::default()),
    );
    focus.set_depth(Some(crate::filter::ViewDepth {
        view: Some(view),
        ..crate::filter::ViewDepth::new(std::sync::Arc::new(depth), view.focal_px())
    }));
    let out = opened
        .compositor
        .apply_filter(&mut focus, &colour, crate::filter::FilterClock::default())
        .expect("the passes draw");
    let shot = crate::filter::testing::read(&mut opened, &out);
    let reference = rgb(&shot, 4, 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if (x, y) == (cx, cy) {
                continue;
            }
            let got = rgb(&shot, x, y);
            for c in 0..3 {
                assert!(
                    got[c].abs_diff(reference[c]) <= 24,
                    "pixel ({x}, {y}) is {got:?}, the even grey around it {reference:?}"
                );
            }
        }
    }
}

/// ADR-0193: the gather of what spreads wide is drawn at half the layer's size, and a
/// blur too slight for that size is still drawn at the layer's own. Stripes one pixel
/// wide, 0.6 px out of focus, are softened and still there.
#[test]
fn golden_model_depth_of_field_gathers_at_half_size_and_keeps_a_slight_blur_fine() {
    let Some(mut opened) = open() else {
        return;
    };
    // Every pixel at the framing distance, 1 / w = 1, and the focus four times as far:
    // a circle of confusion of three times the camera's blur.
    let camera = focused_camera(framing_distance() * 4.0, 0.2);
    let view = camera.view(canvas()).expect("a view");
    let colour: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
        .map(|at| {
            let v = if (at % WIDTH).is_multiple_of(2) {
                0.1
            } else {
                0.9
            };
            [v, v, v, 1.0]
        })
        .collect();
    let depth = vec![[1.0, 0.0, 0.0, 1.0]; (WIDTH * HEIGHT) as usize];
    let maker = opened.compositor.maker();
    let colour =
        crate::image::upload_unit_f16(&maker, WIDTH, HEIGHT, &colour).expect("the colour uploads");
    let depth =
        crate::image::upload_unit_f16(&maker, WIDTH, HEIGHT, &depth).expect("the depth uploads");
    let mut focus = crate::filter::VideoFilter::of(
        super::focus::KIND,
        Box::new(super::focus::ModelFocus::default()),
    );
    focus.set_depth(Some(crate::filter::ViewDepth {
        view: Some(view),
        ..crate::filter::ViewDepth::new(std::sync::Arc::new(depth), view.focal_px())
    }));
    let out = opened
        .compositor
        .apply_filter(&mut focus, &colour, crate::filter::FilterClock::default())
        .expect("the passes draw");
    // The scratch layers in the order the passes take them: tiles, reach, packed, then
    // the gathered layer and its smoothed copy.
    for gathered in &focus.scratch().kept()[3..5] {
        assert_eq!(
            (gathered.width(), gathered.height()),
            (WIDTH / 2, HEIGHT / 2)
        );
    }
    let sharp = crate::filter::testing::read(&mut opened, &colour);
    let shot = crate::filter::testing::read(&mut opened, &out);
    let contrast = |shot: &Snapshot| {
        i32::from(rgb(shot, WIDTH / 2 + 1, HEIGHT / 2)[0])
            - i32::from(rgb(shot, WIDTH / 2, HEIGHT / 2)[0])
    };
    let (before, after) = (contrast(&sharp), contrast(&shot));
    assert!(
        after > 12 && after < before - 40,
        "stripes {before} apart before, {after} after"
    );
}

/// Draws an unlit white plane with `texture` (ADR-0107 decision 5) and reads it back.
fn textured_plane(
    compositor: &mut Compositor,
    texture: Option<super::LiveTexture<'_>>,
) -> Snapshot {
    textured_plane_driven(compositor, texture, &[])
}

/// [`textured_plane`], its materials driven by `materials` (ADR-0171 decision 4).
fn textured_plane_driven(
    compositor: &mut Compositor,
    texture: Option<super::LiveTexture<'_>>,
    materials: &[super::MaterialDrive<'_>],
) -> Snapshot {
    let plane = Shape::Plane
        .model(Material {
            unlit: true,
            ..Material::default()
        })
        .expect("the plane builds");
    let uploaded = compositor.upload_model(&plane).expect("the model uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let geometry = boxed(80.0, 50.0, 80.0);
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera: None,
                lighting: Lighting::Unlit,
                lights: None,
                shadows: &[],
                environment: None,
                floor: None,
                textures: texture.as_slice(),
                materials,
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [crate::tests::item(
        &layer,
        Placement::Layer(geometry),
        Crop::NONE,
        1.0,
    )];
    render(compositor, &items)
}

/// ADR-0107 decision 5: a live texture replaces the base colour of the material it
/// names, or of every material, and follows its picture as that changes; one for
/// another material leaves the model's own, and a picture that isn't there reads
/// transparent.
#[test]
fn golden_model_a_live_texture_replaces_the_base_colour() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    // 8x8 texels: red over its top half, blue under it.
    let rgba: Vec<u8> = (0..8)
        .flat_map(|y| {
            (0..8).flat_map(move |_| {
                if y < 4 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
        })
        .collect();
    let picture = compositor
        .upload_rgba8(8, 8, &rgba)
        .expect("the picture uploads");
    let live = |on| super::LiveTexture {
        image: Some(&picture),
        on,
    };
    let shot = textured_plane(compositor, Some(live(None)));
    assert_near(rgb(&shot, 80, 30), [255, 0, 0], (80, 30));
    assert_near(rgb(&shot, 80, 70), [0, 0, 255], (80, 70));
    let named = textured_plane(compositor, Some(live(Some("shape"))));
    assert_near(rgb(&named, 80, 30), [255, 0, 0], (80, 30));
    let other = textured_plane(compositor, Some(live(Some("another"))));
    assert_near(rgb(&other, 80, 30), [255, 255, 255], (80, 30));
    let missing = textured_plane(
        compositor,
        Some(super::LiveTexture {
            image: None,
            on: None,
        }),
    );
    assert_near(rgb(&missing, 80, 50), [0, 0, 0], (80, 50));
    // A drive reaches a live-textured material too (ADR-0171 decision 4), and the next
    // draw that doesn't drive it shows the picture as it is.
    let dark = super::MaterialDrive {
        color: -1.0,
        ..super::MaterialDrive::default()
    };
    let driven = textured_plane_driven(compositor, Some(live(None)), &[dark]);
    assert_near(rgb(&driven, 80, 30), [0, 0, 0], (80, 30));
    let again = textured_plane(compositor, Some(live(None)));
    assert_near(rgb(&again, 80, 30), [255, 0, 0], (80, 30));
    // A layer drawn again: its mip chain is rebuilt, and the model shows the new picture.
    let layer = compositor.layer(16, 16).expect("a layer");
    for colour in [Color::rgb(0.0, 1.0, 0.0), Color::rgb(1.0, 0.0, 1.0)] {
        compositor
            .render_into(
                &Scene {
                    canvas: FrameSize::new(16, 16).expect("non-zero"),
                    background: colour,
                    items: &[],
                },
                &layer,
            )
            .expect("the layer draws");
        let shot = textured_plane(
            compositor,
            Some(super::LiveTexture {
                image: Some(&layer),
                on: None,
            }),
        );
        let want = if colour.g > 0.5 {
            [0, 255, 0]
        } else {
            [255, 0, 255]
        };
        assert_near(rgb(&shot, 80, 50), want, (80, 50));
    }
}

/// ADR-0250: a model takes several live textures, each on the parts it names by their
/// node's or mesh's name though they share one material, and one that names nothing on
/// the rest; each records the resolution its own parts need.
#[test]
fn a_model_shows_a_live_texture_on_each_part_it_names() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let plane = Shape::Plane
        .model(Material {
            unlit: true,
            ..Material::default()
        })
        .expect("the plane builds");
    // The plane's left and right halves, one material, named apart.
    let half = |shift: f32, name: &str| {
        let mut part = plane.primitives()[0].clone();
        for vertex in &mut part.vertices {
            vertex.position[0] = vertex.position[0] * 0.5 + shift;
        }
        part.names = vec![name.to_owned()];
        part
    };
    let model = Model::new(
        vec![half(-0.25, "left"), half(0.25, "right")],
        plane.materials().to_vec(),
        vec![],
    )
    .expect("the halves build");
    let uploaded = compositor.upload_model(&model).expect("the model uploads");
    let solid = |rgba: [u8; 4]| {
        let texels: Vec<u8> = (0..64).flat_map(|_| rgba).collect();
        compositor
            .upload_rgba8(8, 8, &texels)
            .expect("the picture uploads")
    };
    let (red, blue) = (solid([255, 0, 0, 255]), solid([0, 0, 255, 255]));
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let geometry = boxed(80.0, 50.0, 80.0);
    let textures = [
        super::LiveTexture {
            image: Some(&red),
            on: Some("right"),
        },
        super::LiveTexture {
            image: Some(&blue),
            on: None,
        },
    ];
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera: None,
                lighting: Lighting::Unlit,
                lights: None,
                shadows: &[],
                environment: None,
                floor: None,
                textures: &textures,
                materials: &[],
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [crate::tests::item(
        &layer,
        Placement::Layer(geometry),
        Crop::NONE,
        1.0,
    )];
    let shot = render(compositor, &items);
    assert_near(rgb(&shot, 60, 50), [0, 0, 255], (60, 50));
    assert_near(rgb(&shot, 100, 50), [255, 0, 0], (100, 50));
    // Each half is 40 pixels wide for 8 texels: both need all of theirs.
    for picture in [&red, &blue] {
        let need = picture.take_need();
        assert!(need >= 1.0, "{need}");
    }
}

/// ADR-0249: a live texture's draw records how much of its picture's resolution the
/// model needs where it is drawn largest: a 512² picture over a unit plane drawn 80
/// pixels wide needs 80 of its 512 texels each way.
#[test]
fn a_live_texture_records_the_resolution_its_model_needs() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let rgba = vec![255u8; 512 * 512 * 4];
    let picture = compositor
        .upload_rgba8(512, 512, &rgba)
        .expect("the picture uploads");
    let _ = picture.take_need();
    textured_plane(
        compositor,
        Some(super::LiveTexture {
            image: Some(&picture),
            on: None,
        }),
    );
    let need = picture.take_need();
    assert!((need - 80.0 / 512.0).abs() < 0.01, "{need}");
}

/// ADR-0107 decision 5: a studio-lit mirror reflects its `reflection` picture in place
/// of the studio gradient, and an unlit model ignores it.
#[test]
fn golden_model_a_reflection_replaces_the_studio_environment() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mirror = Shape::Sphere
        .model(Material {
            metallic: 1.0,
            roughness: 0.05,
            ..Material::default()
        })
        .expect("the sphere builds");
    let uploaded = compositor.upload_model(&mirror).expect("the model uploads");
    let green = compositor
        .upload_rgba8(8, 4, &[0, 255, 0, 255].repeat(32))
        .expect("the panorama uploads");
    let geometry = boxed(80.0, 50.0, 80.0);
    let seen = |compositor: &mut Compositor, lighting, reflection| {
        let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
        compositor
            .draw_model(
                &ModelDraw {
                    model: &uploaded,
                    geometry,
                    camera: None,
                    lighting,
                    lights: None,
                    shadows: &[],
                    environment: None,
                    floor: None,
                    textures: &[],
                    materials: &[],
                    reflection,
                    probe: None,
                },
                &layer,
            )
            .expect("the model draws");
        let items = [crate::tests::item(
            &layer,
            Placement::Layer(geometry),
            Crop::NONE,
            1.0,
        )];
        rgb(&render(compositor, &items), 80, 50)
    };
    let studio = seen(compositor, Lighting::Studio, None);
    let reflecting = seen(compositor, Lighting::Studio, Some(&green));
    assert!(
        studio[0].abs_diff(studio[1]) < 30,
        "the studio gradient is grey: {studio:?}"
    );
    assert!(
        reflecting[1] > reflecting[0] + 60 && reflecting[1] > reflecting[2] + 60,
        "the mirror reflects green: {reflecting:?}"
    );
    let unlit = seen(compositor, Lighting::Unlit, Some(&green));
    assert_near(unlit, [255, 255, 255], (80, 50));
}

/// Draws a white mirror sphere as [`sky_sphere`] does, under a uniform grey sky, with
/// `probe` around it.
fn probed_sphere(compositor: &mut Compositor, probe: Option<super::ProbeLookup<'_>>) -> Snapshot {
    let mirror = Shape::Sphere
        .model(Material {
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 1.0,
            roughness: 0.05,
            ..Material::default()
        })
        .expect("the sphere builds");
    let uploaded = compositor.upload_model(&mirror).expect("the model uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let geometry = boxed(80.0, 50.0, 80.0);
    let camera = Some(SceneCamera::framing(canvas()));
    let grey = Color {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera,
                lighting: Lighting::Scene,
                lights: Some(&[]),
                shadows: &[],
                environment: Some(super::Environment {
                    sky: grey,
                    horizon: grey,
                    ground: grey,
                    intensity: 1.0,
                    turn_deg: 0.0,
                    tilt_deg: 0.0,
                    roll_deg: 0.0,
                    image: None,
                }),
                floor: None,
                materials: &[],
                textures: &[],
                reflection: None,
                probe,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [Item {
        camera,
        ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
    }];
    render(compositor, &items)
}

/// A panorama layer of one colour, as a probe's capture.
fn capture(compositor: &mut Compositor, rgba: [u8; 4]) -> crate::Image {
    let colour = crate::tests::solid(compositor, 8, 4, rgba);
    let items = [crate::tests::item(
        &colour,
        Placement::Legacy(crate::LegacyTransform::IDENTITY),
        Crop::NONE,
        1.0,
    )];
    let scene = Scene {
        canvas: FrameSize::new(8, 4).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &items,
    };
    compositor
        .render_image(&scene)
        .expect("the capture renders")
}

/// ADR-0170 decisions 3 and 5: a mirror inside a probe's box reflects the probe's
/// capture in place of the sky; outside every box, and where the capture saw nothing,
/// the sky shows as without one.
#[test]
fn golden_model_a_probe_replaces_the_environment_inside_its_box() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let red = capture(compositor, [255, 0, 0, 255]);
    let empty = capture(compositor, [0, 0, 0, 0]);
    let around = |image| super::ProbeLookup {
        image,
        centre: [80.0, 50.0, 0.0],
        half: [100_000.0; 3],
        fade: 1.0,
    };
    let sky = rgb(&probed_sphere(compositor, None), 80, 50);
    let inside = rgb(&probed_sphere(compositor, Some(around(&red))), 80, 50);
    assert!(
        inside[0] > inside[1] + 60 && inside[0] > inside[2] + 60,
        "the capture's red is reflected: {inside:?} (sky {sky:?})"
    );
    let far = super::ProbeLookup {
        centre: [1_000_000.0, 0.0, 0.0],
        half: [10.0; 3],
        ..around(&red)
    };
    assert_near(
        rgb(&probed_sphere(compositor, Some(far)), 80, 50),
        sky,
        (80, 50),
    );
    assert_near(
        rgb(&probed_sphere(compositor, Some(around(&empty))), 80, 50),
        sky,
        (80, 50),
    );
}

/// ADR-0107 decision 7: a card drawn after a model, but behind it, is hidden where the
/// model is nearer once the model's layer carries its depth; without the depth the card
/// is drawn over it, as before. Where the model isn't, the card shows either way.
#[test]
fn golden_model_a_card_behind_a_model_is_hidden_by_its_depth() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = SceneCamera {
        depth_sort: false,
        ..SceneCamera::framing(canvas())
    };
    let white = compositor
        .upload_rgba8(1, 1, &[255, 255, 255, 255])
        .expect("a pixel uploads");
    let cube = compositor.upload_model(&coloured_cube()).expect("uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let depth = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
    let mut geometry = boxed(80.0, 50.0, 30.0);
    geometry.depth_px = finite(40.0);
    let draw = ModelDraw {
        model: &cube,
        geometry,
        camera: Some(camera),
        lighting: Lighting::Unlit,
        lights: None,
        shadows: &[],
        environment: None,
        floor: None,
        textures: &[],
        reflection: None,
        probe: None,
        materials: &[],
    };
    compositor
        .draw_model(&draw, &layer)
        .expect("the model draws");
    compositor
        .render_view_depth(
            &Scene {
                canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
                background: Color::TRANSPARENT,
                items: &[],
            },
            &depth,
        )
        .expect("the depth clears");
    compositor
        .draw_model_depth(&draw, &depth)
        .expect("the model's depth draws");
    for with_depth in [false, true] {
        // The card is last in stacking order and the camera doesn't sort, so only the
        // model's depth can hide it.
        let items = [
            Item {
                camera: Some(camera),
                depth: with_depth.then_some(&depth),
                ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
            },
            Item {
                camera: Some(camera),
                ..crate::tests::item(
                    &white,
                    Placement::Geometry(boxed(80.0, 50.0, 90.0)),
                    Crop::NONE,
                    1.0,
                )
            },
        ];
        let shot = render(compositor, &items);
        let want = if with_depth { FRONT } else { [255, 255, 255] };
        assert_near(rgb(&shot, 80, 50), want, (80, 50));
        assert_near(rgb(&shot, 120, 50), [255, 255, 255], (120, 50));
    }
}

/// Every compositor on a device shares the pipelines made on it (`pipelines.rs`): a
/// second one, as each output start opens, compiles nothing the first compiled, and
/// draws a model and a sky as the first did, with its own layouts and bind groups.
#[test]
fn a_second_compositor_on_the_device_makes_no_pipeline_again() {
    let Some(mut opened) = open() else {
        return;
    };
    let camera = SceneCamera::framing(canvas());
    let shoot = |compositor: &mut Compositor| {
        let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
        let sky = crate::sky::SkyDraw {
            environment: super::Environment {
                sky: Color::rgb(0.0, 0.0, 1.0),
                horizon: Color::rgb(1.0, 1.0, 1.0),
                ground: Color::rgb(0.0, 1.0, 0.0),
                intensity: 1.0,
                turn_deg: 0.0,
                tilt_deg: 0.0,
                roll_deg: 0.0,
                image: None,
            },
            camera,
            stars: 0.0,
            star_brightness: 1.0,
            opacity: 1.0,
            deep: crate::sky::DeepSky::NONE,
            warp: 0.0,
            travel: 0.0,
            dropping: 0.0,
            pictures: [None; crate::sky::MAX_SKY_PICTURES],
        };
        compositor.draw_sky(&sky, &layer).expect("the sky draws");
        let sky = render(
            compositor,
            &[crate::tests::item(
                &layer,
                Placement::Legacy(crate::LegacyTransform::IDENTITY),
                Crop::NONE,
                1.0,
            )],
        );
        let cube = draw(
            compositor,
            &coloured_cube(),
            boxed(80.0, 50.0, 40.0),
            Some(camera),
            Lighting::Studio,
        );
        (sky, cube)
    };
    let first = shoot(&mut opened.compositor);
    let made = opened.compositor.pipelines_made();
    assert!(made > 0, "the first compositor made its pipelines");
    let mut other = opened.compositor.sibling();
    let second = shoot(&mut other);
    assert_eq!(
        opened.compositor.pipelines_made(),
        made,
        "the second compositor found every pipeline made"
    );
    assert_eq!(first.0.rgba(), second.0.rgba(), "the same sky");
    assert_eq!(first.1.rgba(), second.1.rgba(), "the same cube");
    assert!(count(&first.1, FRONT) > 100, "the cube shows");
}

/// An environment's axes undo its item's orientation in ADR-0022's order (ADR-0182): a
/// direction the item's rotation takes `v` to reads the sky at `v`, and a turn alone
/// spins as the shaders spun it before.
#[test]
fn an_environments_axes_undo_its_items_turn_tilt_and_roll() {
    let environment = |turn: f32, tilt: f32, roll: f32| super::Environment {
        sky: Color::TRANSPARENT,
        horizon: Color::TRANSPARENT,
        ground: Color::TRANSPARENT,
        intensity: 1.0,
        turn_deg: turn,
        tilt_deg: tilt,
        roll_deg: roll,
        image: None,
    };
    // ADR-0022's rotation of `v` by `[rotation, turn, tilt]`: about z, then y, then x.
    let rotate = |[rotation, turn, tilt]: [f32; 3], [x, y, z]: [f64; 3]| {
        let rad = |d: f32| f64::from(d).to_radians();
        let (s, c) = rad(rotation).sin_cos();
        let (x, y) = (x * c - y * s, x * s + y * c);
        let (s, c) = rad(turn).sin_cos();
        let (x, z) = (x * c + z * s, -x * s + z * c);
        let (s, c) = rad(tilt).sin_cos();
        let (y, z) = (y * c - z * s, y * s + z * c);
        [x, y, z]
    };
    let close = |a: [f32; 3], b: [f64; 3]| {
        a.iter()
            .zip(b)
            .all(|(p, q)| (f64::from(*p) - q).abs() < 1e-5)
    };
    let v = [[0.3, -0.5, 0.8], [1.0, 0.0, 0.0], [0.0, 0.6, -0.8]];
    for angles in [
        [0.0, 0.0, 0.0],
        [30.0, 0.0, 0.0],
        [0.0, 25.0, 0.0],
        [0.0, 0.0, -40.0],
        [15.0, -70.0, 120.0],
    ] {
        let [roll, turn, tilt] = angles;
        let axes = environment(turn, tilt, roll).sky_axes(v.map(|w| rotate(angles, w)));
        for (got, want) in axes.iter().zip(v) {
            assert!(close(*got, want), "{angles:?}: {got:?} is not {want:?}");
        }
    }
    // A turn alone, as the shaders turned a direction before: (c x - s z, y, s x + c z).
    let (s, c) = 35.0_f64.to_radians().sin_cos();
    let [x, y, z] = [0.2, -0.4, -0.9];
    let spun = environment(35.0, 0.0, 0.0).sky_axes([[x, y, z]; 3])[0];
    assert!(close(spun, [c * x - s * z, y, s * x + c * z]), "{spun:?}");
}

/// `layer` placed 1:1 over nothing, read back: its alpha as it holds it.
fn read_layer(compositor: &mut Compositor, layer: &crate::Image) -> Snapshot {
    let items = [crate::tests::item(
        layer,
        Placement::Legacy(crate::LegacyTransform::IDENTITY),
        Crop::NONE,
        1.0,
    )];
    let scene = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &items,
    };
    compositor
        .render(&scene, true)
        .expect("the scene renders")
        .pixels()
        .expect("readback was requested")
        .clone()
}

/// ADR-0170 decision 4: a scene-lit model's surface buffer holds where it is, facing
/// the viewer at its centre, and marks its reflection replaceable; a studio-lit one
/// still covers the targets, so nothing behind it shows through, but marks nothing; the
/// nearer of two models wins wherever both are.
#[test]
fn a_models_surface_marks_where_a_reflection_may_be_replaced() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = Some(SceneCamera::framing(canvas()));
    let sphere = compositor
        .upload_model(&grey_sphere())
        .expect("the model uploads");
    let lights = [ambient(1.0)];
    // As the engine draws it: only a scene-lit model has the scene's lights.
    let at = |geometry, lighting| ModelDraw {
        model: &sphere,
        geometry,
        camera,
        lighting,
        lights: (lighting == Lighting::Scene).then_some(&lights[..]),
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let targets = [(); 3].map(|()| compositor.layer(WIDTH, HEIGHT).expect("a layer"));
    let surface = [&targets[0], &targets[1], &targets[2]];
    compositor.clear_surface(surface).expect("it clears");
    compositor
        .draw_model_surface(&at(boxed(80.0, 50.0, 60.0), Lighting::Scene), surface)
        .expect("the surface draws");
    let reflected = read_layer(compositor, &targets[2]);
    assert_eq!(
        reflected.pixel(80, 50).expect("inside")[3],
        255,
        "replaceable"
    );
    assert_eq!(
        reflected.pixel(2, 2).expect("inside")[3],
        0,
        "no model there"
    );
    let normal = read_layer(compositor, &targets[0]);
    let centre = normal.pixel(80, 50).expect("inside");
    assert!(
        centre[3] > 0 && centre[2] > 200,
        "faces the viewer: {centre:?}"
    );

    // A studio-lit sphere in front of it covers it, and marks nothing.
    let mut front = boxed(80.0, 50.0, 30.0);
    front.depth_px = finite(40.0);
    compositor
        .draw_model_surface(&at(front, Lighting::Studio), surface)
        .expect("the surface draws");
    let reflected = read_layer(compositor, &targets[2]);
    assert_eq!(
        reflected.pixel(80, 50).expect("inside")[3],
        0,
        "the nearer wins"
    );
    assert_eq!(
        reflected.pixel(56, 50).expect("inside")[3],
        255,
        "the farther around it"
    );
    // Cleared, nothing is marked.
    compositor.clear_surface(surface).expect("it clears");
    let reflected = read_layer(compositor, &targets[2]);
    assert_eq!(reflected.pixel(80, 50).expect("inside")[3], 0, "cleared");
}

/// A model's surface drawn with its layer and merged into its scene's buffer is the
/// surface a second draw of it gives, and the nearer of two merged models wins
/// (ADR-0170 decision 4).
#[test]
fn a_surface_drawn_with_the_layer_merges_as_a_second_draw_would() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = Some(SceneCamera::framing(canvas()));
    let sphere = compositor
        .upload_model(&grey_sphere())
        .expect("the model uploads");
    let lights = [ambient(1.0)];
    let at = |geometry, lighting| ModelDraw {
        model: &sphere,
        geometry,
        camera,
        lighting,
        lights: (lighting == Lighting::Scene).then_some(&lights[..]),
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let make = |compositor: &mut Compositor| {
        [(); 3].map(|()| compositor.layer(WIDTH, HEIGHT).expect("a layer"))
    };
    let back = at(boxed(80.0, 50.0, 60.0), Lighting::Scene);
    let mut near = boxed(80.0, 50.0, 30.0);
    near.depth_px = finite(40.0);
    let front = at(near, Lighting::Studio);

    // Drawn a second time, each into the buffer.
    let redrawn = make(compositor);
    let redrawn = [&redrawn[0], &redrawn[1], &redrawn[2]];
    compositor.clear_surface(redrawn).expect("it clears");
    for draw in [&back, &front] {
        compositor
            .draw_model_surface(draw, redrawn)
            .expect("the surface draws");
    }

    // Drawn with their layers, then merged.
    let merged = make(compositor);
    let merged = [&merged[0], &merged[1], &merged[2]];
    let beside = [make(compositor), make(compositor)];
    let colour = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    for (draw, layers) in [&back, &front].into_iter().zip(&beside) {
        compositor
            .draw_model_with_surface(draw, &colour, [&layers[0], &layers[1], &layers[2]])
            .expect("the model draws");
    }
    compositor.clear_surface(merged).expect("it clears");
    for layers in &beside {
        compositor
            .merge_model_surface([&layers[0], &layers[1], &layers[2]], merged)
            .expect("it merges");
    }

    for n in 0..3 {
        let want = read_layer(compositor, redrawn[n]);
        let got = read_layer(compositor, merged[n]);
        for (x, y) in [(80, 50), (56, 50), (104, 50), (80, 30), (2, 2)] {
            let (a, b) = (
                want.pixel(x, y).expect("inside"),
                got.pixel(x, y).expect("inside"),
            );
            assert!(
                a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 2),
                "layer {n} at ({x}, {y}): {a:?} redrawn, {b:?} merged"
            );
        }
    }
    let reflected = read_layer(compositor, merged[2]);
    assert_eq!(
        reflected.pixel(80, 50).expect("inside")[3],
        0,
        "the nearer wins"
    );
    assert_eq!(
        reflected.pixel(56, 50).expect("inside")[3],
        255,
        "the farther around it"
    );
}

/// A model's surface drawn on its own reads its live texture as its layer does: a white
/// metal sphere showing a red picture weighs its reflection by a red BRDF in both
/// (ADR-0170 decision 4, ADR-0107 decision 5).
#[test]
fn a_surface_reads_the_live_texture_its_layer_shows() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = Some(SceneCamera::framing(canvas()));
    let metal = Shape::Sphere
        .model(Material {
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 1.0,
            roughness: 0.2,
            ..Material::default()
        })
        .expect("the sphere builds");
    let sphere = compositor.upload_model(&metal).expect("the model uploads");
    let red = compositor
        .upload_rgba8(8, 8, &[255, 0, 0, 255].repeat(64))
        .expect("the picture uploads");
    let lights = [ambient(1.0)];
    let draw = ModelDraw {
        model: &sphere,
        geometry: boxed(80.0, 50.0, 60.0),
        camera,
        lighting: Lighting::Scene,
        lights: Some(&lights[..]),
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[super::LiveTexture {
            image: Some(&red),
            on: None,
        }],
        reflection: None,
        probe: None,
    };
    let make = |compositor: &mut Compositor| {
        [(); 3].map(|()| compositor.layer(WIDTH, HEIGHT).expect("a layer"))
    };
    let alone = make(compositor);
    let alone = [&alone[0], &alone[1], &alone[2]];
    compositor.clear_surface(alone).expect("it clears");
    compositor
        .draw_model_surface(&draw, alone)
        .expect("the surface draws");
    let beside = make(compositor);
    let colour = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model_with_surface(&draw, &colour, [&beside[0], &beside[1], &beside[2]])
        .expect("the model draws");
    let alone = read_layer(compositor, alone[1])
        .pixel(80, 50)
        .expect("inside");
    let with = read_layer(compositor, &beside[1])
        .pixel(80, 50)
        .expect("inside");
    assert!(
        alone[0] > alone[1].saturating_add(40),
        "the BRDF of a red metal is red: {alone:?}"
    );
    assert!(
        alone.iter().zip(with).all(|(a, b)| a.abs_diff(b) <= 2),
        "drawn alone {alone:?}, with the layer {with:?}"
    );
}

/// [`draw`], into a layer `percent` of the canvas's width and of its height that stands
/// for the whole canvas (ADR-0197), placed over it.
fn draw_at(
    compositor: &mut Compositor,
    model: &Model,
    geometry: ItemGeometry,
    camera: Option<SceneCamera>,
    percent: u32,
) -> Snapshot {
    let uploaded = compositor.upload_model(model).expect("the model uploads");
    let mut layer = compositor
        .layer(WIDTH * percent / 100, HEIGHT * percent / 100)
        .expect("a layer");
    layer.set_extent(FrameSize::new(WIDTH, HEIGHT).expect("non-zero"));
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera,
                lighting: Lighting::Unlit,
                lights: None,
                shadows: &[],
                environment: None,
                floor: None,
                materials: &[],
                textures: &[],
                reflection: None,
                probe: None,
            },
            &layer,
        )
        .expect("the model draws");
    let items = [Item {
        camera,
        ..crate::tests::item(&layer, Placement::Layer(geometry), Crop::NONE, 1.0)
    }];
    render(compositor, &items)
}

/// ADR-0197: a model drawn into a layer with half the canvas's pixels each way, which
/// stands for the whole canvas, shows where it does at full size: the same inside, and
/// an edge within two pixels of the sharp one.
#[test]
fn golden_model_drawn_with_fewer_pixels_shows_in_the_same_place() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let model = two_boxes([
        ([0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [255, 0, 0]),
        ([0.0, 0.0, 0.0], [0.1, 0.1, 0.01], [255, 0, 0]),
    ]);
    let geometry = boxed(80.0, 50.0, 40.0);
    let camera = Some(SceneCamera::framing(canvas()));
    let full = draw_at(compositor, &model, geometry, camera, 100);
    let plain = draw(compositor, &model, geometry, camera, Lighting::Unlit);
    assert_eq!(full.rgba(), plain.rgba(), "at 100 nothing changes");
    let half = draw_at(compositor, &model, geometry, camera, 50);
    let edges = |shot: &Snapshot| {
        let row = red_row(shot, 50);
        let left = row
            .iter()
            .position(|&r| r > 128)
            .expect("the slab is drawn");
        let right = row
            .iter()
            .rposition(|&r| r > 128)
            .expect("the slab is drawn");
        (left, right)
    };
    let (sharp, soft) = (edges(&full), edges(&half));
    assert!(
        sharp.0.abs_diff(soft.0) <= 2 && sharp.1.abs_diff(soft.1) <= 2,
        "edges at full size {sharp:?}, at half {soft:?}"
    );
    assert_near(rgb(&half, 80, 50), rgb(&full, 80, 50), (80, 50));
    assert_near(rgb(&half, 10, 10), [0, 0, 0], (10, 10));
}

/// ADR-0197 with ADR-0165: depth of field on a model drawn with half the pixels blurs
/// over as much of the picture as at full size, not twice as much.
#[test]
fn golden_model_drawn_with_fewer_pixels_is_as_soft_out_of_focus() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let model = two_boxes([
        ([0.0, 0.0, 0.0], [1.0, 1.0, 0.01], [255, 0, 0]),
        ([0.0, 0.0, 0.0], [0.1, 0.1, 0.01], [255, 0, 0]),
    ]);
    let geometry = boxed(80.0, 50.0, 40.0);
    // Focused four times as far: the slab blurs by 16 * 3/4 = 12 canvas pixels.
    let camera = Some(focused_camera(framing_distance() * 4.0, 16.0));
    let ramp = |shot: &Snapshot| {
        red_row(shot, 50)[..80]
            .iter()
            .filter(|&&r| r > 10 && r < 245)
            .count()
    };
    let full = ramp(&draw_at(compositor, &model, geometry, camera, 100));
    let half = ramp(&draw_at(compositor, &model, geometry, camera, 50));
    assert!(full >= 8, "the edge ramps at full size: {full} pixels");
    assert!(
        half.abs_diff(full) <= full / 3 + 2,
        "the edge ramps over {half} canvas pixels at half size, {full} at full"
    );
}

/// Pixels of `shot` that are neither the cube's front red nor the black around it: the
/// edge's blends.
fn blended_reds(shot: &Snapshot) -> usize {
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let [r, g, b] = rgb(shot, x, y);
            (24..232).contains(&r) && g < 8 && b < 8
        })
        .count()
}

/// ADR-0242 decision 1: multisampled, a model's slanted edges are smoothed in its layer,
/// each edge texel the model's share of it, and its inside is drawn as without.
#[test]
fn golden_model_multisampling_smooths_a_slanted_edge() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mut geometry = boxed(80.0, 50.0, 50.0);
    geometry.rotation_deg = finite(17.0);
    let cube = coloured_cube();
    assert_eq!(compositor.model_antialiasing(), 1, "a new compositor");
    let aliased = draw(compositor, &cube, geometry, None, Lighting::Studio);
    assert_eq!(compositor.set_model_antialiasing(4), 4);
    let smoothed = draw(compositor, &cube, geometry, None, Lighting::Studio);
    let (before, after) = (blended_reds(&aliased), blended_reds(&smoothed));
    assert!(before <= 4, "{before} blended edge pixels drawn aliased");
    // The box's edges are about 200 pixels long.
    assert!(
        after >= 100,
        "{after} blended edge pixels drawn with 4 samples"
    );
    assert_near(rgb(&smoothed, 80, 50), FRONT, (80, 50));
    assert_near(rgb(&smoothed, 4, 4), [0, 0, 0], (4, 4));
    assert_eq!(compositor.set_model_antialiasing(1), 1);
    let again = draw(compositor, &cube, geometry, None, Lighting::Studio);
    assert_eq!(blended_reds(&again), before, "off again draws as before");
}

/// ADR-0242 decision 2: a count the device can't draw steps down to the most it can,
/// and none is less than one.
#[test]
fn a_sample_count_steps_down_to_one_the_device_draws() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    assert_eq!(compositor.set_model_antialiasing(0), 1);
    assert_eq!(compositor.set_model_antialiasing(1), 1);
    // Every device draws four.
    assert_eq!(compositor.set_model_antialiasing(4), 4);
    assert!(matches!(compositor.set_model_antialiasing(3), 1 | 2));
    assert!(matches!(compositor.set_model_antialiasing(64), 4 | 8));
    assert!(matches!(compositor.model_antialiasing(), 4 | 8));
}

/// ADR-0242 decision 3: multisampled, a model's surface drawn with its layer holds what
/// it holds single-sampled inside the model, one sample of each texel resolved, not a
/// blend, and nothing where the model isn't.
#[test]
fn a_multisampled_surface_matches_the_single_sampled_one_inside() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let camera = Some(SceneCamera::framing(canvas()));
    let sphere = compositor
        .upload_model(&grey_sphere())
        .expect("the model uploads");
    let lights = [ambient(1.0)];
    let draw = ModelDraw {
        model: &sphere,
        geometry: boxed(80.0, 50.0, 60.0),
        camera,
        lighting: Lighting::Scene,
        lights: Some(&lights[..]),
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    let mut drawn = Vec::new();
    for samples in [1, 4] {
        assert_eq!(compositor.set_model_antialiasing(samples), samples);
        let beside = [(); 3].map(|()| compositor.layer(WIDTH, HEIGHT).expect("a layer"));
        let colour = compositor.layer(WIDTH, HEIGHT).expect("a layer");
        compositor
            .draw_model_with_surface(&draw, &colour, [&beside[0], &beside[1], &beside[2]])
            .expect("the model draws");
        drawn.push(beside.each_ref().map(|layer| read_layer(compositor, layer)));
    }
    for (n, (single, multi)) in drawn[0].iter().zip(&drawn[1]).enumerate() {
        for (x, y) in [(80, 50), (70, 45), (90, 58), (2, 2), (150, 90)] {
            let (one, four) = (
                single.pixel(x, y).expect("inside"),
                multi.pixel(x, y).expect("inside"),
            );
            assert!(
                one.iter().zip(four).all(|(a, b)| a.abs_diff(b) <= 2),
                "layer {n} at ({x}, {y}): {one:?} single-sampled, {four:?} with 4"
            );
        }
    }
}

/// A model's view depth read from its layer's own tested depth
/// (`Compositor::draw_model_and_depth`) is the one `draw_model_depth` draws into an
/// emptied depth layer: for a turned cube seen at a distance, and for a blended plane
/// at least half opaque, which the layer's pass tests but writes no depth for; drawn
/// single-sampled the same texel for texel, multisampled within a level inside (the
/// mean of its samples) and differing more only along edges, where a texel the model
/// covers under three quarters of holds none. Its box is the layer's.
#[test]
fn a_view_depth_read_from_the_layer_is_the_one_drawn_again() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let plane = Shape::Plane
        .model(Material {
            base_color: [1.0, 1.0, 1.0, 0.75],
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..Material::default()
        })
        .expect("the plane builds");
    let camera = SceneCamera::framing(canvas());
    let mut geometry = boxed(80.0, 50.0, 50.0);
    geometry.depth_px = finite(-150.0);
    geometry.turn_deg = finite(35.0);
    geometry.tilt_deg = finite(20.0);
    let empty = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &[],
    };
    let read = |compositor: &mut Compositor, depth: &crate::Image| {
        let items = [crate::tests::item(
            depth,
            Placement::IDENTITY,
            Crop::NONE,
            1.0,
        )];
        render(compositor, &items)
    };
    for (name, model) in [("cube", coloured_cube()), ("blended plane", plane)] {
        let uploaded = compositor.upload_model(&model).expect("the model uploads");
        let draw = ModelDraw {
            model: &uploaded,
            geometry,
            camera: Some(camera),
            lighting: Lighting::Unlit,
            lights: None,
            shadows: &[],
            environment: None,
            floor: None,
            materials: &[],
            textures: &[],
            reflection: None,
            probe: None,
        };
        for samples in [1, 4] {
            let samples = compositor.set_model_antialiasing(samples);
            let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
            let drawn_again = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
            compositor
                .draw_model(&draw, &layer)
                .expect("the model draws");
            compositor
                .render_view_depth(&empty, &drawn_again)
                .expect("it clears");
            compositor
                .draw_model_depth(&draw, &drawn_again)
                .expect("the depth draws");
            let read_back = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
            compositor
                .draw_model_and_depth(&draw, &layer, None, &read_back)
                .expect("the model and its depth draw");
            assert_eq!(read_back.content(), layer.content(), "{name}: its box");
            let (want, got) = (read(compositor, &drawn_again), read(compositor, &read_back));
            let centre = rgb(&want, 80, 50);
            assert!(centre[0] > 40, "{name}: the depth is drawn: {centre:?}");
            // Multisampled, a texel holds the mean of its samples, its centre's depth on
            // a plane: a level apart at most.
            let near = rgb(&got, 80, 50)[0].abs_diff(centre[0]);
            assert!(
                near <= u8::from(samples > 1),
                "{name} at {samples}x: the centre, {centre:?} against {:?}",
                rgb(&got, 80, 50)
            );
            let mut differing = 0;
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let (a, b) = (rgb(&want, x, y)[0], rgb(&got, x, y)[0]);
                    if a.abs_diff(b) > 1 {
                        differing += 1;
                    }
                }
            }
            let allowed = if samples == 1 {
                0
            } else {
                2 * (WIDTH + HEIGHT)
            };
            assert!(
                differing <= allowed,
                "{name} at {samples}x: {differing} texels differ"
            );
        }
    }
}

/// A model's textures are block compressed on upload where the device samples BC
/// textures: an opaque colour texture as BC1, one with alpha as BC3, a linear one too;
/// a normal map, whose normals four colours a block would bend into steps, and a texture
/// whose sides aren't whole blocks of 4 stay RGBA8. A device without BC keeps them all
/// RGBA8. A compressed texture draws as its texels: a plane of one colour per quadrant
/// reads each.
#[test]
fn golden_model_textures_are_block_compressed_but_normal_maps() {
    let Some(mut opened) = open() else {
        return;
    };
    let quadrants = |side: u32, alpha: u8| -> Vec<u8> {
        (0..side)
            .flat_map(|y| {
                (0..side).flat_map(move |x| match (x < side / 2, y < side / 2) {
                    (true, true) => [255, 0, 0, alpha],
                    (false, true) => [0, 255, 0, alpha],
                    (true, false) => [0, 0, 255, alpha],
                    (false, false) => [255, 255, 255, alpha],
                })
            })
            .collect()
    };
    let textures = vec![
        ModelTexture::new(16, 16, quadrants(16, 255), TextureColor::Srgb).expect("a texture"),
        ModelTexture::new(16, 16, quadrants(16, 128), TextureColor::Srgb).expect("a texture"),
        ModelTexture::new(16, 16, quadrants(16, 255), TextureColor::Linear).expect("a texture"),
        ModelTexture::new(
            16,
            16,
            [128, 128, 255, 255].repeat(256),
            TextureColor::Linear,
        )
        .expect("a texture"),
        ModelTexture::new(6, 6, quadrants(6, 255), TextureColor::Srgb).expect("a texture"),
    ];
    let plane = Shape::Plane
        .model(Material::default())
        .expect("the plane builds");
    let textured = |base: usize| Material {
        base_color_texture: Some(TextureRef { texture: base }),
        unlit: true,
        ..plane.materials()[0].clone()
    };
    let materials = vec![
        Material {
            metallic_roughness_texture: Some(TextureRef { texture: 2 }),
            normal_texture: Some(TextureRef { texture: 3 }),
            ..textured(0)
        },
        textured(1),
        textured(4),
    ];
    let model =
        Model::new(plane.primitives().to_vec(), materials, textures).expect("the plane builds");
    let uploaded = opened
        .compositor
        .upload_model(&model)
        .expect("the model uploads");
    use wgpu::TextureFormat as F;
    let bc = opened
        .compositor
        .device()
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    let expected = if bc {
        [
            F::Bc1RgbaUnormSrgb,
            F::Bc3RgbaUnormSrgb,
            F::Bc1RgbaUnorm,
            F::Rgba8Unorm,
            F::Rgba8UnormSrgb,
        ]
    } else {
        [
            F::Rgba8UnormSrgb,
            F::Rgba8UnormSrgb,
            F::Rgba8Unorm,
            F::Rgba8Unorm,
            F::Rgba8UnormSrgb,
        ]
    };
    assert_eq!(uploaded.texture_formats(), expected, "BC sampled: {bc}");
    let single = Model::new(
        plane.primitives().to_vec(),
        vec![textured(0)],
        vec![ModelTexture::new(16, 16, quadrants(16, 255), TextureColor::Srgb).expect("a texture")],
    )
    .expect("the plane builds");
    let shot = draw(
        &mut opened.compositor,
        &single,
        boxed(80.0, 50.0, 80.0),
        None,
        Lighting::Studio,
    );
    assert_near(rgb(&shot, 60, 30), [255, 0, 0], (60, 30));
    assert_near(rgb(&shot, 100, 30), [0, 255, 0], (100, 30));
    assert_near(rgb(&shot, 60, 70), [0, 0, 255], (60, 70));
    assert_near(rgb(&shot, 100, 70), [255, 255, 255], (100, 70));
}

/// A model texture the GPU has no memory for is uploaded without its largest level,
/// then the next, until it fits, and draws as its texels, softer; one with no level that
/// fits fails the upload. Here the GPU takes at most 8 × 8 texels, then none.
#[test]
fn a_texture_short_of_memory_is_uploaded_without_its_largest_levels() {
    let Some(mut opened) = open() else {
        return;
    };
    let quadrants = |side: u32| -> Vec<u8> {
        (0..side)
            .flat_map(|y| {
                (0..side).flat_map(move |x| match (x < side / 2, y < side / 2) {
                    (true, true) => [255, 0, 0, 255],
                    (false, true) => [0, 255, 0, 255],
                    (true, false) => [0, 0, 255, 255],
                    (false, false) => [255, 255, 255, 255],
                })
            })
            .collect()
    };
    let plane = Shape::Plane
        .model(Material::default())
        .expect("the plane builds");
    let textured = Material {
        base_color_texture: Some(TextureRef { texture: 0 }),
        unlit: true,
        ..plane.materials()[0].clone()
    };
    let with = |texture: ModelTexture| {
        Model::new(
            plane.primitives().to_vec(),
            vec![textured.clone()],
            vec![texture],
        )
        .expect("the plane builds")
    };
    let mipped =
        with(ModelTexture::new(32, 32, quadrants(32), TextureColor::Srgb).expect("a texture"));
    super::TEXTURE_MEMORY_TEXELS.with(|most| most.set(Some(64)));
    let uploaded = opened.compositor.upload_model(&mipped);
    let shot = draw(
        &mut opened.compositor,
        &mipped,
        boxed(80.0, 50.0, 80.0),
        None,
        Lighting::Studio,
    );
    super::TEXTURE_MEMORY_TEXELS.with(|most| most.set(Some(0)));
    let unfit = opened.compositor.upload_model(&mipped);
    super::TEXTURE_MEMORY_TEXELS.with(|most| most.set(None));
    let uploaded = uploaded.expect("the model uploads");
    assert_eq!(uploaded.texture_sizes(), [(8, 8)], "two levels left out");
    assert_near(rgb(&shot, 60, 30), [255, 0, 0], (60, 30));
    assert_near(rgb(&shot, 100, 30), [0, 255, 0], (100, 30));
    assert_near(rgb(&shot, 60, 70), [0, 0, 255], (60, 70));
    assert_near(rgb(&shot, 100, 70), [255, 255, 255], (100, 70));
    assert!(unfit.is_err(), "no level fits");
}

/// A model's layer drawn again of the same draw is left as it is, its view depth too,
/// and holds the same pixels (ADR-0246): neither is marked as changed. A new placement,
/// a material's drive, another sample count, or a write into the layer since, draws it
/// again.
#[test]
fn a_layer_still_holding_the_same_model_draw_is_kept() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    compositor.set_model_antialiasing(1);
    let uploaded = compositor
        .upload_model(&coloured_cube())
        .expect("the model uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let depth = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
    let camera = SceneCamera::framing(canvas());
    let mut turned = boxed(80.0, 50.0, 50.0);
    turned.turn_deg = finite(30.0);
    let brighter = [super::MaterialDrive {
        color: 0.5,
        ..super::MaterialDrive::default()
    }];
    let draw = |geometry, materials| ModelDraw {
        model: &uploaded,
        geometry,
        camera: Some(camera),
        lighting: Lighting::Studio,
        lights: None,
        shadows: &[],
        environment: None,
        floor: None,
        materials,
        textures: &[],
        reflection: None,
        probe: None,
    };
    // Whether drawing `draw` wrote the layer and its view depth: both or neither.
    let drawn = |compositor: &mut Compositor, draw: &ModelDraw<'_>| {
        let was = (layer.generation(), depth.generation());
        compositor
            .draw_model_and_depth(draw, &layer, None, &depth)
            .expect("the model draws");
        let wrote = (layer.generation() != was.0, depth.generation() != was.1);
        assert_eq!(wrote.0, wrote.1, "the layer and its depth go together");
        wrote.0
    };
    let shown = |compositor: &mut Compositor| {
        let items = [crate::tests::item(
            &layer,
            Placement::IDENTITY,
            Crop::NONE,
            1.0,
        )];
        rgb(&render(compositor, &items), 80, 50)
    };
    let still = draw(boxed(80.0, 50.0, 50.0), &[]);
    assert!(drawn(compositor, &still), "first drawn");
    let first = shown(compositor);
    assert!(!drawn(compositor, &still), "the same draw again");
    assert_eq!(shown(compositor), first, "it holds the same pixels");
    assert!(drawn(compositor, &draw(turned, &[])), "turned");
    assert!(!drawn(compositor, &draw(turned, &[])), "turned again");
    assert!(drawn(compositor, &draw(turned, &brighter)), "driven");
    assert!(!drawn(compositor, &draw(turned, &brighter)), "driven again");
    layer.mark_changed();
    assert!(drawn(compositor, &draw(turned, &brighter)), "written since");
    if compositor.set_model_antialiasing(4) == 4 {
        assert!(drawn(compositor, &draw(turned, &brighter)), "multisampled");
        assert!(
            !drawn(compositor, &draw(turned, &brighter)),
            "multisampled again"
        );
    }
    // Without its view depth it is another draw.
    let was = layer.generation();
    compositor
        .draw_model(&draw(turned, &brighter), &layer)
        .expect("the model draws");
    assert_ne!(layer.generation(), was, "drawn without its depth");
}

/// `warm_3d` makes a model layer's pipelines at the samples set and at one, so a first
/// draw at either compiles nothing, multisampled or not, with its view depth.
#[test]
fn warming_3d_makes_the_model_layers_pipelines_at_its_samples() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    if compositor.set_model_antialiasing(4) != 4 {
        return;
    }
    let uploaded = compositor
        .upload_model(&coloured_cube())
        .expect("the model uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    let depth = compositor.depth_layer(WIDTH, HEIGHT).expect("a layer");
    compositor.warm_3d();
    let made = compositor.pipelines_made();
    let draw = ModelDraw {
        model: &uploaded,
        geometry: boxed(80.0, 50.0, 50.0),
        camera: Some(SceneCamera::framing(canvas())),
        lighting: Lighting::Studio,
        lights: None,
        shadows: &[],
        environment: None,
        floor: None,
        materials: &[],
        textures: &[],
        reflection: None,
        probe: None,
    };
    for samples in [4, 1] {
        compositor.set_model_antialiasing(samples);
        compositor
            .draw_model_and_depth(&draw, &layer, None, &depth)
            .expect("the model draws");
        assert_eq!(
            compositor.pipelines_made(),
            made,
            "drawn at {samples}x, nothing more made"
        );
    }
}
