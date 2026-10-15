<!-- @doc
id = "FUZZ"
title = "Fuzz targets"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Fuzz targets

[cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) targets (libFuzzer) for the
code that reads input JStream doesn't control: protocol messages, kind settings, the
collection file, and what encoders and RTMP servers send.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`, so `cargo test --workspace`, clippy and `cargo deny` never build it, and its
fuzzing dependencies never reach JStream. It needs a nightly toolchain, because libFuzzer
needs the sanitizer flags only nightly accepts.

<a id="targets"></a>
## Targets
<!-- @section
covers = ["fuzz/fuzz_targets/**"]
verified = { hash = "b3:9715d9a5160f0d87", by = "agent:claude@session_01FFzJN8ifSytfHpv52HAhhR", note = "Q1 r3: amf0 header reworded (N2) to the property it asserts; README row unchanged and still accurate" }
-->

No target may panic, overflow or run out of memory on any input. Most also check a
property of what they accept:

| Target | Input | Also checks |
|---|---|---|
| `protocol_decode` | One JSON text message, parsed as a `ClientMessage` and as a `ServerMessage` | What parses serializes, and parses back to the same message |
| `kind_settings` | A byte picking a built-in kind (`jstream-sources`), then a JSON object of settings | Settings the graph accepts on `source.create` are accepted as a `source.set_settings` patch that undoes exactly, and create a source (`Registry::create`, a frame, an update) without a panic. Settings with an asset reference stop before `create`, which would read a file |
| `collection_load` | A collection file, loaded with the built-in kinds | A file that loads saves, and the saved file loads back to the same state and `rev` |
| `amf0` | AMF0 values, as RTMP command messages carry them | Encoding what decodes is a fixed point (bytes are compared, since NaN ≠ NaN) |
| `h264` | Annex B and AVCC H.264, and the `AVCDecoderConfigurationRecord` built from SPS and PPS | Annex B → AVCC → Annex B keeps the same NAL units |
| `hevc` | Annex B HEVC, and the `HEVCDecoderConfigurationRecord` built from VPS, SPS and PPS | |
| `av1` | Low-overhead AV1 OBUs, and the `AV1CodecConfigurationRecord` | An OBU rebuilt with its size field parses the same; a temporal unit's sample keeps every OBU except temporal delimiters and padding |
| `aac` | An `AudioSpecificConfig` | It keeps its bytes, and they parse to the same config |
| `mux` | A container, tracks, a time base and origin, then packets (keyframe flag, track, PTS, DTS offset, payload) | The fMP4 muxer and the FLV tagger (H.264, HEVC and AV1 through enhanced RTMP) end bad packets and timestamp jumps in errors |

`jstream-mux` writes FLV and fMP4 but parses neither, so there is no container parser to
fuzz; the `mux` target covers the parsing the muxers do on their way in.

<a id="running"></a>
## Running
<!-- @section
covers = ["fuzz/Cargo.toml", "fuzz/seed.sh"]
verified = { hash = "b3:41d6d36341f48281", by = "agent:claude@session_01FFzJN8ifSytfHpv52HAhhR", note = "Q1 r3: Cargo.toml now also forbids unsafe_code and denies undocumented_unsafe_blocks (B1); commands, seeds and layout unchanged" }
-->

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
cd fuzz
./seed.sh                               # seed corpora from the repository's own fixtures
cargo +nightly fuzz run protocol_decode -- -max_total_time=600
```

`seed.sh` writes into `corpus/<target>/`: every message in the protocol's example
snapshot (`crates/jstream-protocol/schema/examples.snap`), the collection file fixtures
(`crates/jstream-graph/src/persist/fixtures/`), and a few settings maps for each kind.
The other targets start from nothing and find their structure in seconds.

`corpus/`, `artifacts/` and `coverage/` stay out of git. A crash is saved under
`artifacts/<target>/`; `cargo +nightly fuzz run <target> <file>` replays it, and
`cargo +nightly fuzz tmin <target> <file>` shrinks it. Each crash is fixed in its own
PR, with a regression test in the crate that crashed
([R-TEST-001](../docs/RULES.md#R-TEST-001)).

The graph's property tests live with the crate, in
[`crates/jstream-graph/tests/properties.rs`](../crates/jstream-graph/tests/properties.rs),
and run in `cargo test`.
