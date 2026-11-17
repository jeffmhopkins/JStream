use symphonia::core::audio::{Channels, Position};

use super::Downmix;
use jstream_platform::media::ChannelLayout;

#[test]
fn mono_and_stereo_pass_through() {
    let mono = Downmix::new(&Channels::Discrete(1)).expect("mono");
    assert_eq!(mono.layout(), ChannelLayout::Mono);
    assert_eq!(mono.apply(vec![0.5, -0.25]), vec![0.5, -0.25]);
    let stereo = Downmix::new(&Channels::Positioned(
        Position::FRONT_LEFT | Position::FRONT_RIGHT,
    ))
    .expect("stereo");
    assert_eq!(stereo.layout(), ChannelLayout::Stereo);
    assert_eq!(stereo.apply(vec![0.5, -0.25]), vec![0.5, -0.25]);
    assert_eq!(Downmix::new(&Channels::Discrete(0)), None);
}

#[test]
fn five_one_folds_into_stereo_without_clipping() {
    // FL FR FC LFE BL BR, in bit order.
    let positions = Position::FRONT_LEFT
        | Position::FRONT_RIGHT
        | Position::FRONT_CENTER
        | Position::LFE1
        | Position::REAR_LEFT
        | Position::REAR_RIGHT;
    let mix = Downmix::new(&Channels::Positioned(positions)).expect("5.1");
    assert_eq!(mix.layout(), ChannelLayout::Stereo);
    // Full scale on every channel stays within full scale.
    let out = mix.apply(vec![1.0; 6]);
    assert_eq!(out.len(), 2);
    for s in &out {
        assert!(*s <= 1.0 + 1e-6 && *s > 0.9, "{s}");
    }
    // The LFE is left out, and left only reaches the left.
    let out = mix.apply(vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    assert_eq!(out, vec![0.0, 0.0]);
    let out = mix.apply(vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert!(out[0] > 0.0 && out[1] == 0.0, "{out:?}");
}

#[test]
fn discrete_channels_keep_the_first_two() {
    let mix = Downmix::new(&Channels::Discrete(4)).expect("four");
    assert_eq!(mix.apply(vec![0.1, 0.2, 0.3, 0.4]), vec![0.1, 0.2]);
}
