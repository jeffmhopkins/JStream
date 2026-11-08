//! A VISCA message or VISCA-over-IP datagram from the LAN, read as a camera reads a
//! controller's (JStream's virtual camera, #1181) and as JStream reads a camera's reply
//! (#1182).
//!
//! Nothing may panic. A request or reply that parses encodes back to the same bytes.

#![no_main]

use jstream_visca::{Reply, Request, ip};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let message = match ip::parse(data) {
        Ok(d) => d.payload,
        Err(_) => data,
    };
    if let Ok((to, request)) = Request::parse(message) {
        let bytes = match request {
            Request::Command(c) => c.encode(to).expect("a parsed command encodes"),
            Request::Inquiry(q) => q.encode(to),
        };
        assert_eq!(bytes.as_slice(), message);
    }
    if let Ok((from, reply)) = Reply::parse(message) {
        let bytes = reply.encode(from).expect("a parsed reply encodes");
        assert_eq!(bytes.as_slice(), message);
    }
    let _ = ip::next_message(data);
});
