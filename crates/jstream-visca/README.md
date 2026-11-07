<!-- @doc
id = "CRATE-jstream-visca"
title = "jstream-visca"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-visca

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-visca/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-visca"]
-->

Layer-0 VISCA format for PTZ control
([ADR-0122](../../docs/adr/0122-controller-input-midi-and-one-binding-system.md)), in both
directions: JStream driving physical PTZ heads (#1182) and a PTZ controller driving
JStream's virtual cameras (#1181). No sockets, no threads, no allocation and no
dependencies. A message is at most 16 bytes and is held in a fixed `Bytes`.

The command bytes follow the commonly used VISCA set that #1182 and #1181 list, which
Sony's [VISCA command list](https://pro.sony/s3/2022/09/03065933/VISCA_Command_List_v4.pdf)
documents. That document hasn't been re-read against this crate yet.

Contents:
- `Address`: cameras 1 to 7 and broadcast (8). Commands start `8x`; replies start
  `y0`, where `y` is 8 plus the address.
- `Command`: IF_Clear, pan-tilt drive (speeds 1 to `18`h and 1 to `17`h), absolute and
  relative position, home, reset, zoom and focus drive (standard or variable speed 0 to
  7) and direct, focus mode and one-push, preset reset, set and recall (0 to 127), and
  power. Positions are signed nibbles, with pan in five nibbles (Sony) or four
  (`PositionWidth`). `encode` refuses a value the message can't carry.
- `Inquiry`: power, zoom, focus, focus mode and pan-tilt position.
- `Request::parse`: a message to a camera. It tells malformed bytes (`Error::Syntax`,
  which a camera answers with a syntax error) from a well-formed message this crate
  doesn't know (`Error::Unsupported`, answered "not executable"), such as exposure.
- `Reply`: ACK, completion, inquiry data and errors (`ErrorKind`), parsed and encoded.
  `InquiryValue` decodes the data for the inquiry asked, and encodes the reply a virtual
  camera answers with.
- `ip`: Sony-style VISCA over IP (UDP port 52381): the 8-byte header (payload type,
  length, sequence number), its control messages (reset, sequence and message errors),
  `Sequence` for a controller's numbering, and `next_message`, which splits raw VISCA
  from a TCP or serial byte stream at each `FF`.
