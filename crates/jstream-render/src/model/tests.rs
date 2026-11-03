//! Model goldens (ADR-0107): built-in shapes drawn into a layer and placed with
//! [`Placement::Layer`], read back and checked pixel by pixel against where the shared
//! placement ([`jstream_protocol::Solid`]) says each face lands.

use jstream_media::FrameSize;
use jstream_media::model::shapes::Shape;
use jstream_media::model::{AlphaMode, Material, Model, ModelTexture, TextureColor, TextureRef};
use jstream_protocol::{Canvas, Finite32, ItemGeometry, SceneCamera, Size, Vec2, Vec3};

use super::{Lighting, ModelDraw};
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
    let uploaded = compositor.upload_model(model).expect("the model uploads");
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera,
                lighting,
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
        },
        &layer,
    );
    assert!(matches!(
        drawn,
        Err(crate::RenderError::Invalid { what: "model", .. })
    ));
}
