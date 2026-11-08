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
    let layer = compositor.layer(WIDTH, HEIGHT).expect("a layer");
    compositor
        .draw_model(
            &ModelDraw {
                model: &uploaded,
                geometry,
                camera,
                lighting,
                lights,
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
                    lights: None,
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
            lights: None,
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
