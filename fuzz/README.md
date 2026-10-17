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
collection file, image and font files, and what encoders and RTMP servers send.

The crate is not a workspace member. It has its own `[workspace]` table and its own
`Cargo.lock`, so `cargo test --workspace`, clippy and `cargo deny` never build it, and its
fuzzing dependencies never reach JStream. Only the nightly [`fuzz` workflow](#ci) builds
it. It needs a nightly toolchain, because libFuzzer needs the sanitizer flags only nightly
accepts.

<a id="targets"></a>
## Targets
<!-- @section
covers = ["fuzz/fuzz_targets/**"]
-->

No target may panic, overflow or run out of memory on any input. Most also check a
property of what they accept:

| Target | Input | Also checks |
|---|---|---|
| `protocol_decode` | One JSON text message, parsed as a `ClientMessage` and as a `ServerMessage` | What parses serializes, and parses back to the same message |
| `kind_settings` | A byte picking a built-in kind (`jstream-sources`), then a JSON object of settings | Settings the graph accepts on `source.create` are accepted as a `source.set_settings` patch that undoes exactly, and create a source (`Registry::create`, a frame, an update) without a panic. Settings with an asset reference stop before `create`, which would read a file; `image_decode` and `font_load` fuzz the files themselves |
| `collection_load` | A collection file, loaded with the built-in kinds | A file that loads saves, and the saved file loads back to the same state and `rev` |
| `amf0` | AMF0 values, as RTMP command messages carry them | Encoding what decodes is a fixed point (bytes are compared, since NaN ≠ NaN) |
| `h264` | Annex B and AVCC H.264, and the `AVCDecoderConfigurationRecord` built from SPS and PPS | Annex B → AVCC → Annex B keeps the same NAL units |
| `hevc` | Annex B HEVC, and the `HEVCDecoderConfigurationRecord` built from VPS, SPS and PPS | |
| `av1` | Low-overhead AV1 OBUs, and the `AV1CodecConfigurationRecord` | An OBU rebuilt with its size field parses the same; a temporal unit's sample keeps every OBU except temporal delimiters and padding |
| `aac` | An `AudioSpecificConfig` | It keeps its bytes, and they parse to the same config |
| `mux` | A container, tracks, a time base and origin, then packets (keyframe flag, track, PTS, DTS offset, payload) | The fMP4 muxer and the FLV tagger (H.264, HEVC and AV1 through enhanced RTMP) end bad packets and timestamp jumps in errors |
| `image_decode` | An image file, read as a `collection` asset and decoded by the `image` kind's `Registry::create` (PNG, JPEG, WebP or GIF, picked by its first bytes) | A created source draws at several times, and each change it announces is later than the time asked. The limits are lower than the defaults (4096 px a side, 256 MiB decoded), to stay inside libFuzzer's memory limit |
| `font_load` | A font file or collection, read as a `collection` asset, parsed and drawn by the `text` kind's `Registry::create` | A font that loads draws its text, and draws again on a live update with wrapping and an outline. The limits are lower, as for `image_decode` |

`jstream-mux` writes FLV and fMP4 but parses neither, so there is no container parser to
fuzz; the `mux` target covers the parsing the muxers do on their way in.

<a id="running"></a>
## Running
<!-- @section
covers = ["fuzz/Cargo.toml", "fuzz/seed.sh"]
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
(`crates/jstream-graph/src/persist/fixtures/`), a few settings maps for each kind, the
image files in `crates/jstream-sources/tests/data/` (JPEG, WebP and the text golden
PNGs) with a 1x1 GIF it writes itself, and the bundled Roboto subset for `font_load`.
The other targets start from nothing and find their structure in seconds.

`corpus/`, `artifacts/` and `coverage/` stay out of git. A crash is saved under
`artifacts/<target>/`; `cargo +nightly fuzz run <target> <file>` replays it, and
`cargo +nightly fuzz tmin <target> <file>` shrinks it. Each crash is fixed in its own
PR, with a regression test in the crate that crashed
([R-TEST-001](../docs/RULES.md#R-TEST-001)).

The graph's property tests live with the crate, in
[`crates/jstream-graph/tests/properties.rs`](../crates/jstream-graph/tests/properties.rs),
and run in `cargo test`.

<a id="ci"></a>
## In CI
<!-- @section
covers = [".github/workflows/fuzz.yml"]
-->

The scheduled `fuzz` workflow ([R-GIT-003](../docs/RULES.md#R-GIT-003)) runs every
target, nightly and when started by hand from the Actions tab. It never runs on a PR and
is not required. It installs a pinned nightly and a pinned cargo-fuzz release (both
bumped by hand), builds the targets, runs `seed.sh`, then runs each target for three
minutes from its seeds, with a 30 s timeout per input and libFuzzer's 2 GiB memory limit.
Corpora are not kept between runs.

A crash, hang or out-of-memory input is a finding. It is uploaded as the run's
`fuzz-artifacts` artifact, and an issue titled `Fuzz crash: <target>` is opened, or the
open one gets a comment, with the run link and the end of the target's log. Findings
never fail the job; a target that exits with an error and saves no input does, and so
does a build or install that fails.
