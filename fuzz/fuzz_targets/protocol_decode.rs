//! Protocol message decode: any text a client or server sends.
//!
//! Parsing must never panic. A client message that parses must serialize, and the
//! serialized form must parse back to the same message (and so serialize to the same
//! text): what the server echoes or logs is what the client sent.

#![no_main]

use jstream_protocol::{ClientMessage, ServerMessage};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(message) = ClientMessage::parse(text) {
        let json = serde_json::to_string(&message).expect("a parsed message serializes");
        let again = ClientMessage::parse(&json)
            .unwrap_or_else(|e| panic!("{json} doesn't parse back: {e}"));
        assert_eq!(again, message, "{json} parses back to another message");
    }
    // Clients parse what a (possibly newer) server sends.
    if let Ok(message) = serde_json::from_str::<ServerMessage>(text) {
        let json = serde_json::to_string(&message).expect("a parsed message serializes");
        let again: ServerMessage = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{json} doesn't parse back: {e}"));
        assert_eq!(again, message, "{json} parses back to another message");
    }
});
