use super::*;
use jstream_media::Timebase;
use jstream_protocol::Canvas;

fn ms(ms: i64) -> MediaTime {
    MediaTime::new(ms, Timebase::new(1, 1000).expect("timebase"))
}

fn camera() -> SceneCamera {
    SceneCamera::framing(Canvas {
        width: 1920,
        height: 1080,
    })
}

fn xyz(v: Vec3) -> [f64; 3] {
    [v.x.get().into(), v.y.get().into(), v.z.get().into()]
}

fn close(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

fn aim(pan: f32, tilt: f32, zoom: f32) -> CameraAim {
    CameraAim {
        pan_deg: Finite32::new(pan),
        tilt_deg: Finite32::new(tilt),
        zoom: Finite32::new(zoom),
    }
}

#[test]
fn the_home_aim_leaves_the_camera_as_it_is() {
    let camera = camera();
    assert_eq!(turn(camera, (0.0, 0.0, 1.0)), camera);
    assert_eq!(Aim::default().apply(camera, ms(5000)), camera);
    let info = Aim::default().info(ms(0));
    assert_eq!(
        (
            info.pan_deg.get(),
            info.tilt_deg.get(),
            info.zoom.get(),
            info.moving
        ),
        (0.0, 0.0, 1.0, false)
    );
}

#[test]
fn pan_turns_right_tilt_turns_up_and_zoom_narrows_the_view() {
    let camera = camera();
    let p = xyz(camera.position);
    let t = xyz(camera.target);
    let distance = p[2] - t[2];

    // A quarter turn right: the camera now looks along +x, as far away as before.
    let right = turn(camera, (90.0, 0.0, 1.0));
    let r = xyz(right.target);
    assert!(close(r[0], p[0] + distance, 0.5), "{r:?}");
    assert!(close(r[1], p[1], 0.01) && close(r[2], p[2], 0.5), "{r:?}");
    assert_eq!(right.position, camera.position);

    // Tilting up raises the target: up is -y.
    let up = turn(camera, (0.0, 30.0, 1.0));
    let u = xyz(up.target);
    let rise = p[1] - u[1];
    assert!(
        close(rise, distance * 30f64.to_radians().sin(), 0.5),
        "{u:?}"
    );

    // The view stops short of straight up, however far the tilt goes.
    let top = turn(camera, (0.0, 89.0, 1.0));
    let past = turn(top, (0.0, 89.0, 1.0));
    let pitch = |c: SceneCamera| {
        let (p, t) = (xyz(c.position), xyz(c.target));
        let d = [t[0] - p[0], t[1] - p[1], t[2] - p[2]];
        (-d[1]).atan2(d[0].hypot(d[2])).to_degrees()
    };
    assert!(close(pitch(past), 89.0, 0.01), "{}", pitch(past));

    // Twice the zoom halves the tangent of the half-angle.
    let zoomed = turn(camera, (0.0, 0.0, 2.0));
    let want = (2.0 * ((22.5f64).to_radians().tan() / 2.0).atan()).to_degrees();
    assert!(close(zoomed.fov_deg.get().into(), want, 0.001));
    assert_eq!(zoomed.target, camera.target);
}

#[test]
fn the_rates_run_from_slow_steps_to_fast_ones() {
    assert_eq!(pan_rate(0), 0.0);
    assert!(close(pan_rate(1), 1.0, 1e-9));
    assert!(close(pan_rate(-24), -80.0, 1e-9));
    assert!(close(tilt_rate(23), 80.0, 1e-9));
    assert!(close(zoom_rate(8), 1.0, 1e-9));
    assert!(close(zoom_rate(-1), -0.1, 1e-9));
    // Out-of-range steps are held to the top one.
    assert!(close(pan_rate(i8::MAX), 80.0, 1e-9));
    assert!(
        pan_rate(12) < 80.0 / 2.0,
        "the curve is finer at the low steps"
    );
}

#[test]
fn a_drive_runs_until_its_limit_and_stops_when_its_session_ends() {
    let me = ClientId::new("me");
    let other = ClientId::new("other");
    let mut a = Aim::default();
    a.drive(ms(1000), (Some(24), Some(0), Some(8)), &me);
    let (pan, tilt, zoom) = a.at(ms(2000));
    assert!(close(pan, 80.0, 1e-6) && tilt == 0.0, "{pan} {tilt}");
    assert!(close(zoom, 1f64.exp(), 1e-6), "{zoom}");
    assert!(a.info(ms(2000)).moving);
    // Pan stops at 170°, zoom at 8: then it isn't moving any more.
    let (pan, _, zoom) = a.at(ms(10_000));
    assert!(
        close(pan, 170.0, 1e-9) && close(zoom, 8.0, 1e-6),
        "{pan} {zoom}"
    );
    assert!(!a.info(ms(10_000)).moving);

    // Another session ending doesn't stop it; its own does, where it is then.
    let mut b = Aim::default();
    b.drive(ms(0), (Some(-12), Some(6), Some(0)), &me);
    assert!(!b.forget(ms(500), &other));
    let held = b.at(ms(500));
    assert!(b.forget(ms(500), &me));
    assert_eq!(b.at(ms(9000)), held);
    assert!(!b.info(ms(9000)).moving);

    // A drive at 0 on every axis is a stop, and leaves no session driving it.
    let mut c = Aim::default();
    c.drive(ms(0), (Some(5), Some(0), Some(0)), &me);
    c.drive(ms(1000), (Some(0), Some(0), Some(0)), &me);
    assert!(!c.forget(ms(1000), &me));
    assert_eq!(c.at(ms(1000)), c.at(ms(5000)));

    // An axis a drive leaves out keeps moving: the zoom rocker doesn't stop the pan.
    let mut d = Aim::default();
    d.drive(ms(0), (Some(24), None, None), &me);
    d.drive(ms(1000), (None, None, Some(8)), &me);
    let (pan, _, zoom) = d.at(ms(1500));
    assert!(close(pan, 120.0, 1e-6) && zoom > 1.0, "{pan} {zoom}");
    d.drive(ms(1500), (None, None, Some(0)), &me);
    assert!(d.info(ms(1600)).moving, "the pan still moves");
}

#[test]
fn an_aim_glides_every_axis_it_sets_to_arrive_together() {
    let me = ClientId::new("me");
    let mut a = Aim::default();
    // At the top speed, 80°/s: 40° of pan takes half a second, and the tilt's 10°
    // arrives with it.
    a.aim(ms(0), &aim(40.0, -10.0, 1.0), None);
    let (pan, tilt, _) = a.at(ms(250));
    assert!(
        close(pan, 20.0, 1e-6) && close(tilt, -5.0, 1e-6),
        "eased halfway: {pan} {tilt}"
    );
    assert!(a.info(ms(250)).moving);
    let (pan, tilt, zoom) = a.at(ms(500));
    assert!(close(pan, 40.0, 1e-9) && close(tilt, -10.0, 1e-9) && close(zoom, 1.0, 1e-9));
    assert!(!a.info(ms(600)).moving);

    // A slower speed takes longer to the same place.
    let mut slow = Aim::default();
    slow.aim(ms(0), &aim(40.0, 0.0, 1.0), Some(1));
    assert!(slow.at(ms(500)).0 < 1.0);
    assert!(close(slow.at(ms(40_000)).0, 40.0, 1e-9));

    // Axes an aim leaves out keep moving; the ones it sets stop driving.
    let mut b = Aim::default();
    b.drive(ms(0), (Some(24), Some(23), Some(0)), &me);
    b.aim(
        ms(1000),
        &CameraAim {
            tilt_deg: Finite32::new(0.0),
            ..CameraAim::default()
        },
        None,
    );
    let (pan, tilt, _) = b.at(ms(1500));
    assert!(close(pan, 120.0, 1e-6), "the pan drive goes on: {pan}");
    assert!(tilt < 80.0, "the tilt glides back: {tilt}");
    assert!(
        b.forget(ms(1500), &me),
        "the pan still belongs to its session"
    );

    // An aim past an axis's limit is held to it.
    let mut c = Aim::default();
    c.aim(ms(0), &aim(10.0, 0.0, 1.0), None);
    let mut far = c.clone();
    far.aim(ms(0), &aim(170.0, 0.0, 1.0), None);
    assert!(close(far.at(ms(60_000)).0, 170.0, 1e-9));
}
