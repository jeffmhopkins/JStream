//! The AAC `AudioSpecificConfig`, from encoder output.
//!
//! Nothing may panic. A config that parses keeps its bytes, and they parse to the same
//! config.

#![no_main]

use jstream_mux::aac::AudioSpecificConfig;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(config) = AudioSpecificConfig::from_bytes(data.to_vec()) else {
        return;
    };
    assert_eq!(config.as_bytes(), data);
    let again =
        AudioSpecificConfig::from_bytes(config.as_bytes().to_vec()).expect("its own bytes parse");
    assert_eq!(again, config);
});
