//! The HEVC decoder configuration record, from encoder output.
//!
//! Nothing may panic. A configuration that builds must serialize.

#![no_main]

use jstream_mux::hevc::HevcDecoderConfig;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(config) = HevcDecoderConfig::from_annexb(data) {
        let _ = config.to_bytes();
    }
});
