<!-- @doc
id = "CRATE-jstream-protocol"
title = "jstream-protocol"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-protocol

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-protocol/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-protocol"]
-->

**The API.** Every capability of JStream is a command, query, event or stream defined
here first ([R-ARCH-001](../../docs/RULES.md#R-ARCH-001)), and the engine, the web UI, the
CLI and MCP agents all speak it ([ADR-0001](../../docs/adr/0001-library-first-engine.md)).
Layer 1: plain data types with serde and JSON Schema derives, no I/O, no `unsafe`, and
WASM-safe, because `jstream-ui` and `jstream-control` compile it to `wasm32`. It depends
on no other JStream crate yet: `jstream-types` has no IDs or transforms so far, so the
first cut defines its own wire types for them.

Contents (a deliberately small first cut; [Design](#design) describes the whole model):
- **Envelope** (`envelope.rs`): `ClientMessage` (`hello`, `request`) and `ServerMessage`
  (`welcome`, `response`, `error`, `event`), `Request`/`Call` and `Response`/`CallResult`,
  and `ActionResult`, the data an action may answer with (`pairing.start`'s code and
  network listener, `youtube.sign_in`'s consent URL, a YouTube broadcast).
  `ClientMessage::parse` maps every parse failure to `malformed`.
- **History** (`history.rs`, [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)):
  `HistoryCall` with its `HistoryOp` (`history.undo`, `history.redo`), the fourth call
  kind, and `HistoryDirection` (`undo`, `redo`), the `cause` of a `state.changed` and
  the `direction` of a `history_empty` error.
- **Live values** (`live.rs`, [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)):
  `LiveOutcome` (`committed`, `cancelled`, `expired`), `GestureInfo` (one open gesture
  in a `live.list` result), the limits `MAX_GESTURES_PER_CLIENT` (16) and
  `MAX_LIVE_VALUES` (256), and `is_live_adjustable`, which says which setters `live.set`
  takes (`item.set_transform`, `item.set_geometry`, `item.set_crop`, `source.set_settings`,
  `scene.set_camera`), so every
  implementation agrees.

<a id="contents-commands"></a>
## Contents: commands
<!-- @section
covers = ["crates/jstream-protocol/src/command.rs"]
-->

- **Commands** (`command.rs`): `Edit` (`scene.create`, `scene.create_variant`,
  `scene.rename`, `scene.delete`,
  `scene.reorder`, `scene.set_role`, `item.clear_override`, `item.set_effects`, `item.add`, `item.remove`, `item.reorder`,
  `item.move`, `item.set_geometry`, `item.set_transform` (deprecated), `item.set_crop`,
  `item.set_visible`, `item.set_source`, `item.set_opacity`, `item.set_blend`,
  `canvas.set_size`, `canvas.set_frame_rate`, `canvas.set_blending`, `source.create`, `source.rename`, `source.set_settings`,
  `source.duplicate`, `source.delete`, `filter.add`, `filter.remove`,
  `filter.set_enabled`, `filter.set_settings`, `filter.reorder`, `output.create`,
  `output.set_target`, `output.set_path`, `output.set_encoder`, `output.set_video_mix`,
  `output.set_audio_buses`, `output.delete`,
  `encoder.create`, `encoder.update`, `encoder.delete`, `video_mix.create`,
  `video_mix.update`, `video_mix.delete`, `source.set_audio`, `bus.create`,
  `bus.rename`, `bus.set_fader`, `bus.set_monitor`, `bus.delete`, `send.set`,
  `send.remove`, `hotkey.set`, `hotkey.set_labels`,
  `hotkey.clear`, `binding.set`, `binding.clear`, `transition.create`, `transition.update`, `transition.delete`,
  `transition.set_default`, `scene.set_transitions`, `item.set_transition`,
  `item.set_locked`, `item.set_effects_space`, `item.set_motion`, `item.set_timing`, `folder.create`, `folder.rename`, `folder.set_parent`, `folder.reorder`,
  `folder.delete`, `scene.set_folder`, `scene.set_meta`, `scene.set_options`,
  `scene.set_camera`, `scene.set_camera_motion`, `scene.set_camera_follow`,
  `scene.set_ptz_shots`, `label.create`,
  `label.update`, `label.reorder`, `label.delete`, `source.set_meta`, `timeline.set`,
  `timeline.delete`), with
  `ItemTransform` (deprecated), `Crop`, `SourceKind`, `SceneRole`, `BlendMode` and `Blending`;
  `Action` (`output.start`, `output.stop`, `output.set_key`, `output.clear_key`,
  `scene.switch` with its optional `preset_id`, `scene.switch_and_watch`, `studio.set_preview`,
  `studio.set_warm` (0.115, with `MAX_WARM_SCENES` and `DEFAULT_WARM_SCENES`),
  `scene.zoom_camera`, `ptz.drive`, `ptz.aim`, `ptz.home`, `ptz.recall`, `ptz.store`,
  `ptz.focus`, `item.show`,
  `item.hide`, `item.toggle`, `privacy.set` (deprecated),
  `audio.reset_loudness`, `preview.subscribe`, `preview.unsubscribe`, `preview.report`,
  `pairing.start`, `device.revoke`, `youtube.sign_in`, `youtube.sign_out`,
  `youtube.create_broadcast`, `youtube.bind_broadcast`, `youtube.transition_broadcast`,
  `live.set`, `live.cancel`, `plugin.install`, `plugin.grant`, `plugin.enable`,
  `plugin.clap_rescan`, `effect.enable`, `codec.download`, `codec.delete`, `pack.write`, `pack.delete`, `values.set`,
  `item.trigger_motion`, `timeline.play`, `timeline.stop`, `timeline.seek`,
  `source.restart`, `scene.set_keyframe`, `collection.open`, `collection.new`,
  `collection.save_as`), with
  `Action::named_objects`, `Action::bind_problem` and `Action::takes_steps` (whether a
  wheel chord may run it, 0.104),
  which every implementation uses to check and follow a hotkey's action, and
  `Action::cue_problem` and `Action::cue_state`, which check a timeline cue's action
  and say what state it sets ([Timelines](#timelines)); `Batch`, whose
  optional `ends_gesture` commits a gesture; and the `Command`/`CommandClass`
  classification view.

<a id="contents-state"></a>
## Contents: collection state
<!-- @section
covers = ["crates/jstream-protocol/src/filter.rs", "crates/jstream-protocol/src/output.rs", "crates/jstream-protocol/src/hotkey.rs", "crates/jstream-protocol/src/encoder.rs", "crates/jstream-protocol/src/mix.rs", "crates/jstream-protocol/src/audio.rs", "crates/jstream-protocol/src/organise.rs", "crates/jstream-protocol/src/query.rs"]
-->

- **Filters** (`filter.rs`, [ADR-0026](../../docs/adr/0026-source-filter-chains.md)):
  `FilterOwner` (tagged `type`: `source`, `master`, `output`, `scene`, `item` and
  `keyframe`),
  `FilterChain` (`video`, `audio`), `FilterInfo` and `FilterChains`, the chains
  `SourceInfo`, `OutputInfo` and `SceneInfo` report (`ItemInfo.effects` is one video
  chain), and `MAX_CHAIN_FILTERS` (8). `MasterInfo` (`query.rs`) is the
  master stage as `state.get` reports it
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)).
- **Outputs** (`output.rs`): `OutputKind` (`rtmp`, `record` and `virtual_cam` so far), `RtmpTarget`
  (a generic `url`, or a `youtube` broadcast), `RecordContainer` (only `mp4`), `OutputInfo` (with the `video_mix` it shows), and `SecretString`, the write-only stream key, which
  never prints and never comes back from the engine
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). `OutputKind::key_server` names the
  server a stream key is bound to (scheme, host and port), which the engine and the
  mock compare to remove a key that would otherwise follow its output elsewhere (#343).
- **Hotkeys** (`hotkey.rs`, [ADR-0024 §5](../../docs/adr/0024-scene-structure.md)):
  `Chord`, a key chord string with one spelling per chord (`Chord::problem`, shared by
  every implementation), which may end in the wheel (`Chord::WHEEL`, `Chord::is_wheel`,
  0.104, [Hotkeys](#hotkeys)), and `HotkeyBinding`, a chord, its action and its labels
  ([ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)) as reads report it.
- **Controller bindings** (`binding.rs`, since 0.96,
  [ADR-0122](../../docs/adr/0122-controller-input-midi-and-one-binding-system.md)):
  `Trigger`, a MIDI note, control change or program change, on one channel or any, from
  one input or any (`Trigger::problem`, shared by every implementation), and `Binding`,
  a trigger and its action as reads report it. See [Controller bindings](#bindings).
- **Encoder profiles** (`encoder.rs`): `EncoderProfile` (H.264, HEVC or AV1 and AAC, with rate
  control, keyframe interval and hardware preference), its range checks
  (`EncoderProfile::problem`, shared by every implementation), `EncoderProfileInfo`,
  and `EncoderSessionInfo`, the per-output sharing and cost `output.status` reports
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)); and
  `CodecDownloadInfo`, a codec library the engine downloads on a person's request
  ([Codec downloads](#codec-downloads)).
- **Video mixes** (`mix.rs`): `VideoMix`, what an output's video shows: a `MixInput`
  (`program`, `preview` or one `scene`), a `MixResolution` and an `fps_divisor` of the
  base frame rate, with its range checks (`VideoMix::problem`, shared by every
  implementation), and `VideoMixInfo`.
- **Audio** (`audio.rs`, [ADR-0028](../../docs/adr/0028-audio-model-v2.md)):
  `SourceAudio` and `AudioFollow`, a source's audio settings; `BusInfo` and `SendInfo`,
  the buses and the send matrix as reads report them; `BusMonitor` and `MonitorTap`, a
  bus's monitor, with its shared check (`BusMonitor::problem`, 0.42); the fader range and
  its shared check (`gain_db_problem`); the bus limit (`MAX_BUSES`), which the graph, the mock and
  the UI use and the engine's mixer is held to; and `SourceMeter`, `BusMeter` and `Loudness`, the readings
  a `meters.tick` carries.
- **Organisation** (`organise.rs`,
  [ADR-0024](../../docs/adr/0024-scene-structure.md) §3 and §4): `FolderInfo`,
  `LabelInfo`, `SceneMeta` and `SourceMeta`, and the colour and icon checks every
  implementation shares (`check_color`, `check_icon`, `SceneMeta::problem`); and
  `LabelMembers`, with `LabelledScene` and `LabelledTimeline`, what `label.members`
  lists for a label
  ([ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)).

<a id="contents-calls"></a>
## Contents: queries, kinds and events
<!-- @section
covers = ["crates/jstream-protocol/src/query.rs", "crates/jstream-protocol/src/keyframe.rs", "crates/jstream-protocol/src/youtube.rs", "crates/jstream-protocol/src/transition.rs", "crates/jstream-protocol/src/kind.rs", "crates/jstream-protocol/src/settings.rs", "crates/jstream-protocol/src/event.rs", "crates/jstream-protocol/src/plugin.rs", "crates/jstream-protocol/src/preview.rs", "crates/jstream-protocol/src/profile.rs", "crates/jstream-protocol/src/quality.rs", "crates/jstream-protocol/src/activity.rs"]
-->

- **Queries** (`query.rs`): `state.get`, `scene.list`, `source.list`, `item.list`,
  `output.list`, `output.status`, `encoder.list`, `video_mix.list`, `bus.list`,
  `video.snapshot`, `video.frames`, `kind.list`,
  `kind.describe`, `delete.plan` (with its `DeleteTarget`: a scene, a source, a bus, a
  folder or a label),
  `studio.get`, `privacy.get` (deprecated), `device.list`, `agent.approved`, `live.list`, `capture.targets`,
  `ptz.list`, `jtunnel.senders` (since 0.114, with `JtunnelSenderInfo` in `profile.rs`), `font.list`, `source.sizes`, `activity.list`, `plugin.list`, `plugin.clap_scan`, `codec.downloads`, `pack.list`,
  `pack.read`, `values.list`, `label.members` (since 0.100,
  [ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)) and `profile.get` (`profile.rs`, with `ProfileInfo` and the shapes it
  holds, `StreamProfile`, `ProfileStream`, `MixerProfile`, `ViewProfile` and, since
  0.113, `NetworkProfile` and `NetworkInputProfile` among them),
  `quality.probe` (since 0.110, `quality.rs`: `ProbeStream`, `QualityProbeInfo` with its
  `FrameQuality`, `probe_frames`, `DEFAULT_PROBE_FRAMES`, `MAX_PROBE_FRAMES` and
  `MAX_PSNR_DB`), with `QueryResult`
  and the `SceneSummary`, `SceneInfo`, `SourceInfo`, `ItemInfo`, `DeviceInfo`,
  `CaptureTargetInfo`, `FontInfo`, `SourceSize` and `ActivityEntry` shapes the reads report
  (`activity.rs` holds the last, with `MAX_ACTIVITY_ENTRIES` and
  `MAX_ACTIVITY_SUMMARY_CHARS`). `state.get` carries the `Canvas` and the `Blending` (since 0.101), and
  `ItemInfo`
  the item's `placement` form and its `geometry`, `SceneInfo` a scene's transition
  overrides and its `camera` (since 0.70; on a keyframe scene its own override) with its
  `camera_motion` and `camera_follow` (since 0.85, ADR-0113, each omitted while absent;
  since 0.91 `camera_follow` is read from its items' `follow_pointer` filters, ADR-0120)
  and its `timelines` (since 0.87, ADR-0115, omitted while empty), and `studio.get`
  the timelines playing (`PlayingTimeline`) and since 0.88 the keyframes placed scenes
  are drawn through (`PlacedKeyframe`, ADR-0116) and since 0.115 the warm budget
  (`warm_scenes`), `ItemInfo` its `transition` and `state.get` the presets. `video.snapshot`
  returns the program mix as a PNG, and `video.frames` a frame sheet ([Frame
  sheets](#design)). `youtube.status`, `youtube.list_broadcasts` and
  `youtube.list_streams` read the YouTube account, and `youtube.stats` the running
  `youtube` outputs' audience and the day's quota estimate.
- **Frame sheets** (`frames.rs`, #1061): `FrameStrip` (a sheet's `count`, `interval_ms`
  and `delay_ms`, its limits `MAX_SHEET_FRAMES`, `MIN_SHEET_INTERVAL_MS` and
  `MAX_SHEET_SPAN_MS`, and the `check` every implementation shares), and `FrameSheet`
  and `SheetFrame`, which `video.frames` and `scene.switch_and_watch` answer with
  ([Frame sheets](#design)).
- **Keyframe scenes** (`keyframe.rs`, [ADR-0024](../../docs/adr/0024-scene-structure.md)
  §2, [ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)):
  `ItemOverride`, what a keyframe scene overrides for one item as `SceneInfo.overrides`
  reports it (its `effects` since 0.63, its `motion` since 0.81, its `timing` since 0.99), `OverrideField`, the field `item.clear_override` removes, and
  `DroppedOverride`, an override a batch dropped, and `PlacedKeyframe` (since 0.88),
  a placed scene's switched keyframe ([Keyframe tracks](#keyframe-tracks)).
- **Motion** (`motion.rs`, since 0.81,
  [ADR-0099](../../docs/adr/0099-native-motion-modifiers-and-timelines.md)):
  `MotionModifier` (an ID, the `MotionProperty` it moves, its `MotionKind`, its
  `MotionClock` and `enabled`), the limits `MAX_MOTION_MODIFIERS` (8) and
  `MAX_MOTION_ID_LEN` (64), `check_motion`, the list check every implementation
  shares, and `check_motion_id`, the ID check `item.trigger_motion` shares
  ([Motion](#motion)). Since 0.86 `MotionKind::Audio`, with its `MotionMeasure`
  and `MotionCurve`, follows a bus or a source
  ([ADR-0100](../../docs/adr/0100-audio-reactive-motion-modifiers.md)). Since 0.109
  `MotionProperty::Effect` and `MotionEffect`, the effect setting such a modifier drives
  ([ADR-0138](../../docs/adr/0138-motion-drives-effect-settings.md)).
- **Camera motion and follow** (`camera_motion.rs`, since 0.85,
  [ADR-0113](../../docs/adr/0113-camera-motion-and-follow-pointer.md)):
  `CameraMotionModifier` (an item's modifier with a `CameraProperty`: the position's and
  the target's x, y and z, `fov`, `roll`, `focus` and `blur`) and `check_camera_motion`,
  which checks a camera's list as `check_motion` checks an item's and refuses an
  `audio` modifier (`motion.N.kind`), since a camera hears nothing (ADR-0100); `CameraFollow`, a
  camera that follows the pointer over a display or window capture (its item, `zoom`,
  `yaw_deg`, `pitch_deg`, `lean_deg`, `smooth_ms` and, since 0.91, `safe_margin_pct`
  up to `MAX_FOLLOW_MARGIN_PCT`, each with its default) with `CameraFollow::check`, and
  `FOLLOW_ZOOM_RANGE` (1 to 4) and `FOLLOW_ZOOM_STEP` (1.25), what `scene.zoom_camera`
  steps by. Since 0.91 a follow camera is a filter (ADR-0120): `FOLLOW_POINTER_KIND`
  (`follow_pointer`), its `follow_pointer_descriptor`, and `CameraFollow::settings` and
  `CameraFollow::from_settings`, which map a follow to that filter's settings and back.
- **YouTube** (`youtube.rs`, [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)):
  `YoutubeAuth`, `BroadcastInfo`, `StreamInfo` and the enums they carry, mirroring the
  Live API with an `other` value for anything newer, and `AudienceStatsInfo`,
  `QuotaInfo` and `StatsPolling` (0.38).
- **Transitions** (`transition.rs`, [ADR-0029](../../docs/adr/0029-transitions.md),
  [ADR-0037](../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)): the
  built-in transition kinds (`cut`, `fade`, `move`, `stinger`) and their
  descriptors, `Easing` with
  `Easing::apply`, the one easing function every implementation shares (golden-tested),
  `ItemTransition` (an item's delay, easing, glide and appear and disappear effects) with
  its range check, `TransitionPresetInfo` and `TransitionsInfo` as `state.get` reports
  them, and `TransitionStarted`, what a `studio.changed` says a switch ran.
- **Kinds and settings** (`kind.rs`, `settings.rs`,
  [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)): `KindDescriptor`,
  `ParamDescriptor` and `ParamType`, the shapes that describe a source, filter or
  transition kind's parameters, and `Settings`, a map from parameter ID to a JSON value, whose `scene`,
  `source`, `bus` and `asset` references are tagged objects (`Reference`). The per-value checks
  (`ParamDescriptor::check`, `check_asset_path`, and `not_a_reference`, the reason a value
  isn't a reference, which sources reuse for their own checks) and the settings schema
  (`KindDescriptor::settings_schema`) are pure functions here, so every implementation
  shares them. No source or filter kind is defined here; the transition kinds are
  (Transitions above).
- **Events** (`event.rs`): `state.changed`, `output.state_changed`, `studio.changed`
  (program, preview and the transition a switch started), `privacy.changed` (deprecated),
  `source.size_changed`, `events.dropped`,
  `stats.tick` (with `DropCounts`, `EngineLoad`, `ChainCost` and each running output's
  `OutputStatsInfo` and `OutputHealth`), `meters.tick`,
  `youtube.status_changed`, `youtube.health_changed`,
  `youtube.stats`, `live.changed`, `live.ended`, `plugin.state_changed`,
  `gpu.device_lost`, `gpu.effect_slow` (0.102), `values.changed`, and `timeline.changed` (with `TimelineChange`)
  and `timeline.cue_fired` (0.87), `scene.keyframe_changed` (0.88), and
  `collection.file_changed` (with `CollectionCause`, 0.94), `kinds.changed` (with
  `KindsCause`, 0.107), `agent.approval_requested` and `agent.approval_resolved`
  (0.108), and `jtunnel.senders_changed` (0.114).
- **Plugins** (`plugin.rs`, [ADR-0069](../../docs/adr/0069-wasm-plugin-limits-and-capabilities.md)): `PluginClass` (`read`,
  `edit`, `live`, `outputs`), `PLUGIN_SCOPES`, the table that places every command in
  one class or under "never", with `plugin_scope` and `plugin_classes` (a call's
  classes, adding a `hotkey.set`'s bound action and a `live.set`'s edits, or the
  `PluginDenial` no grant overrides), `PluginGrant` (the classes, hosts and folders a
  person grants, with `check`, which returns the `forbidden` error, and `problem`),
  `settings_reach` and `Reach` (the hosts and files a settings map names),
  `plugin_may_bind`, `PluginState`, `PluginDisabledReason`, and `PluginInfo` and
  `PluginRequest`, what `plugin.list` reports ([Plugins](#plugins)). And (`clap.rs`)
  `ClapScanInfo` with its `ClapFolder`, `ClapFound` and `ClapFailed`, what
  `plugin.clap_scan` reports of the CLAP plugin scan ([Plugins](#plugins)).
- **Preview streams** (`preview.rs`, [ADR-0045](../../docs/adr/0045-leptos-web-ui.md)):
  `PreviewCodec` (`h264`, `jpeg`, `png`), `PreviewSettings` (`preview.subscribe`'s fields with
  their defaults and range checks, which the engine applies), `PreviewDraft` (its
  `source`, a draft source the collection doesn't hold), and the binary
  frame: `BinaryFrame`, its fixed `BinaryHeader` with `StreamKind`, the byte layout's
  `to_bytes`/`parse` and `BinaryError`, and `KeyframeGate`, the drop rule every hop that
  queues frames applies ([Binary streams](#binary-streams)); and `ViewCounts`, what a
  client's view did with its pictures, with `check_view_label`, `MAX_VIEW_LABEL_CHARS`
  and `MAX_SESSION_VIEWS` (`preview.report`, 0.103).

<a id="contents-errors"></a>
## Contents: errors, IDs and numbers
<!-- @section
covers = ["crates/jstream-protocol/src/error.rs", "crates/jstream-protocol/src/id.rs", "crates/jstream-protocol/src/version.rs", "crates/jstream-protocol/src/finite.rs"]
-->

- **Errors** (`error.rs`): `ProtocolError`, a `thiserror` enum whose variants have stable
  codes, `Blocker`, the `blockers` detail of a refused delete, and `WireError`, its wire
  form with a message.
- **IDs and versions** (`id.rs`, `version.rs`): opaque string IDs (`BusId::master()` is
  the one fixed ID, `master`; a `GestureId` and a `TimelineId` are chosen by the client), `Rev`, and
  `PROTOCOL_VERSION`.
- **Numbers** (`finite.rs`): `Finite32`, the only float type on the wire, which rejects
  NaN, infinity and out-of-range numbers at construction and deserialization. Computed
  results (`CanvasPoint`, `ContentRect`, `LegacyQuad`) use plain `f32`.

<a id="contents-geometry"></a>
## Contents: geometry
<!-- @section
covers = ["crates/jstream-protocol/src/geometry.rs", "crates/jstream-protocol/src/geometry/**"]
-->

- **Geometry** (`geometry.rs`): [`ItemGeometry`](src/geometry.rs), `Canvas`,
  `PlacementForm` and the shared placement formula
  ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)). `project` places the
  box, rotation about its centre, with the 3D fields in the formula and defaulting to
  neutral. `content` maps a cropped source with `stretch`, `fit` or `fill`, and
  `flip_h` and `flip_v` mirror it in the box. `drawn_polygon` and `unplace` are
  ADR-0050's 3D polygon and inverse map, and `check_drawable` whether an edit may store
  a geometry ([Item geometry](#geometry)). `geometry/camera.rs` holds the scene camera
  ([ADR-0094](../../docs/adr/0094-scene-cameras.md)): `Vec3`, `SceneCamera` with
  `framing(canvas)`, the camera that puts every flat item on its 2D box, and `validate`,
  and `CameraView`, the camera seen on one canvas, whose `blur_at` is the camera's
  depth-of-field blur at a distance along its view
  ([ADR-0108](../../docs/adr/0108-camera-depth-of-field.md)) and whose `in_view` and
  `direction_in_view` take scene points and directions into a model's viewing frame.
  `ItemGeometry::light_pose` is where a light its item places sits and shines: the
  box's centre at its `depth_px`, and its back normal.
  `drawn_polygon_seen`, `unplace_seen`,
  `drawn_vertex_seen` and `project_seen` take an optional view, and with one place the
  item by its geometry in the scene's space through the camera (`w = D / f`, the same
  near plane); with none they are the functions above.
  `geometry/solid.rs` places a 3D model by an item
  ([ADR-0107](../../docs/adr/0107-3d-models.md)): `ItemGeometry::solid(bounds, view)`
  scales the model's `SolidBounds` into the box by its fit mode, flips, turns and moves
  it, and returns a `Solid`: the map from the model's space to the viewing frame, in
  `f64`, relative to the camera. Its `place` and `corners` are what an editor outlines and
  hits, and `clip_matrix`, `view_matrix` and `normal_matrix` what the compositor draws
  with, cast to `f32` only once composed.
  `transform_view` is the deprecated 0.3 view of a geometry that `ItemInfo.transform`
  reports. `legacy_quad` and `ItemGeometry::from_legacy` evaluate a 0.3 transform. A
  negative scale is not refused: `from_legacy` returns the drawn outline with the
  reflection as the geometry's flip. `ItemTransform::check_drawable` gives bounds
  within which any source draws a legacy transform: its position within `MAX_DRAWN_PX`
  and its scale within ±`MAX_LEGACY_SCALE` (100 000, #1130). They are enough, not
  needed, and no edit checks them (ADR-0022); motion keeps a moved transform within
  them.

<a id="versions"></a>
## Protocol versions
<!-- @section
covers = ["crates/jstream-protocol/src/version.rs", "crates/jstream-protocol/src/pack.rs"]
-->

`PROTOCOL_VERSION` is 0.115 because the studio's warm budget is a setting: the action
`studio.set_warm` and `studio.get`'s `warm_scenes` (ADR-0117 §7, #1468); 0.114 because
the JTunnel receiver finds senders on the LAN:
the query `jtunnel.senders` (`JtunnelSenderInfo`) and the event
`jtunnel.senders_changed` (ADR-0123 §3, #1223, a number the release queue set); 0.113
because `profile.get` reports the JTunnel receiver's
senders: `network` (`NetworkProfile`, `NetworkInputProfile`, #1223, a number the release
queue set); 0.112 because the engine keeps a MIDI clock's tempo as the
shared values `tempo.bpm`, `tempo.playing`, `tempo.beat` and `tempo.bar`, which it sets
itself with `values.changed` from the client `engine`, and a `browser` source's page may
be granted those values, `page_api_tempo`, and one MIDI input's state, `page_api_midi`
(ADR-0147, a number the release queue set); 0.111 (a number the release queue set)
because the engine drives PTZ cameras: the actions `ptz.drive`, `ptz.aim`,
`ptz.home`, `ptz.recall`, `ptz.store` and `ptz.focus`, the query `ptz.list`, and a
scene's `ptz_shots`, set with `scene.set_ptz_shots` ([PTZ cameras](#ptz)); 0.110 because the query `quality.probe` (`ProbeStream`,
`QualityProbeInfo`) measures a running output's encoded picture
([Queries](#queries), [ADR-0145](../../docs/adr/0145-the-quality-probe-reads-back-the-compositor.md),
a number the release queue set); 0.109 because a motion modifier can drive a number or colour
setting of one of its item's effects (#1404): `MotionProperty::Effect` and `MotionModifier.effect`
(`MotionEffect`, [ADR-0138](../../docs/adr/0138-motion-drives-effect-settings.md),
#1294, a number the release queue set); 0.108 because the person is asked before a new agent connects:
the events `agent.approval_requested` and `agent.approval_resolved`, the actions
`agent.approve` and `agent.forget`, the query `agent.approved`, the error `not_allowed`
and the entity kind `approval`
([Agent approval](#agent-approval), [ADR-0131](../../docs/adr/0131-ask-before-a-new-agent-connects.md),
a number the release queue set); 0.107 because the event `kinds.changed` (`KindsCause`) tells
every client when a CLAP scan or the effect packs change the kinds
([Events](#events), #1328, a number the release queue set); 0.106 because a person can see and redo the CLAP plugin scan: the
query `plugin.clap_scan` (`ClapScanInfo`) and the action `plugin.clap_rescan`
([Plugins](#plugins), a number the release queue set); 0.105 because a webcam's camera controls are settable:
`capture.targets` lists each device's `controls` (`CameraControlInfo`), and the
`camera_controls` parameter type sets them on the device that its `target` parameter
names (#1196, a number the release queue set); 0.104 because a chord may end in the wheel, `WheelUp` or
`WheelDown`, bound only to an action that takes steps (`Chord::WHEEL`,
`Action::takes_steps`, [ADR-0112](../../docs/adr/0112-assignable-camera-zoom-bindings.md)
§3, a number the release queue set); 0.103 because a client's views report back: the action
`preview.report` (`ViewCounts`) and `profile.get`'s `views` (`ViewProfile`)
([ADR-0132](../../docs/adr/0132-bench-gates-views-and-quality-probe.md), a number the
release queue set); 0.102 because the engine quarantines an effect-pack kind that runs
too long on the GPU, before the OS resets the device, and says so in `gpu.effect_slow`
(#349, [Effect quarantine](#quarantine), a number the release queue set); 0.101 because a
collection has one blending space, `canvas.set_blending` and `state.get`'s `blending`,
and `BlendMode` is back to nine modes, the two `_srgb` names still read as `normal`
and `additive` ([ADR-0126](../../docs/adr/0126-one-set-of-blend-modes-and-a-blending-space.md),
a number the release queue set); 0.100 because hotkey bindings and timelines carry labels
(`hotkey.set_labels`, `HotkeyBinding.labels`, `TimelineInfo.labels`) and `label.members`
lists everything that carries a label
([ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md), a number the
release queue set); 0.99 because a keyframe scene can time an item in switches into it
(`item.set_timing`, `ItemOverride.timing`, `KeyframeTiming`, `OverrideField::Timing`) and
the easings gain `ease_in`, `hold` and `cubic_bezier`
([ADR-0129](../../docs/adr/0129-keyframe-timing-and-more-easings.md), a number the
release queue set); 0.98 because `scene.set_camera` accepts a camera on a keyframe
of a 2D scene, which it refused before, and a Move between a 2D side and a 3D one glides
([ADR-0128](../../docs/adr/0128-moves-between-2d-and-3d-glide.md), a number the
release queue set); 0.97 because `pairing.start`'s answer can carry `network`, the
network listener's addresses and certificate fingerprint
([ADR-0130](../../docs/adr/0130-browsers-keep-no-certificate-pin-v2.md), a number the
release queue set); 0.96 because of controller bindings: `binding.set`,
`binding.clear`, `Trigger`, `Binding`, `state.get`'s `bindings`, `delete.plan`'s
`bindings`, the batch result's `removed_bindings` and the `binding` entity kind
([ADR-0122](../../docs/adr/0122-controller-input-midi-and-one-binding-system.md), a
number the release queue set); 0.95 because `preview.subscribe` takes an `fps` up to 60, so a
live view can show a 60 fps program at its full rate (30 before; a number the release
queue set); 0.94 because of projects: the actions `collection.open`,
`collection.new` and `collection.save_as`, the query `collection.info`, the event
`collection.file_changed` and the `file` entity kind
([ADR-0121](../../docs/adr/0121-projects-open-new-and-save-as.md), a number the release
queue set); 0.93 because a parameter's descriptor can carry a `warning`, one
line a client shows beside the control
([ADR-0124](../../docs/adr/0124-page-api-read-review.md), a number the release queue
set); 0.92 because `profile.get` reports past the drawers: each
encoded stream's frames (`streams`, `StreamProfile`, `ProfileStream`) and the audio
mixer's blocks (`mixer`, `MixerProfile`) (#1220); 0.91 because a follow camera is a `follow_pointer` filter on a
capture item, `scene.set_camera_follow` a shortcut for its filter edits, and
`CameraFollow` gains `safe_margin_pct`
([ADR-0120](../../docs/adr/0120-follow-pointer-is-a-filter.md), a placeholder number the
release queue sets); 0.90 because `capture.targets` lists video capture devices
(`CaptureTargetKind::Device`, with each device's `formats` as `CaptureFormatInfo`), a
`webcam` source's `device` is a `capture_target` of kind `device`, and the
`capture_format` parameter type (`CaptureFormatPart`) picks a device's resolution, frame
rate and pixel format; 0.89 because a collection can set its base frame rate with
`canvas.set_frame_rate` (30 by default, or 60; [ADR-0119](../../docs/adr/0119-base-frame-rate-option.md));
0.88 because of keyframe tracks: the action `scene.set_keyframe`,
`studio.get`'s `keyframes` (`PlacedKeyframe`) and the event `scene.keyframe_changed`
([ADR-0116](../../docs/adr/0116-keyframe-tracks.md), a placeholder number the release
queue sets); 0.87 because of timelines: `timeline.set`, `timeline.delete`,
`timeline.play`, `timeline.stop`, `timeline.seek`, `source.restart`,
`SceneInfo.timelines`, `studio.get`'s `timelines`, the `timeline.changed` and
`timeline.cue_fired` events and the `timeline` entity kind
([ADR-0115](../../docs/adr/0115-timelines-on-the-wire.md), a placeholder number the
release queue sets); 0.86 because of the `audio` motion kind and its `MotionMeasure`
and `MotionCurve` ([ADR-0100](../../docs/adr/0100-audio-reactive-motion-modifiers.md));
0.85 because of
`scene.set_camera_motion`, `scene.set_camera_follow`, the action `scene.zoom_camera`,
`SceneInfo.camera_motion` and `SceneInfo.camera_follow`
([ADR-0113](../../docs/adr/0113-camera-motion-and-follow-pointer.md)); 0.84 because of `item.trigger_motion`, the `impulse` motion kind
and the `triggered` clock (#1107); 0.83 because `item.set_opacity`,
`item.add` and a keyframe override take an opacity above 1, to `MAX_OPACITY` (#1101);
0.82 because of the frame
sheets, `video.frames` and `scene.switch_and_watch` (#1061); 0.81 because of
`item.set_motion`, `ItemInfo.motion`, `ItemOverride.motion` and `OverrideField`'s
`motion` ([ADR-0099](../../docs/adr/0099-native-motion-modifiers-and-timelines.md));
0.80 because of the
`item.show`, `item.hide` and `item.toggle` actions
([ADR-0101](../../docs/adr/0101-item-show-hide-actions.md)); 0.79 because a master or output filter may name a file (an
asset reference, so Apply LUT grades the program or one output; no wire change:
earlier minors refused it with `invalid_argument` on the parameter); 0.78 because of
the shared values `values.set`, `values.list`
and `values.changed` (`values.rs`,
[ADR-0102](../../docs/adr/0102-page-api-for-browser-sources.md)); 0.77 because of
`SceneCamera`'s `focus_px` and `blur_px`, a camera's depth of field
([ADR-0108](../../docs/adr/0108-camera-depth-of-field.md)); 0.76
because of `item.set_effects_space` and `ItemInfo.effects_space`
([ADR-0106](../../docs/adr/0106-item-effects-in-canvas-space.md)); 0.75 because of the
`normal_srgb` and `additive_srgb` blend modes (ADR-0105); 0.74 because of
`output.set_path` (#1072); 0.73 because of the `profile.get` query
([ADR-0103](../../docs/adr/0103-per-item-performance-profile.md)); 0.72 because of the
effect-pack commands `pack.write`, `pack.delete`, `pack.list` and `pack.read`
(`pack.rs`: `PackFile`, `PackContent`, `PackInfo`, `PackFileInfo` and the write's
limits, [ADR-0097](../../docs/adr/0097-agents-write-effect-packs.md)), which agents
may call and no plugin is granted but `pack.list` and `pack.read`, and no hotkey
binds; 0.71 because of `item.set_locked`, `ItemInfo.locked` and the `png` preview
codec ([ADR-0096](../../docs/adr/0096-item-lock-and-png-preview-frames.md)); 0.70 because of `scene.set_camera` and
`SceneInfo.camera`, a live value too
([ADR-0094](../../docs/adr/0094-scene-cameras.md)); 0.69 because of the
`codec.downloads` query and the `codec.download` and `codec.delete` actions, which
download Cisco's OpenH264 on a person's request ([Codec downloads](#codec-downloads),
[ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)); 0.68 because of the
`font.list` query ([ADR-0092](../../docs/adr/0092-installed-fonts-query.md)); 0.67 because `live.set`
takes a source's settings whose kind marks them `live` (`source.set_settings`,
[ADR-0091](../../docs/adr/0091-source-settings-as-live-values.md)); 0.66 because
`stats.tick`'s `chains` counts the output stage's video chains too (`ChainCost`'s
`stage_chains`, `stage_layers` and `stage_passes`, #980,
[ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)); 0.65 added
`scene.set_options` and `SceneInfo`'s flattened `SceneOptions`, whose `include_audio`
leaves a scene's audio out of every nesting of it and whose `background` is the
colour a scene is drawn over
([ADR-0089](../../docs/adr/0089-scene-options-and-nested-audio.md)); 0.64 let the
engine's `compressor` audio filter kind take a sidechain `key`, a `source` reference it ducks
under (ADR-0028; no wire change: `kind.describe` reports the new parameter and the
engine accepts it); 0.63 made a keyframe
scene override an item's effects
([ADR-0088](../../docs/adr/0088-keyframe-scenes-override-item-effects.md):
`ItemOverride.effects`, `OverrideField`'s `effects`, `item.set_effects` and the
`keyframe` filter owner); 0.62 added the
`too_large` error and `Welcome.max_request_bytes`, a message over the server's request
limit refused with the connection kept open
([ADR-0074](../../docs/adr/0074-too-large-requests.md)); 0.61 blamed a GPU device
loss on the effect-pack kinds that drew before it, which are quarantined
until a person re-enables them (#349: the `gpu.device_lost` event, `KindSummary`'s
`quarantined` and the `effect.enable` action, [Effect quarantine](#quarantine)); 0.60
made the master stage and every output take video filters on their `video` chain
(ADR-0080; no wire change: earlier minors refused them with `invalid_argument` on
`chain`); 0.59 made outputs carry
audio buses: `OutputInfo.audio_buses`, the `output.set_audio_buses` edit, and an
output as a `bus.delete` blocker ([Audio](#audio)); 0.58 added the
`plugin.list` query, the installed WASM plugins with what each manifest asks for and
what a person granted ([Plugins](#plugins)); 0.57 added
`stats.tick`'s `outputs`, each running output's live stats (`OutputStatsInfo`); 0.56
added `preview.subscribe`'s `source_id`, a stream of one of the collection's sources alone
(a projector of the source); 0.55 made a keyframe
scene override the children of the groups in its parent's tree (#846; no wire
change); 0.54 added `stats.tick`'s `chains`, what scene filter chains and item effects
add to each frame (`ChainCost`); 0.53 made the engine take live values on a keyframe scene (`live.set`,
#846; no wire change); 0.52 added
`ui_settings.set` and `ui_settings.get`, the UI settings the server keeps
([ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md)); 0.51 added the
`activity.list` query, the engine's recent activity with a sentence for each call
([ADR-0083](../../docs/adr/0083-activity-log-over-the-protocol.md)); 0.50 added the
`capture_target` parameter type, which a `display` or `window` source's `target` has, so
a UI can offer what `capture.targets` lists ([Kinds](#kinds)); 0.49 made
`preview.subscribe` take a `source`, a draft source the collection doesn't hold
(`PreviewDraft`, [Binary streams](#binary-streams)); 0.48 added `stats.tick`'s `load`, the
engine's frame and CPU totals (`EngineLoad`); 0.47 made the engine draw, and so
accept, every blend mode
([ADR-0082](../../docs/adr/0082-blend-modes-drawn.md); no wire change); 0.46 added
`preview.subscribe`'s `scene_id`, a preview stream of any scene, and its 16 streams a
session ([Binary streams](#binary-streams)); 0.45 made scenes
carry labels (`SceneMeta`'s `labels`, set by `scene.set_meta`;
[ADR-0078](../../docs/adr/0078-scenes-carry-labels.md)); 0.44 added the
`hardware_session_limit` error (`output.start` refused a new hardware video session
while other outputs held hardware sessions, ADR-0080 §3); 0.43 added scene filters
and item effects ([ADR-0065](../../docs/adr/0065-scene-chains-and-item-effects.md):
`FilterOwner`'s `scene` and `item`, `SceneInfo.filters`, `ItemInfo.effects` and
`KindDescriptor.max_outset_px`); 0.42 added bus monitors (`bus.set_monitor`,
`BusInfo`'s `monitor`, `BusMonitor` and `MonitorTap`); 0.41 added the `VideoCodec`
values `hevc` and `av1`, which an encoder profile now accepts; 0.40 added runtime
source sizes (the `source.sizes` query and the `source.size_changed` event,
`SourceSize`); 0.39 added keyframe
scenes (`scene.create_variant`, `item.clear_override`, `SceneInfo`'s `variant_of` and
`overrides`, the batch result's `dropped_overrides`, and placement edits whose
`scene_id` names a keyframe scene); 0.38 added YouTube audience stats and the quota
estimate (the `youtube.stats` event and query, `AudienceStatsInfo`, `QuotaInfo`,
`StatsPolling`); 0.37 added
`output.status`'s `cleartext` (#562); 0.36 added
the `show_cursor` setting of the `display` and `window` kinds (whether the pointer
shows in the capture; on by default, which is what those kinds did before); 0.35
added WASM plugins ([Plugins](#plugins): `plugin.install`, `plugin.grant`, `plugin.enable`,
`plugin.state_changed`, the `forbidden` error and the `plugin` entity kind); 0.34 made
the engine accept non-neutral 3D (`tilt_deg`, `turn_deg`, `depth_px` and
`perspective_px`, which earlier minors rejected with `invalid_argument`, are stored and
drawn (ADR-0050) when the compositor can draw them (`check_drawable`); the wire form is
unchanged, so a client that sends only neutral 3D sees no difference); 0.33 added the
privacy screen (`privacy.set`, `privacy.get`, `privacy.changed`, since deprecated by
[ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)) and the `dedicated`
flag on an output's encoder session; 0.32 added the geometry flips (`ItemGeometry`'s
`flip_h` and `flip_v`); 0.31 added folders, labels and organisation metadata (the `folder.*` and `label.*` edits,
`scene.set_folder`, `scene.set_meta`, `source.set_meta`, `state.get`'s `folders` and
`labels`, `SceneInfo`'s `folder` and `meta`, `SourceInfo`'s `meta`, the `folder` and
`label` `delete.plan` targets, entity kinds and blockers); 0.30 added
transition presets and studio mode's preview (the `transition.*` edits,
`scene.set_transitions`, `item.set_transition`, `scene.switch`'s `preset_id`,
`studio.set_preview`, `studio.changed`'s `preview_scene` and `transition`, and the
`transition` entity kind and kind category); 0.29 added the master stage's and
outputs' filter chains (the `master` and `output` filter owners,
`OutputInfo.filters` and `state.get`'s `master`); 0.28 added video mixes
(the `video_mix.*` edits, `output.set_video_mix`, `video_mix.list` and the mixes in
`state.get` and `output.list`); 0.27 added the `capture.targets` query;
0.26 added live values ([Live values](#live-values): `live.set`, `live.cancel`, `live.list`, the
`live.changed` and `live.ended` events, and `Batch.ends_gesture`); 0.25 added
hotkeys (`hotkey.set`, `hotkey.clear`, `state.get`'s `hotkeys`, `delete.plan`'s
`hotkeys` and the batch result's `removed_hotkeys`); 0.24 added bus
references (the `bus` parameter type and reference, which the `waveform` and
`spectrum` kinds' `audio` setting uses, a `bus` `delete.plan` target, and
`bus.delete` refused while one refers to its bus), 0.23 YouTube
([YouTube](#youtube): the `youtube.*` commands and events, the `youtube`
RTMP target, and the YouTube actions' `ActionResult`s,
[ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)), 0.22
opacity, blend and filter chains (`item.set_opacity`, `item.set_blend`, the
`filter.*` edits, `SourceInfo.filters` and the `filter` entity kind,
[ADR-0023](../../docs/adr/0023-placements-references-and-clones.md),
[ADR-0026](../../docs/adr/0026-source-filter-chains.md)), 0.21 pairing
(`pairing.start`, `device.list`, `device.revoke`, `ActionResult`, the
`unsupported` error and the `device` entity kind,
[ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md)), 0.20
the preview streams (`preview.subscribe`, `preview.unsubscribe` and the binary
frame), 0.19 audio (sources' audio, buses, sends, `bus.list`, `meters.tick` and
`audio.reset_loudness`), 0.18 the `virtual_cam` output kind, 0.17
`scene.create`'s optional `role`, 0.16 `scene.switch`, `studio.get` and
`studio.changed`, 0.15 `delete.plan` and the
`blockers` detail on `invalid_argument`, 0.14 references
and clones (`item.set_source`, `duplicated_from`), 0.13 encoder
profiles and session sharing, 0.12 groups (`scene.set_role`,
`item.move`, a scene's `role` and the placement edits' `scene_id`), 0.11 the canvas
and the geometry wire surface ([Item geometry](#geometry)), 0.10 the `history` calls,
0.9 the `record` output kind, 0.8 the kind queries and settings, 0.7 the output
edits, `output.list`, the key actions and `key_set`, 0.6 `stats.tick` and 0.5
`video.snapshot`.

A bump rewrites the paragraph's opening line, `` `PROTOCOL_VERSION` is 0.N because `` and
its own reason, and keeps the reason before it as the next clause. The test
`the_readme_opens_on_the_current_version` holds that line to the number, so two PRs that
take the same number conflict in git and the second to land takes the next one
([waves › Protocol versions](../../docs/runbooks/waves.md#protocol)).

<a id="contents-schema"></a>
## Contents: schema and parity fixture
<!-- @section
covers = ["crates/jstream-protocol/src/schema.rs", "crates/jstream-protocol/src/parity.rs", "crates/jstream-protocol/src/fixtures.rs"]
-->

- **Schema** (`schema.rs`): `json_schema()`, snapshot-tested in
  [`schema/protocol.snap`](schema/protocol.snap), with every fixture message's wire form in
  [`schema/examples.snap`](schema/examples.snap).
- **Parity fixture** (`parity.rs`, only with the non-default `fixtures` feature): test
  support, not wire types. One batch that builds a small collection (a non-default
  canvas, legacy items, one geometry item (flipped left to right), two RTMP outputs, a virtual camera, a recording output, two
  encoder profiles, two video mixes, two hotkeys and two controller bindings, one of each rebound), the exact answer to every collection read of it (`output.list`
  included), and the exact error for batches rejected against it (geometry, canvas, RTMP
  URL, recording path, encoder profile, video mix, hotkey and binding checks, a hotkey
binding's labels, `label.members` of a missing label, and `item.set_source` and
  `duplicated_from`, opacity, blend and filter checks, included). The collection also
  holds a clone made with `source.duplicate` and `item.set_source`, a
  `duplicated_from` naming a missing source, items at opacity 0.25 and 0.5, a
  desktop audio source with an audio chain of two gain filters, one disabled, a gain
  filter each on the master stage's audio chain and on the recording output's, and
  audio: a microphone with its fader, mute and sends, two buses (one deleted again with
  its send), a renamed and faded bus, a duplicated microphone with a send removed, and
  a disabled nested scene. The rejected batches also cover an audio filter on the
  virtual camera, a reference in a master filter's settings (refused before its
  index), the bus limit, the master bus, fader ranges, and sources with no audio (a
  display capture, `screen`, never placed) or no fader of their own.
  `jstream-graph` and `jstream-control`'s mock engine are both layer 2, so neither can
  test against the other ([R-ARCH-002](../../docs/RULES.md#R-ARCH-002)); both enable
  the feature in dev-dependencies only and are held to these answers. `kinds()` names
  the kinds an implementation must describe for the batch to apply: `webcam`, with
  video and audio (its paired input, #716), and `display`, with video and no audio,
  each described as the engine's built-in kind is, whose one parameter (`device`,
  `target`) the batch always sends; `desktop_audio` and `mic`, with audio and no video
  and no parameters; and the audio filter kind `gain`, whose one parameter the batch
  always sets. So every implementation reports the same settings and audio defaults.
  It changes no
  message, so it never changes `PROTOCOL_VERSION`.

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-protocol/src/lib.rs", "crates/jstream-protocol/src/envelope.rs", "crates/jstream-protocol/src/version.rs"]
depends_on = ["docs/RULES.md#R-API-001", "docs/RULES.md#R-API-002"]
-->

This is the protocol's design. It is a proposal, so the maintainer can steer it: the open
points are listed under [Open questions](#open-questions).

### Envelope and versioning

A connection carries JSON text messages, each tagged by `type`. It starts with a
handshake: the client sends `hello` with its `ProtocolVersion` and a `client_name`, and
the server answers `welcome` with its own version, the `client_id` this connection acts
as, and the current `rev`. If the server can't serve that version it answers an
`unsupported_version` error instead. A request sent before the handshake gets
`handshake_required`.

After the handshake, the client sends `request`s. Each carries an `id` the client
chooses, and the server answers each request with exactly one `response` or `error`
carrying the same `id`. Answers may come out of order. `event`s arrive at any time.

**Message size** ([ADR-0074](../../docs/adr/0074-too-large-requests.md)). A server in
front of the engine handles client messages of up to its request limit, which `welcome`
advertises as `max_request_bytes` (0.62; missing from an older server, which means
4 MiB). A message over it, up to the server's higher frame limit, is refused with
`too_large` (`size_bytes`, `limit_bytes`), answering the request's `id` when the server
can read one, a string of at most 256 bytes (a longer one is answered with no `id`, as
is a `malformed` message's): nothing in it is applied, and the connection stays open. A message over
the frame limit closes the connection with close code 1009. A client splits work that
doesn't fit, a batch of `state` writes one filter per batch, and reads server messages
of at least the frame limit.

```json
{"type":"hello","protocol":{"major":0,"minor":37},"client_name":"jstream-cli"}
{"type":"welcome","protocol":{"major":0,"minor":37},"client_id":"cli_7","rev":12}
{"type":"request","id":"r1","call":{"kind":"batch","edits":[{"type":"scene.create","scene_id":"scn_1","name":"Main"}],"expected_rev":12}}
{"type":"response","id":"r1","result":{"kind":"batch","rev":13}}
{"type":"event","event":{"type":"state.changed","rev":13,"client_id":"cli_7","request_id":"r1"}}
```

The version is `major.minor` and follows semver independently of the app
([R-API-001](../../docs/RULES.md#R-API-001)). A server serves a client with the same
major and a minor no newer than its own (`can_serve`): a 0.2 server serves a 0.1
client, and a 0.1 server refuses a 0.2 client. The rule is the same at 0.x as after 1.0,
because additions only ever bump the minor (see *Schema and compatibility* below). The
in-process `Control` implementation has
no handshake; it is compiled against the same crate.

### Tagging

Every enum uses serde's internally tagged form, so a message is one flat JSON object
whose tag says what it is:

- `type` names the message (`request`), the command, query, history op or result
  (`scene.create`, `history.undo`), or the event (`state.changed`). Command, query,
  history and event names are `noun.verb` in
  snake_case ([R-API-002](../../docs/RULES.md#R-API-002)), so the tag *is* the name that
  MCP tools, the CLI and logs use.
- `kind` names the class of a call and its result: `batch`, `action`, `query` or
  `history`. It is a
  different key from `type`, so a call can nest its command without the two tags
  colliding, and the class of every request is explicit on the wire.
- `code` names an error.

Why internally tagged: a flat object is easiest to write by hand and for agents, it
gives the schema one `oneOf` with a `const` per alternative, and an unknown tag fails
with an error that names it. Externally tagged enums nest every value one level deeper
under its name, adjacently tagged ones need a second `content` key, and untagged ones
give useless parse errors and ambiguous schemas.

<a id="calls"></a>
## Edits, actions and queries
<!-- @section
covers = ["crates/jstream-protocol/src/command.rs", "crates/jstream-protocol/src/envelope.rs", "crates/jstream-protocol/src/query.rs"]
depends_on = ["docs/adr/0013-command-model.md", "docs/RULES.md#R-ARCH-006"]
-->

Following [ADR-0013](../../docs/adr/0013-command-model.md), every command is exactly one
of two classes ([R-ARCH-006](../../docs/RULES.md#R-ARCH-006)):

- **Edit** (`Edit`): changes the collection state, the data that is saved in the
  collection file and that `rev` counts. Undoable, batchable, atomic.
- **Action** (`Action`): has effects outside the collection state, such as network,
  devices, files, secrets, time or runtime state (starting an output, going live,
  storing a stream key, OAuth, switching the program scene). Never undoable or
  batchable. The response says the action was accepted, and carries an
  `ActionResult` only for an action that answers with data (`pairing.start`'s code,
  0.21; `youtube.sign_in`'s consent URL and the broadcast a YouTube broadcast action
  made or changed, 0.23; every other action omits `result`); progress arrives as events
  (`output.state_changed`).

The classification rule: **if the command's whole effect is a change to the collection
state, and its inverse can be computed from the state before it, it is an edit.
Otherwise it is an action.** A command that does both is split into an edit and an
action. A **query** (`Query`) changes nothing and is neither.

The types enforce the split: a `Batch` holds only `Edit`s, an action travels alone, and
`Call` (tagged by `kind`) has exactly four forms: `batch`, `action`, `query` and
`history`. A `history` call carries no command: it names a direction and applies edits
the engine already holds (see *Undo* below), so it is neither class and R-ARCH-006
still covers every command.
`Command` is a classification view over both enums, with `class()`, `is_undoable()` and
`is_batchable()`. The tests `names_are_valid` and `every_command_is_classified` check
every name in the generated schema.

Creating edits carry a client-chosen ID (`scene_id`, `item_id`, `output_id`). A batch can then refer
to an object it creates earlier in the same batch, a retry can't create a duplicate,
and an undo can re-create a deleted object under its old ID. IDs are opaque strings.

Sources are shared instances, independent of scenes. `source.create` adds one, and
`item.add` places an existing source by reference, so one source can appear in many
scenes, or several times in one, as the same capture. `source.duplicate` makes a
separate copy under a new ID, with the same kind, settings and filters, and records the source it
copied in the copy's `duplicated_from`; `source.rename` renames one. An item placing a
shared source is a **reference**, and one placing a copy is a **clone**
([ADR-0023](../../docs/adr/0023-placements-references-and-clones.md)). `item.set_source`
(0.14) changes which source an item places and keeps the item's ID, placement and stacking
position, so a client converts between the two in one batch: `source.duplicate` then
`item.set_source` to the copy, or `item.set_source` back to `duplicated_from` then
`source.delete` of the unplaced copy. `duplicated_from` is a plain link, not a
dependency: deleting the original is allowed, `SourceInfo` then omits the link, and
`source.create` takes it (never naming the new source itself) so that undoing a
`source.delete` restores it. A `SourceKind` is `builtin` (a kind the engine describes) or `scene`, which
nests another scene. Positions are indexes: scenes in display order, items in stacking
order with 0 at the bottom.

<a id="placement"></a>
## Groups and placement addressing
<!-- @section
covers = ["crates/jstream-protocol/src/command.rs", "crates/jstream-protocol/src/keyframe.rs"]
-->

[ADR-0024](../../docs/adr/0024-scene-structure.md) §1 makes a group a scene with
`role: group` (protocol 0.12). `SceneRole` is `scene` or `group`. `scene.create` takes
an optional `role` (omitted means `scene`; protocol 0.17), and `scene.set_role { scene_id, role }`
changes it; the role changes no engine behaviour.
`scene.list` and `state.get` report `role`, omitted for `scene`, so a 0.11 reader sees
no change for ordinary scenes. `item.move { item_id, scene_id, index }` moves an item
to a position in another scene, keeping its ID and every placement property, under the
same cycle check as `item.add` (field `scene_id`). `index` is the item's position after
the move: up to the number of items already there for another scene, and below it for
the item's own scene, where the move is a reorder. Grouping is a client-built batch:
`scene.create` with `role: group`, `source.create` of a `scene` source,
`item.add` of it with `item.move` to the lowest selected index, then an `item.move` of
each selected item into the group, bottom first.

The placement edits (`item.set_geometry`, `item.set_transform`, `item.set_crop`,
`item.set_visible`, `item.set_opacity`) take an optional `scene_id` (ADR-0024 §2).
Absent, or the item's own scene, changes the item. A keyframe scene of the item's
scene writes that field of its override instead. Any other existing scene is
`invalid_argument` on `edits.N.scene_id`, and a missing one is `not_found`. An inverse
keeps the addressing of the edit it undoes.

**Keyframe scenes** (0.39, [ADR-0024](../../docs/adr/0024-scene-structure.md) §2,
[ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)):
- `scene.create_variant { scene_id, variant_of, name }` makes a keyframe scene (a
  **variant**) of `variant_of`, appended to the scene list. `variant_of` must exist and
  not be a variant itself (`invalid_argument` on `variant_of`: make the keyframe on its
  parent). Its inverse is `scene.delete`. `SceneInfo.variant_of` names the parent
  (omitted for any other scene).
- A variant has no items of its own. It shows its parent's items, at the parent's
  current values except for the fields it overrides: `geometry` (whole, as one
  `item.set_geometry` or `item.set_transform` writes it, reported as `geometry` or, for
  the 0.3 form, `transform`), `crop`, `visible`, `opacity` and, since 0.63, `effects`
  (below). `SceneInfo.overrides`
  lists them in item ID order, as `ItemOverride`s with at least one field set (omitted
  when there are none). An override names an item in the parent's tree: one of the
  parent's own, or, since 0.55, a child of a group (role `group`) the parent places, at
  any depth (before, `invalid_argument`). Any other item is `invalid_argument`. An override of a group's child applies
  wherever the group is placed in the tree (ADR-0040 §2), and the variant draws the
  group with it (#846).
- Writing a field that was absent is undone by
  `item.clear_override { scene_id, item_id, field }`, and writing one that was present by
  the same edit with the old value. `item.clear_override` of a present field is undone by
  writing the old value. Of an absent field it is allowed, changes nothing and has an
  empty inverse, so "Reset keyframe" is a batch of clears sent without reading first
  (ADR-0049 §2). A batch that changed nothing is no undo step.
- Everything else follows the parent, so the edits that add or order a scene's own
  things refuse a variant with `invalid_argument` on `scene_id`: `item.add`, `item.move`
  into it, and `scene.set_folder` (a variant is listed under its parent, never in a
  folder). `item.set_transform`, `scene.set_role`, renames, transitions and metadata
  work as on any scene.
- **Dropped overrides** (ADR-0040 §1): an edit that takes an item out of a variant's
  tree drops its override there in the same edit: `item.remove` of it, `item.move` of
  it out of the tree (a move between the parent and a group it places keeps it), and,
  for a group's children, `scene.set_role` of the group to `scene` and removing or
  retargeting (`item.set_source`) the last item that places the group. An override
  stays while any path reaches its item. The
  batch result lists each as a `DroppedOverride {scene_id, item_id, ...fields}`
  (`dropped_overrides`, omitted when there are none), and the inverse restores them
  after the item is back.
- **Effects** (0.63, [ADR-0088](../../docs/adr/0088-keyframe-scenes-override-item-effects.md)):
  an override's `effects` is a whole list of `FilterInfo` that the variant draws the
  item with in place of its own effects; an empty list draws none.
  `item.set_effects { scene_id, item_id, effects }` writes it whole, each filter checked
  and its settings completed as `filter.add` does (at most 8, unique IDs, a known
  `video_filter` kind); naming the item's own scene, or one that isn't its keyframe, is
  `invalid_argument` on `edits.N.scene_id`. The `filter.*` edits also take the owner
  `{"type":"keyframe","scene_id":…,"item_id":…}`, the item's effects as that keyframe
  shows them, with the same errors on `edits.N.owner`: where the keyframe follows the
  item, the edit first copies the item's own effects into the override, and its
  inverse is then `item.clear_override` of `effects`. References in an overridden
  effect are edges of the keyframe scene; a blocker names them as
  `overrides.<item_id>.effects.<filter_id>.<param>` of that scene, and `delete.plan`
  clears them with `item.set_effects`.
- **Timing** (0.99, [ADR-0129](../../docs/adr/0129-keyframe-timing-and-more-easings.md)):
  an override's `timing` is a `KeyframeTiming {delay_ms?, easing?, duration_ms?}` for
  switches into that keyframe: the item takes the keyframe's delay and easing in place
  of its own `ItemTransition`'s, and its duration in place of the preset's, whether it
  glides in, appears or leaves; an omitted field follows the item and the preset, and
  the preset's `stagger_ms` still adds to the delay. A switch into any other scene, the
  parent and the other keyframes included, doesn't use it. `item.set_timing { scene_id,
  item_id, timing }` writes it whole; only a keyframe scene takes it (the item's own
  scene is `invalid_argument` on `edits.N.scene_id`), a delay or duration over 60000 is
  `invalid_argument` on `edits.N.timing.<field>`, and a timing with no field set removes
  it. Its inverse is the old timing, or `item.clear_override` of `timing`.
- A variant depends on its parent: `scene.delete` of a parent is refused while it has
  variants (blockers with field `variant_of`), and `delete.plan` of a parent deletes its
  variants, and the `scene` sources that show them, before it.

<a id="audio"></a>
## Audio
<!-- @section
covers = ["crates/jstream-protocol/src/audio.rs"]
-->

[ADR-0028](../../docs/adr/0028-audio-model-v2.md) makes audio part of the collection
(0.19). An **audio source** is one whose kind has audio (`KindDescriptor.audio`), or a
`scene` source, whose audio is what the scene shows. Its `SourceInfo.audio` holds
`enabled`, `follow` (`always` or `when_shown`), its fader `gain_db` and `muted`; every
other source omits `audio`. A new source takes its kind's defaults: on, at 0 dB,
unmuted, following `always` for a kind with audio and no video (an audio input) and
`when_shown` for every other audio source, a `scene` source included.
`source.set_audio { source_id, enabled?, follow?, gain_db?, muted? }` sets any of them
and needs at least one. A `scene` source has no fader, mute or sends of its own: its
`gain_db` and `muted` stay 0 and false, and setting either is `invalid_argument`.

**Buses** are collection objects with an ID, a name and a fader (`gain_db`, `muted`).
Every collection has the master bus, ID `master`, named `Master`; it can be renamed and
faded but never deleted. `bus.create`, `bus.rename`, `bus.set_fader` and `bus.delete`
manage the others, at most 16 buses in all (`MAX_BUSES`). **Sends** route a source's audio into a
bus, post-fader, at a gain: `send.set { source_id, bus_id, gain_db }` creates one or
changes its gain, `send.remove` removes it. Only a source with a channel of its own
sends, so not a `scene` source. A new channel source sends to the master at 0 dB.
Deleting a bus removes the sends into it, and deleting a source removes its sends;
the inverses put them back. Since 0.24 a setting can refer to a bus (a `bus`
reference, which the `waveform` and `spectrum` kinds' `audio` uses to draw it), and
`bus.delete` is refused while one does, as for a scene or source. Every gain is in dB from −100 to +24; silence is `muted`.

**Output buses** (0.59): an output carries the audio of the buses in its
`audio_buses`, one per audio track, in track order. Every output writes one audio track
so far, so the list holds exactly one bus; `OutputInfo` leaves it out while it is
`["master"]`, the default and what a server older than 0.59 means. `output.set_audio_buses
{ output_id, audio_buses }` chooses it: any other length, or an output with no audio (a
virtual camera), is `invalid_argument` on `audio_buses`, and a bus that doesn't exist
`not_found`. A running output reads its bus, so moving it to another one is
`invalid_argument` on `output_id` until it stops; that also refuses `delete.plan`'s
batch for a bus a running output carries, rather than leave the output silent. The master stage's audio chain runs on the bus before the output's own
chain ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md) §1), a
CLAP filter there on a plugin host of that bus's own
([ADR-0086](../../docs/adr/0086-master-clap-filter-one-host-per-output-bus.md)). An
output that carries a bus blocks its `bus.delete` (blocker field `audio_buses`), and
`delete.plan` moves it back to the master.

**Monitors** (0.42, ADR-0028's D22): any bus, the master's included, may have a monitor,
an output device the engine plays the bus on while the program mixer runs, so the
operator hears it. `bus.set_monitor { bus_id, monitor? }` sets one, and with `monitor`
omitted or null takes it away; `BusInfo` reports it as `monitor`, omitted for none.
A `BusMonitor` is `{ device, tap }`: `device` names the output device as the platform
does, empty (the default) for the system's default output, and at most 512 bytes with
no control characters; `tap` is `pre_filters` (the bus's sum before its filters,
through its fader and mute: "before FX") or `post_filters` (after them, the default:
"after FX"). Neither tap goes through the master stage's chain. Whether the device is
connected is never checked by the edit: a monitor is collection state, valid on any
machine, and one whose device is missing stays silent. A tap a newer server sent parses
as `unknown`, which `bus.set_monitor` refuses with `invalid_argument` on
`monitor.tap`. The engine plays a monitor while the program stage runs; one it can't
open stays silent and is retried, never failing `output.start`.

Buses never feed buses (ADR-0028), so a send always runs from a source to a bus, and
an audio routing cycle can't be expressed. Filter chains (a source's from 0.22, the
master stage's and outputs' from 0.29) hold no reference that could feed one bus from
another either; a routing check comes with the first thing that could. `follow`'s activity gain is the engine's (D6); the
protocol only stores it.

Levels and loudness are runtime state: `meters.tick` carries them (see
[Events](#events)), never a query, and `audio.reset_loudness` restarts the master's
integrated loudness, an action because it changes no collection state.

<a id="kinds"></a>
## Kinds and settings
<!-- @section
covers = ["crates/jstream-protocol/src/kind.rs", "crates/jstream-protocol/src/settings.rs"]
-->

[ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md) decides the shape. A
`builtin` source's `kind` is a kind ID, and its settings travel beside it, as a
`Settings` map on the source. The protocol defines the descriptor and value shapes once,
never a type per kind.

- `kind.list` returns a `KindSummary` per kind (ID, category, name, version), ordered by
  ID. `kind.describe { kind_id }` returns the `KindDescriptor` and a JSON Schema of its
  settings generated from it (`settings_schema`), or `invalid_argument` on `kind_id` for
  a kind the engine doesn't have. Kinds are not collection state, so neither result
  carries `rev`.
- A `ParamDescriptor` has an `id`, a `type` with its constraints flattened into the same
  object (`bool`; `int` and `float` with `min`, `max`, `step`, `unit`; `enum` with
  `options`; `color` with `alpha`; `string` with `multiline` and `max_len`; `asset` with
  `accept`; `scene`; `source` with an optional `media`; `bus`, since 0.24;
  `capture_target` with a `kind`, `display`, `window` or, since 0.90, `device` (an
  unknown one parses as `CaptureTargetKind::Unknown`, so adding a kind is a minor
  change), and `max_len`, since 0.50: a
  string checked as `string` is, naming a target of that kind as `capture.targets`
  lists it, so a UI can offer those to pick from; `capture_format` with a `part`
  (`resolution`, `fps` or `pixel_format`; an unknown one parses as
  `CaptureFormatPart::Unknown`) and the `target` parameter naming the device, since
  0.90: one part of the format a device is opened in, a UI offering what
  `capture.targets` lists for that device. A `resolution` is `<width>x<height>`, each
  side 1 to `MAX_CAPTURE_SIDE`; an `fps` a number from 0 to `MAX_CAPTURE_FPS`; a
  `pixel_format` one of `PIXEL_FORMATS`; `""` or `0` leaves the part to the engine);
  `camera_controls` with the `target` parameter naming the device, since 0.105: an
  object of at most `MAX_CAMERA_CONTROLS` entries, each a control name (lowercase
  letters, digits and `_`, at most `MAX_CAMERA_CONTROL_NAME` bytes) set to a 32-bit
  integer or `CAMERA_CONTROL_AUTO` (`"auto"`), a UI offering the controls
  `capture.targets` lists for that device; a control the object doesn't name keeps what
  the device has), a
  `default`, a `label`, an
  optional `description`, an optional `warning` (since 0.93: one line a client shows
  beside the control, not only in a tooltip, saying what the setting gives away), the UI
  hints `group`, `advanced` and `visible_if`, and `live`.
  An unknown `type` parses as `ParamType::Unknown`, which a client shows read-only.
  `step`, `multiline` and the hints never affect validation.
- Values are plain JSON. Colours are `#rrggbb`, or `#rrggbbaa` when `alpha` is set.
  References are tagged objects: `{"type":"scene","scene_id":…}`,
  `{"type":"source","source_id":…}`, `{"type":"bus","bus_id":…}` (0.24) and
  `{"type":"asset","root":…,"path":…}`, with
  `root` one of `collection`, `app` and `absolute`. A reference parameter also accepts
  `null`. `Settings::references` finds every reference in a map without the descriptor.
- `source.create` takes optional `settings`. The engine checks each entry against the
  descriptor, fills every parameter left out (or sent as `null`, outside reference
  parameters) with its default, and stores the complete map. `source.set_settings
  { source_id, patch }` replaces the patched parameters, and `null` resets one to its
  default. Its inverse is the patch of the old values. A `scene` source has no settings.
- Rejections: an unknown kind (`edits.N.kind.kind`), an unknown parameter, a wrong
  type, a number out of range, an unknown enum option, a bad colour, an over-long
  string, or a bad asset path are `invalid_argument` with `field =
  edits.N.settings.<id>` (or `edits.N.patch.<id>`). Values are never clamped. A
  reference to a missing scene, source or bus is `not_found`. A `source` reference that
  requires `media` rejects a source whose kind doesn't produce it. Asset paths under
  `collection` and `app` are relative, with `/` separators and no empty or `..` segment,
  drive letter, leading `/` or `:` (which names an NTFS alternate data stream on Windows).
  Under `absolute` the path is `/…`, `\…`, `C:\…` or `C:/…`, with no `..` segment, no
  second leading separator (`\\host\share`, `//host/share`, `\\.\…` and `\\?\…` name
  a UNC share or a device, and opening one on Windows hands the engine's credentials to
  that host), no `:` but the drive letter's, and no `?` or `*` (Win32 never allows
  them in a file name, and `\??\UNC\…` or `\??\GLOBALROOT\Device\…` is an NT object
  path that reaches the same hosts and devices). Under every root, the last component
  must not name a DOS device, which Win32 opens in place of a file in any folder: a
  stem (the name before the first `.`, without trailing spaces, in any case) of `CON`,
  `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `COM¹`–`COM³`, `LPT0`–`LPT9`, `LPT¹`–`LPT³`,
  `CONIN$` or `CONOUT$`. So `C:\x\nul`, `\CON .png` and `/x/aux.txt` are refused under
  `absolute`, and `CON.png` and `media/nul` under `collection` or `app` (joined to the
  root folder, they still open the device), while `CONSOLE.png` or `COM10.png` is not.
  Earlier components may use these names: Win32 fails to open such a path, and elsewhere
  they are ordinary folders. An asset that doesn't exist yet is accepted. A value that
  isn't the reference a parameter takes (a bare path or id string, or another kind's
  reference) is refused with a reason that shows the reference's shape, such as
  `{"type": "asset", "root": "absolute", "path": "/path/to/file.png"}` (#1021).
- **References are dependency edges.** Items placing sources, `scene` sources showing
  scenes, and scene, source and bus references in settings (a filter's settings count
  as its source's) form one dependency graph. An edit that would close a cycle
  (`item.add`, `item.move`, `item.set_source`, `source.create`, `source.set_settings`,
  `filter.add`, `filter.set_settings`) is
  rejected with `invalid_argument` naming it (for example scene `a` -> source `sa` ->
  scene `a`). A bus depends on nothing, so a bus reference never closes one. Asset
  references are not edges.
- **Deletes are refused while anything refers to the target**, with no cascading edit
  ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)).
  `source.delete` is refused while an item places the source, and `scene.delete` while a
  `scene` source or a video mix shows the scene; either, and `bus.delete` (0.24), is
  refused while a setting refers to its target, and `bus.delete` (0.59) while an output
  carries the bus. The `invalid_argument` lists every blocker in `blockers`, each a `Blocker`
  `{entity, id, field}`: the item (`source_id`), the `scene` source (`kind.scene_id`),
  the video mix (`mix.input.scene_id`), the output (`audio_buses`) or the source whose
  setting refers to it (`settings.<param>`, or `filters.<filter_id>.<param>` for a
  filter's setting).
  `delete.plan` returns
  the batch that removes them with the target (see [Queries](#queries)). Hotkey
  bindings are never blockers: a delete removes the bindings that name its target
  (below).

`SourceInfo` reports `settings`, omitted when empty (a `scene` source, or a kind with no
parameters), so a reader of 0.7 messages sees no change for those.

<a id="filters"></a>
## Opacity, blend and filters
<!-- @section
covers = ["crates/jstream-protocol/src/filter.rs", "crates/jstream-protocol/src/finite.rs"]
-->

All of this is added in 0.22. A placement has an `opacity`, from 0 to 1 an alpha multiplier (default 1), and from
0.83 up to `MAX_OPACITY` (4) a gain on its light above 1: its colour is multiplied and its alpha by at most 1, so an
`additive` item adds more light than its source draws (#1101); and a
`BlendMode` (`normal`, `additive`, `multiply`, `screen`, `overlay`, `darken`, `lighten`,
`difference`, `exclusion`; default `normal`). `item.add` takes both, optional;
`item.set_opacity { item_id, scene_id?, opacity }` and `item.set_blend { item_id, blend }`
set them. Out of range is `invalid_argument` on `edits.N.opacity`. Before 0.47 every
mode but `normal` was `invalid_argument` on `edits.N.blend`; from 0.47 the compositor
draws all nine, in linear light except overlay, difference and exclusion, which blend
sRGB-encoded values ([ADR-0082](../../docs/adr/0082-blend-modes-drawn.md)). From 0.101
the collection's blending space picks where every mode mixes: `srgb`, the default, as a
browser composites, or `linear`, ADR-0082's ([ADR-0126](../../docs/adr/0126-one-set-of-blend-modes-and-a-blending-space.md)). 0.75 to 0.100 had
`normal_srgb` and `additive_srgb` (ADR-0105);
they still parse, as `normal` and `additive`, and are never sent.
`ItemInfo` reports both.

A source has a video and an audio filter chain
([ADR-0026](../../docs/adr/0026-source-filter-chains.md)). The `filter.*` edits address a
filter by `owner` (`{"type":"source","source_id":…}`) and `filter_id`, unique within the
owner: `filter.add { owner, chain, filter_id, kind, settings?, enabled?, index? }`,
`filter.remove`, `filter.set_enabled { enabled }`, `filter.set_settings { patch }` (the
rules of `source.set_settings`) and `filter.reorder { index }` within the chain. A
source has a video chain if its kind produces video and an audio chain if it produces
audio and is not a `scene` source; `kind` must be a `video_filter` or `audio_filter`
kind for that chain (`invalid_argument` on `edits.N.kind`), and a chain holds at most
8 filters (`edits.N.chain`). A missing filter is `not_found` with entity `filter`.
`SourceInfo.filters { video, audio }` reports them, each omitted while empty.

From 0.29 the master stage and each output own chains too
([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)), with the owners
`{"type":"master"}` and `{"type":"output","output_id":…}` and the same edits. The
master's audio chain runs on every bus that feeds an output, after that bus's own chain;
an output's runs after it, for that output only, and gives the output its own audio
encode from its next start (edits to a chain it started with apply live). From 0.60
their video chains take filters too (earlier minors refused them with
`invalid_argument` on `edits.N.chain`): the master's runs once per frame on each mix an
output draws, and an output's after it, for that output only. A stream or recording
with a video filter, even a disabled one, gets its own render pass and encoder session
from its next start (`output.status` reports it `dedicated`); a virtual camera has a
video chain only, and with a filter gets a render pass of its own but no encoder
session. An audio filter on an output that carries no audio (a `virtual_cam`) is
`invalid_argument` on `edits.N.chain`. A missing output is `not_found` with entity
`output`. Their filters can't hold a scene, source or bus reference yet
(`invalid_argument` on the parameter), since the delete checks follow only a source's
references. From 0.79 they may name a file (an asset reference), which is no edge:
the engine loads it as it does a source's filter's, so `apply_lut` or `image_mask`
grades the program or one output. Before 0.79 an asset was refused there too. `output.delete`'s
inverse re-adds the output's filters. `OutputInfo.filters` and `state.get`'s
`master { filters }` report them, each omitted while empty.

From 0.43 a scene and an item name chains too
([ADR-0065](../../docs/adr/0065-scene-chains-and-item-effects.md)), with the owners
`{"type":"scene","scene_id":…}` and `{"type":"item","item_id":…}` and the same edits. A
scene's filters run on its whole composite, wherever it renders; an item's effects run
on that one placement's cropped, fitted box, before its opacity and blend. Both have a
video chain only. `SceneInfo.filters` and `ItemInfo.effects` (a list of `FilterInfo`)
report them, each omitted while empty, so an older server's answer parses as none. A
`video_filter` descriptor's optional `max_outset_px` is the most the kind draws beyond
each edge of its input at any settings, which pads an item's effects layer. A scene's
or an item's audio chain is `invalid_argument` on `edits.N.chain`, and a keyframe scene
as the `scene` owner on `edits.N.owner`, since it shows its parent's filters (an item's
effects in a keyframe take the `keyframe` owner, 0.63). References in
them are edges of the scene, or of the item's scene: a blocker names them as
`filters.<filter_id>.<param>` of the scene or `effects.<filter_id>.<param>` of the item.
The engine runs a scene's filters on its composite and an item's effects over its box,
once per frame each.

Every float on the wire (`Vec2`, `ItemTransform`, `ItemGeometry`, an opacity) is a `Finite32`:
an `f32` that is never NaN or infinite. Computed geometry results use plain `f32`. serde
would otherwise narrow an out-of-range JSON number such as `1e39` to infinity, which
then serializes as `null` and no longer parses. So a
non-finite or out-of-`f32`-range number fails deserialization, and
`ClientMessage::parse` rejects the whole message as `malformed`. In-process clients
can't build a non-finite value either: `Finite32::new` and `Vec2::new` return `None`
for one. Settings values keep non-integer JSON numbers as `f64`, parsed to the nearest
one (serde_json's `float_roundtrip`), so a message with a long number still serializes
and parses back to itself.

<a id="outputs"></a>
## Outputs and stream keys
<!-- @section
covers = ["crates/jstream-protocol/src/output.rs"]
-->

Outputs are collection state too: `output.create` adds one, stopped, with its
`OutputKind`; `output.set_target` changes where an RTMP output sends, from its next
`output.start`; `output.delete` removes one, and is refused with `invalid_argument`
while it runs. A target URL must be an `rtmp://` or `rtmps://` server URL with a host and an
application name, and is refused if it could carry a credential (`user:password@`, a
query or a fragment). A key pasted into the path can't be told from a nested
application name, so it is stored as given. A `record` output (0.9) records to a file
on the engine's machine: `path` must be absolute (`/…`, `\…`, `C:\…` or `C:/…`), have
no `..` segment and end in a file name. As for an `absolute` asset path, it must not
start with a second separator (a UNC share or device, `\\host\share\…`, `\\.\…`,
`\\?\…`), nor contain `:` but the drive letter's, `?` or `*` (`\??\UNC\…`), nor
end in a DOS device name (`NUL`, `nul.mp4`, `CON`, `COM1`, `LPT1`, …), by the same rule
as an asset path's last component. And
`container` is `mp4` (fragmented). `output.start` writes `<path>.part` and fails with
`action_failed` when `path` or `<path>.part` already exists, so a recording never
overwrites; `output.stop` renames it to `path`. A recording output has no RTMP target,
so `output.set_target` refuses it with `invalid_argument`. `output.set_path
{output_id, path}` (0.74, #1072) points it at another file, checked like `path` above,
keeping its container, encoder profile, mix, bus and filters; a running recording keeps
its file, and the new path applies from the next `output.start`. On any other output it
is `invalid_argument`, and its inverse is `output.set_path` with the previous path. A `virtual_cam` output (0.18) has no fields: it offers the program
video to other apps on the engine's machine as a webcam, with no audio, at the canvas
size it has when it starts and 30 fps. `output.start` fails with `action_failed` when
the engine has no virtual camera, the camera isn't registered on the machine, or it
can't offer that size. It has no target either, so `output.set_target` refuses it too,
and it encodes nothing, so its encoder profile is stored but unused. A 0.9 to 0.17
client reads it as `Unknown`. The
stream key is not collection state: it is a secret held in the OS credential store,
so it is set with the `output.set_key` action and removed with `output.clear_key`. A
`youtube` target (0.23) names a broadcast instead of a URL: its ingest and key come
from YouTube on each `output.start` ([YouTube](#youtube)), so it needs no
`output.set_key`. Its ID must be ASCII letters, digits, `-` and `_`, at most 128 bytes.
It is write-only ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): no response, event,
error, query or log carries it, and `output.status` reports only `key_set`. A key is
bound to the server it was set for (#343): when an applied batch, undo or redo deletes
the output, or points it at another server (`OutputKind::key_server`: scheme, host and
port; a new path on the same server keeps the key), the engine removes the key from the
credential store, and refuses that output's `output.start` with `action_failed` until
the key is gone. So a retarget never sends the old key to the new server, and an output
re-created with a deleted one's ID starts with no key. Undoing the delete or the
retarget restores the target but not the secret: set it again with `output.set_key`.
A start already opening when the retarget lands opens the server it checked, so while
an output's `output.start` is opening, `output.set_key` for it fails with
`action_failed`: a key sent for the new server waits until the start has settled.
The two stay edits under the classification rule above: their effect on the collection
state, and its inverse, are unchanged, and the key, which is not collection state, is
removed by the engine guarding it, as a deleted output's runtime state is dropped.
`output.clear_key` succeeds for an output no longer in the collection. No message
changed, so this changes no `PROTOCOL_VERSION`. Its errors carry fixed reasons that
never quote the key.

<a id="encoder-profiles"></a>
## Encoder profiles
<!-- @section
covers = ["crates/jstream-protocol/src/encoder.rs"]
-->

Encoder profiles (0.13) are collection state as well
([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)).
`encoder.create` and `encoder.update` take a `profile_id`, a `name` and an
`EncoderProfile`: H.264, HEVC or AV1 (the last two since 0.41) video with `cbr` or `vbr` rate control (100..=100000 kbit/s;
a `vbr` peak at least the average and at most 200000), a keyframe interval of 1..=10 s
and a hardware preference (`prefer`, `require` or `avoid`, which an omitted field
means), and AAC audio
at 32..=320 kbit/s. Anything else, including a value a newer server sent as `unknown`,
is refused with `invalid_argument` naming the field. Whether the machine has an encoder
for the codec is not checked there, since a profile is valid on any machine: an
`output.start` on a machine with none fails. `encoder.delete` is refused while
an output uses the profile. `output.set_encoder` points an output at a profile, or with
no `profile_id` back at the engine's default (H.264 CBR 8000 kbit/s, a keyframe every
2 s, hardware when there is one, AAC 160 kbit/s). Both apply from the output's next `output.start`.
Running outputs with the same mix and the same profile, as it was when each started,
share one encoder session, and `output.status` reports the sharing.

<a id="hotkeys"></a>
## Hotkeys
<!-- @section
covers = ["crates/jstream-protocol/src/hotkey.rs"]
-->

Hotkey bindings (0.25) are collection state too
([ADR-0024 §5](../../docs/adr/0024-scene-structure.md),
[ADR-0040 §3](../../docs/adr/0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md)):
a `Chord` bound to an `Action`, which a client that listens for keys sends when the
chord is pressed. The engine only stores bindings; the desktop host (OS-global
shortcuts) and the web UI (in-page ones) dispatch them, and the headless daemon none.
- **Chords.** Zero or more of the modifiers `Ctrl`, `Alt`, `Shift` and `Meta`, each at
  most once and in that order, then one key, joined by `+`: `F1`, `Ctrl+Digit1`,
  `Ctrl+Shift+KeyM`. The key is a W3C UI Events
  [`KeyboardEvent.code`](https://www.w3.org/TR/uievents-code/) value from
  `Chord::KEYS` (letters, digits, punctuation, function keys F1–F24, editing and
  arrow keys, the keypad and the media keys): the physical key, whatever the layout,
  which is what browsers and Tauri's global-shortcut API report. A chord has exactly
  one spelling, so two bindings are on the same keys exactly when their strings are
  equal. Anything else is `invalid_argument` on `chord`, with `Chord::problem`'s reason.
- **Wheel chords** (0.104, [ADR-0112](../../docs/adr/0112-assignable-camera-zoom-bindings.md)
  §3). A chord may end in the wheel instead of a key, `Chord::WHEEL`: `WheelUp` (away
  from the person) or `WheelDown`, with modifiers like any key, such as
  `Ctrl+WheelUp` (`Chord::is_wheel`). A listener runs a wheel chord once per notch, or
  per 120 units of high-resolution wheel delta, so it can only be bound to an action
  that takes steps (`Action::takes_steps`: today `scene.zoom_camera`, whose `steps` is
  what one notch zooms). Bound to any other action, `hotkey.set` is `invalid_argument`
  on `action.type`.
- **`hotkey.set {chord, action}`** binds a chord; its inverse is `hotkey.clear`. It is
  refused with `already_exists` (entity `hotkey`, the chord as `id`) for a chord that
  is bound: rebinding is a batch of `hotkey.clear` then `hotkey.set`. `action` is an
  action request, checked as an `action` call is, and only actions can be bound, so a
  hotkey never adds to an undo history. `output.set_key` can't be bound (its stream key
  would be stored in the collection, [R-SEC-001](../../docs/RULES.md#R-SEC-001)), nor
  can the preview actions (they serve only the sending session), `live.set` and
  `live.cancel` (their gesture belongs to the sending session), `pairing.start` and
  `device.revoke` (the server answers them for the client that asks), the
  `youtube.*` actions (YouTube is driven through the UI,
  [ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)), or the
  `plugin.*` actions (a person installs and grants plugins): each is
  `invalid_argument` on `action.type`. A scene, output, preset or item the action names
  must exist, else `not_found`. `item.show`, `item.hide` and `item.toggle` (0.80,
  [ADR-0101](../../docs/adr/0101-item-show-hide-actions.md)) are the actions that show
  and hide one item: they change its placement's visibility as `item.set_visible` would,
  moving `rev` on with no undo step, and the engine plays the item's own `appear` or
  `disappear` effect on program over the default preset's duration.
- **`hotkey.set_labels {chord, labels}`** (0.100,
  [ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)) sets the labels of
  a binding, whole, from the collection's one set of labels: the binding must exist
  (`not_found`, entity `hotkey`), then each label (`not_found`), each listed once
  (`invalid_argument` on `labels`). They are kept ordered by ID, and an empty list
  removes them; its inverse sets the old ones. `hotkey.set` binds with none.
- **`hotkey.clear {chord}`** removes a binding, or is `not_found` (entity `hotkey`); its
  inverse is `hotkey.set` with the action, then `hotkey.set_labels` when it carried any.
- **Deleting what a binding names removes the binding**, in the same edit:
  `scene.delete`, `source.delete`, `output.delete`, `encoder.delete` and
  `transition.delete` drop every binding whose action names their target
  (`Action::named_objects`; today scenes, outputs, the preset a `scene.switch`
  names, the scene, keyframe scene and preset a `scene.set_keyframe` names, and the
  item an `item.*` action names), and any edit that takes an item away
  (`item.remove`, or a delete that takes its scene or source) drops the bindings naming
  the item (ADR-0101). A binding goes with its labels. Their inverses restore the
  bindings, with their labels, after the target. If another client has bound one of those chords since,
  that `hotkey.set` fails and so does the undo, rather than overwriting the binding.
  A batch's result lists the bindings it removed (`removed_hotkeys`), and `delete.plan`
  lists the ones its batch would (`hotkeys`), so a client can say "this also removes
  the hotkey Ctrl+Digit1".
- Reads report a binding as a `HotkeyBinding {chord, action, labels}` (`labels` omitted
  for none) whose `action` is plain
  JSON, so a client older than an action type still reads the collection; it keeps the
  binding and can't run it. `HotkeyBinding::action` parses it.

<a id="ptz"></a>
## PTZ cameras
<!-- @section
covers = ["crates/jstream-protocol/src/ptz.rs"]
depends_on = ["docs/adr/0146-ptz-cameras-shots-and-actions.md"]
-->

PTZ cameras (0.111) are physical heads the engine drives over VISCA
([ADR-0146](../../docs/adr/0146-ptz-cameras-shots-and-actions.md)). The camera list,
with each head's address, belongs to the machine (ADR-0123 §4): the host gives it to
the engine, and nothing on the wire adds, changes or removes a camera. A call names a
camera by its name on that list, 1 to 64 bytes with no control characters. Positions
are the head's own counts and speeds VISCA's steps.
- **Actions**, runtime and never undone: `ptz.drive {camera, pan, tilt, zoom}` (signed
  speeds, pan -24 to 24, tilt -23 to 23, zoom -8 to 8, 0 stops), `ptz.aim {camera, pan?,
  tilt?, zoom?, focus?, speed?}` (pan and tilt together, at least one field, speed 1 to
  24), `ptz.home {camera}`, `ptz.recall {camera, preset}` and `ptz.store {camera,
  preset}` (0 to 127; a store writes the head's memory), and `ptz.focus {camera, focus,
  speed?}` (`auto`, `manual`, `one_push`, `near`, `far`, `stop`; speed 1 to 8).
  `ptz_action_check` gives the order every implementation checks in: the fields
  (`invalid_argument`), then whether the machine lists any camera (`action_failed`),
  then whether it lists this one (`invalid_argument` on `camera`). An action returns
  once its command is queued. Each but `ptz.drive` can be bound to a trigger (a binding
  fires only on press, so nothing would stop a bound drive), and none names a collection
  object. A drive that moves stops when the session that sent it ends. A plugin can't send them, or read `ptz.list`.
- **`ptz.list`** gives each camera (`PtzCameraInfo`): its name, `address`, `transport`
  (`visca_ip`, `udp` or `tcp`), `visca_address`, whether it answered in the last three
  seconds, its last `pose` and `error`, and its `sent`, `replies` and `dropped` counts.
  Empty when the machine drives none, which is the default.
- **Shots** (`PtzShot`): a scene's `ptz_shots`, at most 16, one per camera, each a
  `camera` and either a `preset` or a pose (`pan` and `tilt` together, `zoom`,
  `focus`), flat in the shot. `scene.set_ptz_shots {scene_id, shots}` replaces the
  list, a keyframe scene's too; `check_ptz_shots` refuses a list with
  `invalid_argument` on `shots[i].<field>`; its inverse sets the old list. A keyframe
  scene's shot for a camera replaces its parent's, and it follows the parent's others.
  The engine sends a scene's shots when it goes on program, and when it goes on
  preview for the cameras the program's shots don't name; an edit moves nothing.

<a id="bindings"></a>
## Controller bindings
<!-- @section
covers = ["crates/jstream-protocol/src/binding.rs"]
depends_on = ["docs/adr/0122-controller-input-midi-and-one-binding-system.md"]
-->

Controller bindings (0.96) are collection state beside the hotkeys
([ADR-0122](../../docs/adr/0122-controller-input-midi-and-one-binding-system.md)
decision 1): a `Trigger` bound to an `Action`. A key chord stays a hotkey, so
`state.get`'s `hotkeys` lists chords only, as a client that registers them with the OS
expects; every other trigger is a binding, in `bindings`. Where a client listens for a
chord and sends its action, a controller's messages come from an input the engine reads,
so the engine runs the action itself, on the first frame at or after the message's time.
No input feeds bindings yet; the MIDI and JTunnel inputs add them.
- **Triggers** are tagged by `kind`: `midi_note {note}`, `midi_cc {controller}` and
  `midi_program {program}`, each 0 to 127, with an optional `channel` (1 to 16, as a
  device's panel shows it; missing for any channel) and an optional `input` (the name of
  a MIDI port or JTunnel sender, at most 64 bytes and no control characters; missing for any). A note fires on
  note-on, a control change when its value crosses from 63 or below to 64 or above (so a
  button sending 127 then 0 fires once), a program change each time. Two triggers are
  the same exactly when they are equal, so channel 1 and any channel are two bindings,
  and a message matching both fires both. Anything else is `invalid_argument` on
  `trigger.<field>`, with `Trigger::problem`'s reason.
- **`binding.set {trigger, action}`** binds a trigger; its inverse is `binding.clear`.
  It is refused with `already_exists` (entity `binding`, the trigger as `Display`
  writes it, such as `MIDI note 36 on channel 10`, as `id`) for a trigger that is
  bound. The action is checked as `hotkey.set` checks it: the same actions can't be
  bound, and an object it names must exist.
- **`binding.clear {trigger}`** removes a binding, or is `not_found` (entity
  `binding`); its inverse is `binding.set` with the action.
- **Deletes remove bindings** as they remove hotkeys, in the same edit, and their
  inverses restore them after the hotkeys. A batch's result lists them in
  `removed_bindings`, and `delete.plan` in `bindings`, ordered by trigger.
- Reads report a binding as a `Binding {trigger, action}` whose `action` is plain JSON,
  as `HotkeyBinding`'s is. `Binding::action` parses it.

<a id="organisation"></a>
## Folders, labels and scene options
<!-- @section
covers = ["crates/jstream-protocol/src/organise.rs", "crates/jstream-protocol/src/geometry/camera.rs"]
-->

Folders, labels and organisation metadata are collection state too
([ADR-0024](../../docs/adr/0024-scene-structure.md) §3 and §4). They change no
rendering, switching or nesting. Folders organise the scene list as a tree: an ordered
list of `{ folder_id, name, parent? }`, where a folder's children show in folder order,
and a scene names at most one folder (`scene.set_folder`) while keeping its place in the
flat display order. `folder.set_parent` refuses a parent that would put the folder
inside itself. Labels are an ordered list of `{ label_id, name, color, icon? }`, and a
source carries a set of them, with a favourite flag (`source.set_meta`); they belong to
the source, so every item placing it shares them, and `source.duplicate` copies them.
A scene's `SceneMeta` (`scene.set_meta`) is its `labels` (since 0.45,
[ADR-0078](../../docs/adr/0078-scenes-carry-labels.md); checked and stored as a
source's), `icon`, `color`, `favourite` and `show_in_multiview` (default `true`). `scene.set_meta` and `source.set_meta` replace
the whole metadata, so a field left out takes its default. A scene's `SceneOptions`
(`scene.set_options`, since 0.65,
[ADR-0089](../../docs/adr/0089-scene-options-and-nested-audio.md)) are behaviour, not
organisation: `include_audio` (default `true`), off for a scene whose sources reach the
Mixer through no nesting of it, whatever each scene source's own `audio.enabled`, while
it is still heard on program itself (on a group, which is only reached through nesting,
it leaves out the children's audio wherever the group shows), and `background` (default
none, which is transparent), `#rrggbb` or `#rrggbbaa` with straight alpha, drawn under the
scene's items wherever it is drawn, though never for a group (`parse_rgba` reads it). `scene.set_options` replaces them all the
same way, a keyframe scene has its own except a background, which it shows from its
parent and is refused, and `state.get` reports them flattened beside the scene's other
fields, each omitted while it has its default. A scene's **camera**
(`scene.set_camera {scene_id, camera?}`, since 0.70,
[ADR-0094](../../docs/adr/0094-scene-cameras.md)) makes it a 3D scene: `SceneCamera`'s
`position` and `target` (`Vec3`, scene pixels, x right, y down, z toward the viewer),
`fov_deg` (vertical, 1 to 170, default 45), `roll_deg` (default 0), `depth_sort`
(default `true`, not written while true), and since 0.77 a depth of field: `focus_px`,
the distance along the view of the plane in focus (absent, everything is in focus), and
`blur_px`, the blur radius of a point infinitely far behind it, 0 to 64 (default 0, not
written while 0) ([ADR-0108](../../docs/adr/0108-camera-depth-of-field.md)). A camera
omitted removes it. The edit refuses a field of view out of range
(`edits.N.camera.fov_deg`), a coordinate past the drawable limit (`edits.N.camera`), a
position closer than 0.001 px to the target (`edits.N.camera.target`), a focus distance
not above 0 or past the drawable limit (`edits.N.camera.focus_px`) and a blur outside 0
to 64 (`edits.N.camera.blur_px`). On a keyframe scene it sets the variant's override, which a
keyframe of a 2D scene may have too ([ADR-0128](../../docs/adr/0128-moves-between-2d-and-3d-glide.md));
removing the parent's camera drops its variants' overrides (a removal on a parent with
none drops nothing), and its inverse restores them. `SceneInfo.camera` reports the scene's own
camera, on a keyframe scene its override (it shows its parent's where it has none), and
is omitted when there is none. It is live-adjustable. Other colours are sRGB `#rrggbb`;
an icon is an opaque name from the UI's icon set, checked only for its length (1 to 64
bytes). Since 0.100 hotkey bindings ([Hotkeys](#hotkeys)) and timelines
([Timelines](#timelines)) carry labels from the same set, checked the same way
([ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)), and
`label.members {label_id}` lists everything that carries one: its scenes in display
order, each with its parent when it is a keyframe scene, its sources by ID, its bindings
by chord with their actions, and its timelines by scene then ID, each with its scene;
`not_found` for a label that doesn't exist. A folder that holds a scene or folder, and a
label anything carries, can't be deleted: the `invalid_argument` lists the `blockers`
(`folder`, `parent` or `labels`; for a label, its sources first, then its scenes, its
bindings and its timelines), and `delete.plan` with a `folder` or `label` target returns
the batch that moves what the folder holds up to its parent, or takes the label off
each, in that order, then deletes it.
`state.get` reports `folders` and `labels`, each omitted when empty, and `scene.list`,
`state.get` and `source.list` report each scene's and source's fields, each omitted
while it has its default, so an older client reads the collection as before.

<a id="video-mixes"></a>
## Video mixes
<!-- @section
covers = ["crates/jstream-protocol/src/mix.rs"]
-->

Video mixes (0.28) are collection state too: what an output's video shows.
`video_mix.create` and `video_mix.update` take a `mix_id`, a `name` and a `VideoMix`: an
`input` (`program`, the scene on program; `preview`, which still shows the program,
since no mix draws studio mode's preview scene yet; or `scene` with a `scene_id`, one scene whatever is on
program), a `resolution` whose sides are even and 16..=8192, and an `fps_divisor` of
1..=60 (missing, it parses as 1): the mix renders on every `fps_divisor`th tick of the
engine's base frame rate (30 by default, or 60 after `canvas.set_frame_rate`, ADR-0119). The canvas is fitted inside the resolution, centred, with
black bars where the aspect ratios differ, never stretched
([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)). Anything else,
including an input a newer server sent as `unknown`, is refused with `invalid_argument`
naming the field (`edits.N.mix.resolution.width`), and a missing scene with
`not_found`. `video_mix.delete` is refused while an output shows the mix.
`output.set_video_mix` points an output at a mix, or with no `mix_id` back at the
program at the engine's canvas size and base rate. A running output follows a change
of its mix's `input` from the next frame; a new resolution, rate or mix applies from
its next `output.start`, like an encoder profile. A virtual camera keeps the mix it
names but always shows the program at the canvas size, and its own video chain runs
over that (#1006).

<a id="transitions"></a>
## Transitions and studio mode
<!-- @section
covers = ["crates/jstream-protocol/src/transition.rs"]
-->

[ADR-0029](../../docs/adr/0029-transitions.md) and
[ADR-0037](../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md) decide
the model (0.30). A transition preset is collection state:
`{preset_id, name, kind, duration_ms, easing, settings}`, where `kind` is a transition
kind `kind.list` reports (category `transition`: `cut`, `fade`, `move` and `stinger` so
far) and `settings` are checked against its descriptor as a source's are (`unmatched`,
the effect for items nothing pairs, and `stagger_ms`, an extra delay per incoming item by
stacking order, for `fade` and `move`; [ADR-0052](../../docs/adr/0052-transition-mechanics-and-studio-preview.md)).
A `stinger` takes the `scene` it plays over the switch (a scene reference, or `null` to
cut), `cut_ms`, when the picture under it cuts (default 500, at most the duration), and
`stagger_ms`, which staggers the stinger scene's items by stacking order.

- `transition.create` adds a preset, `transition.update` changes its fields or patches
  its settings (a new `kind` resets the settings to that kind's defaults, overlaid by
  the patch), and `transition.delete` removes one, clearing the scene overrides and
  removing the hotkey bindings that name it in the same edit; its inverse re-creates
  the preset and restores them. The
  default preset can't be deleted (`invalid_argument` on `preset_id`);
  `transition.set_default` moves it. A new collection holds one preset, `cut`, named
  Cut, as its default, with `duration_ms` 0: a switch that uses it shows the new scene
  on the next frame. A `cut` ignores a stored duration, so one saved at 300 ms (the
  preset this collection used to start with) still cuts.
- `scene.set_transitions {scene_id, transition_in?, transition_out?}` sets both of a
  scene's overrides (absent clears one), and `item.set_transition {item_id,
  transition}` an item's `ItemTransition {delay_ms, easing, glide, appear,
  disappear}`, omitted from `ItemInfo` while it is the default.
- `item.set_locked {item_id, locked}` locks or unlocks an item's canvas box
  ([ADR-0096](../../docs/adr/0096-item-lock-and-png-preview-frames.md), 0.71), reported
  as `ItemInfo.locked`, omitted while false. It is an editing aid a client honours by
  not dragging the box; the engine still applies every edit to a locked item. It
  belongs to the item, so it takes no `scene_id` and a keyframe doesn't override it.
- `item.set_effects_space {item_id, space}` sets where an item's effects run
  ([ADR-0106](../../docs/adr/0106-item-effects-in-canvas-space.md), 0.76): `box`, the
  default, over its cropped, fitted box before it is placed, or `canvas`, over a
  canvas-sized layer it is drawn into where it is placed, which then covers the canvas
  with the item's opacity and blend. So a canvas-sized mask lines up with the canvas
  however the item is moved, scaled, turned or flipped. Reported as
  `ItemInfo.effects_space`, omitted while `box`. Like a lock it belongs to the item:
  no `scene_id`, and a keyframe doesn't override it. A canvas-space item never glides
  in a Move.
- `scene.switch {scene_id, preset_id?}` resolves the preset in order: the one it names
  (`not_found` when there is no such preset), else the new scene's `transition_in`,
  else the current scene's `transition_out`, else the default. A switch while a
  transition runs starts the new one from what is on screen.
- `studio.set_preview {scene_id}` puts a scene on preview, studio mode's next scene.
  Like a switch it changes no `rev` and is never undone; a switch leaves preview where
  it is.
- `studio.set_warm {scenes}` (0.115, [ADR-0117](../../docs/adr/0117-real-time-video-thread-model.md) §7)
  sets how many scenes besides the program the studio keeps warm, 0 to
  `MAX_WARM_SCENES` (8; `invalid_argument` on `scenes` past it), `DEFAULT_WARM_SCENES`
  (3) until it is set. The Preview scene is warm even past it; the rest are the scenes
  most recently on program. An application setting: no `rev`, never undone, no event,
  whatever collection is open, and kept across restarts when the engine's host names a
  file for it. It can't be bound to a hotkey, and no plugin is granted it.

An `Easing` is `ease_in_out`, `ease_out`, `linear` or `spring`, and since 0.99
([ADR-0129](../../docs/adr/0129-keyframe-timing-and-more-easings.md)) `ease_in` (cubic),
`hold` (no progress until the end, then the whole jump) or `{"cubic_bezier": {x1, y1, x2,
y2}}`, CSS's curve, with `x1` and `x2` in 0 to 1 and `y1` and `y2` in -2 to 3, so it may
overshoot (`Bezier`; `Bezier::BACK_OUT` is the curve a client offers first). A point out
of range is `invalid_argument` on `…easing.cubic_bezier.<field>`, on a preset, on an
item's `transition` or on a keyframe's `timing`. `ItemEasing` is the same list plus
`inherit`. `Easing::apply` is the one easing function every implementation evaluates, so
the UI's preview of a transition and the engine's render agree.

<a id="batches"></a>
## Batches, revisions and `expected_rev`
<!-- @section
covers = ["crates/jstream-protocol/src/command.rs"]
depends_on = ["docs/RULES.md#R-API-004"]
-->

Edits are only sent inside a `Batch`; a single edit is a batch of one.

- `rev` is the collection state revision. Every applied batch increments it by exactly
  one, however many edits it holds. Actions and queries never change it.
- A batch is atomic: the engine applies its edits in order, each against the state the
  previous one left. If any edit fails, nothing is applied, `rev` is unchanged, and the
  error names the failing edit's field (`edits.2.name`).
- `expected_rev` is optional ([R-API-004](../../docs/RULES.md#R-API-004)). If it is set
  and the state is no longer at that revision, the batch fails with `rev_conflict`
  (carrying `expected` and `current`) and nothing is applied. The client re-reads the
  state and retries: optimistic concurrency for read-modify-write. Without it, the batch
  applies to whatever is current.
- `idempotency_key` is optional. The engine remembers, per client, the result of each
  batch applied with a key. A retry with the same key is not applied again and gets the
  original result, even if `rev` has moved on since.
- Results: a successful batch answers `{"kind":"batch","rev":N}`, the revision it
  produced, with `removed_hotkeys` when its deletes removed hotkey bindings (0.25;
  omitted when there are none), `removed_bindings` likewise for controller bindings
  (0.96) and `dropped_overrides` when it dropped keyframe
  overrides (0.39; omitted when there are none). Queries that read collection state report the `rev` they read.

<a id="queries"></a>
## Queries
<!-- @section
covers = ["crates/jstream-protocol/src/query.rs", "crates/jstream-protocol/src/profile.rs", "crates/jstream-protocol/src/quality.rs", "crates/jstream-protocol/src/activity.rs"]
-->

Queries read and never change `rev`. Those that read the collection state carry the
`rev` they read, so a client can send its next batch with that `expected_rev`:

- `state.get`: the whole collection at one revision: the `canvas` (missing from a
  pre-0.11 server, it parses as 1920×1080), the `blending` space (always sent from
  0.101; missing from an older server, it parses as `linear`, ADR-0126), every source, every scene in display order
  with its items, every output (`outputs`, ordered by ID; missing from a pre-0.7
  server, it parses as empty), every encoder profile (`encoders`, ordered by ID;
  missing from a pre-0.13 server, it parses as empty), every video mix (`video_mixes`,
  ordered by ID; missing from a pre-0.28 server, it parses as empty), the `buses` and
  `sends` (as `bus.list`; missing from a pre-0.19 server, both parse as empty), every
  hotkey binding (`hotkeys`, ordered by chord; missing from a pre-0.25 server, it
  parses as empty), every controller binding (`bindings`, ordered by trigger; omitted
  when there are none, so missing, also from a pre-0.96 server, parses as empty), the
  master stage (`master`, with its filter chains; omitted while
  it has none, so missing, also from a pre-0.29 server, parses as empty), the
  transition presets with the default (`transitions`; missing from a pre-0.30 server,
  it parses as the one initial `cut`), and the `folders` and `labels` in their orders
  (each omitted when empty, so it parses as empty from an older server). A client reads
  it on connecting, and again after a `state.changed` it didn't cause.
- `scene.list`: the scenes in display order, each with its name, `role` (omitted for a
  `scene`) and item count.
- `label.members {label_id}` (0.100,
  [ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)): everything that
  carries one label, as `LabelMembers` at the `rev` it was read: the label, its `scenes`
  in display order (`scene_id`, `name`, and `variant_of` for a keyframe scene), its
  `sources` by ID, its `hotkeys` by chord with their actions, and its `timelines` by
  scene then ID (`timeline_id`, `name`, `scene_id`), each list omitted when empty, so an
  agent lists a label's action group in one call. `not_found` for a label that doesn't
  exist.
- `source.list`: the sources, ordered by ID (sources have no display order), each with
  its name, `SourceKind`, settings, filters and, for an audio source, its `audio`.
- `bus.list`: the buses, ordered by ID (the master among them), and every send,
  ordered by bus and then by source, at a `rev` (0.19).
- `item.list {scene_id}`: one scene's items in stacking order, bottom first, or
  `not_found` for a missing scene.
- `output.list`: the outputs, ordered by ID (outputs have no display order), each with
  its `OutputKind`, `encoder_profile` (absent for the default), `video_mix` (absent
  for the program) and its own `filters` (omitted while empty; 0.29), at a `rev`.
- `encoder.list`: the encoder profiles, ordered by ID, at a `rev` (0.13).
- `video_mix.list`: the video mixes, ordered by ID, at a `rev` (0.28).
- `output.status {output_id}`: an output's runtime state and `key_set`, whether the
  credential store holds its key (missing from a pre-0.7 server, it parses as
  `false`). While it runs, `encoder` names its session's profile (absent for the
  default), the other running outputs that share the session (`shared_with`, by ID),
  whether the session is a hardware one, and whether it is `dedicated` to this output
  because the output has its own video chain (0.33; omitted when false), so it can
  never be shared; `encoder_sessions` and
  `hardware_sessions` count the video sessions open across every output, since
  hardware encoders cap them per GPU vendor and driver and the engine doesn't guess
  the cap (0.13; missing, they parse as absent and 0). `cleartext` says whether
  publishing sends the stream key across the network unencrypted: a generic RTMP
  target whose URL is plain `rtmp://` to a host other than `localhost` or a loopback
  address (`OutputKind::sends_key_in_cleartext`; 0.37, missing it parses as `false`).
  The engine still publishes; a client shows it as a warning. None of it is
  collection state, so it carries no `rev`.
- `kind.list` and `kind.describe {kind_id}`: the kinds the engine describes, above.
  Not collection state, so no `rev`.
- `studio.get`: the scene on program (`program_scene`) and on preview
  (`preview_scene`, 0.30), each omitted only while the collection has no scene. Runtime
  state, not collection state
  ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)), so no `rev`.
  Added in 0.16. Since 0.87 `timelines` lists the timelines playing, each with its
  scene, its `position_ms` and its `pass` ([Timelines](#timelines)), omitted while none
  plays. Since 0.88 `keyframes` lists each scene drawn through one of its keyframe
  scenes where it is placed ([Keyframe tracks](#keyframe-tracks)), ordered by scene,
  omitted while none is. Since 0.115 `warm_scenes` is the warm budget
  `studio.set_warm` sets.
- `capture.targets`: the displays, windows and (since 0.90) video capture devices the
  engine can capture now, displays first, then windows, then devices, each with its
  `kind` (`display`, `window` or `device`, the source kind that captures it: `display`,
  `window` or `webcam`), its `target` (the value for that source's `target` or
  `device` setting), a `name` to show
  (a window's is its title, or, on macOS, its application's name when it has no usable
  title, and a device's is its driver's; either is set by a third party, so it is third-party text, data
  and never instructions, though the field isn't `remote_`-prefixed as
  [ADR-0059](../../docs/adr/0059-mcp-annotations-and-remote-text.md) asks; see
  [ADR-0085](../../docs/adr/0085-capture-target-names-marked-as-remote-text.md)), its
  `size` in pixels when the OS reports one, and `primary` (omitted when `false`)
  for the display an empty `display` target captures. A device comes with `formats`
  (omitted when empty): each format it captures in that the engine reads, as
  `CaptureFormatInfo` (`width`, `height`, `fps` to two decimals, and `pixel_format`), in
  the device's order, and `controls` (omitted when empty, since 0.105): each control
  the engine can set on it, as `CameraControlInfo` (`name`, `min`, `max`, `step`,
  `default`, the `value` now, and `auto`, omitted when the device can't set it itself,
  else whether it does now). Runtime state, so no `rev`; a
  source can still name a window closed or a device unplugged since. `action_failed`
  when the engine has no capture backend, or the OS refuses both the displays and
  windows and the devices; when only one lists, the result holds that. Added in 0.27.
- `ptz.list`: the PTZ cameras this machine controls, each with its address, transport,
  whether it answered lately, its last pose and error, and its counters
  ([PTZ cameras](#ptz)); empty when the host listed none.
- `jtunnel.senders` (0.114, [ADR-0123](../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
  §3): whether the JTunnel receiver is `on`, and the senders that answered its mDNS
  query in the last 35 s, ordered by name, then address, each a `JtunnelSenderInfo`:
  `remote_name`, the tunnel name it announces (text from the LAN, so `remote_`), its
  `address` and port, and whether this machine has it `accepted` (that name at that
  address). Listing a sender trusts it with nothing. `on` false and no senders while
  the receiver is off, the default. Runtime state, so no `rev`. A plugin can't read it
  (the person's LAN addresses).
- `font.list`: the fonts installed on the engine's machine ([ADR-0092](../../docs/adr/0092-installed-fonts-query.md)), ordered by
  family, then style, then path, each a `FontInfo`: `remote_family` and
  `remote_style` from the font's name table (the font maker's text, so `remote_`), and
  the file's absolute `path`, which a text source's `font` names as an `asset` with
  root `absolute`. A collection lists its first font. The engine reads its OS's font
  folders on the first call and keeps the list, so a font installed later shows after
  a restart. Runtime state, so no `rev`. Added in 0.68.
- `profile.get {seconds?, top?}`: where the engine's time went over the last
  `seconds` (1 to 60, default 5), as a `ProfileInfo`
  ([ADR-0103](../../docs/adr/0103-per-item-performance-profile.md)): each drawing
  thread's (`program`, `preview`) frames, their average, 95th percentile and longest
  time, the frames over `frame_budget_ms`, the program's skipped ticks and the time per
  phase; the units of drawing work costliest first, each a `ProfileUnit` naming its
  owner (a source's upload or chain, a capture, a scene's chain or pass, an item's
  effects, a placed scene's chain, the composite, the master or an output's chain) with
  its frames, average and recent longest time and share of its drawer's time; the
  sources the `jstream-sources` thread and the planner spent time on; the plans; the
  process's CPU; and its threads by name, as a percentage of one core (JStream's own
  `name`, any other `remote_name`, third-party text). `top` (default 20, at most 200)
  cuts each list. Times are the drawing thread's, not the GPU's; `gpu_ms` and
  `gpu_timing` are reserved for GPU timestamps. Since 0.92 it also reports past the
  drawers. `streams` lists each running output's program feed and each H.264 preview
  stream (a `ProfileStream`: `output` with its `output_id`, or `preview` with its
  `channel` and the `scene_id` or `source_id` it shows) as a `StreamProfile`: its
  encoded `fps`, the video frames handed to its encoder (`frames_in`), taken by it
  (`encoded`), sent on (`frames_out`, to the muxer or publish target, or to the
  client) and `dropped` on the way, the frames its encoder took more than a frame
  period after they were handed over (`late`), the encoder's average time per frame
  (`encode_ms`), and the bit rate sent against the encoders' (`kbps`, `target_kbps`;
  video and audio for an output, video for a preview stream). Most frames lost come
  first; a stream that ran in none of the window, or has ended, is left out. `mixer`
  (omitted when it mixed no block in the window) counts the mixer's `blocks`, the
  `late_blocks` whose mix took longer than the audio they hold (`block_ms`), the longest
  mix (`block_max_ms`), the frames it skipped to catch up (`skipped_frames`), and the
  inputs' frames played as silence (`underrun_frames`) and dropped (`dropped_frames`).
  Since 0.103, `views` lists what the clients' views did with their preview pictures, as
  they reported it with `preview.report`
  ([ADR-0132](../../docs/adr/0132-bench-gates-views-and-quality-probe.md)): one
  `ViewProfile` per view a session reported in the window, with the session's `client`
  name and the `view` label (both third-party text), each `ViewCounts` count's growth
  over the window (from zero for a view, or a restarted view, the window's start
  doesn't have), and `received_fps` and `shown_fps`, in client then label order. A
  view whose session has ended stays while a window reaches back to its last report.
  Since 0.113, `network` (omitted while the JTunnel receiver is off, #1223) lists each
  accepted sender as a `NetworkInputProfile`, in the order the host gave them: its
  `sender` name, its stream's `sample_rate` (0 before anything arrived), whether a
  source is `recording` it, `latency_ms` (how far ahead of now its held audio reaches:
  the buffer's fill), `lead_ms` (the recording source's `latency_ms`), `drift_ppm` once
  enough stream has arrived, and its frames `played`, `concealed`, `late`, `repeats` and
  `restarts`; and the receiver's drops, `outside_lan`, `unaccepted`, `malformed` and
  `unsupported`, with the `midi` messages it handed on. Unlike the rest of the profile,
  these count from when the receiver started, not over the window.
  Runtime state, so no `rev`. Added in 0.73.
- `quality.probe {stream, frames?}`: how close a stream's decoded pictures are to the
  frames its encoder was given
  ([ADR-0145](../../docs/adr/0145-the-quality-probe-reads-back-the-compositor.md),
  refining [ADR-0132](../../docs/adr/0132-bench-gates-views-and-quality-probe.md) §3).
  `stream` is a `ProbeStream`, so far only `{"type": "output", "output_id"}`, a running
  output's encoder session. The engine copies the next `frames` (1 to 10, default 3)
  frames the render thread pushes to the session, from the next keyframe on, decodes
  the session's packets with the platform's decoder, and answers a `QualityProbeInfo`:
  the `encoder` (and whether it is `hardware`), the `decoder`, the encoded `width` and
  `height`, each frame's `FrameQuality` (`pts_us` on the engine's clock, `psnr_y` and
  `psnr` in dB up to `MAX_PSNR_DB`, 100, for identical pictures, and `ssim_y` from 0 to
  1), and their means. Both pictures are compared as NV12 in BT.709 limited range.
  Nothing is copied or decoded unless a probe waits, and one probe of a session runs at
  a time. Fails with `not_found` for an unknown output, `invalid_argument` on `frames`,
  and `action_failed` when the output isn't running, the platform has no decoder for
  its codec, another probe of the stream runs, or the frames don't come back within ten
  seconds. A plugin with the `read` class may call it. Runtime state, so no `rev`.
  Added in 0.110.
- `privacy.get`: deprecated
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)), removed in
  1.0. JStream has no privacy screen, so it always answers `on: false`. Added in 0.33.
- `source.sizes`: every source, ordered by ID, with its current `size` in pixels,
  omitted while the source has no video frame (it hasn't started, it is audio-only, or
  it failed). A source of a video kind with nothing to show, such as an `image` or
  `media` source with no file or a `media` file with no video stream, draws a 1×1
  transparent frame and reports that size. A source drawn at a density reports its own size, the size its frames are
  placed at, not their pixels
  ([ADR-0073](../../docs/adr/0073-sources-drawn-at-their-shown-size.md)). With
  `source.size_changed` it is what a client needs to draw a legacy placement's box, crop
  handles and a size in % of the source's own (ADR-0022 §4). Runtime state, so no
  `rev`. Added in 0.40.
- `activity.list {after_seq?, limit?}`: the engine's recent activity, newest first
  ([ADR-0083](../../docs/adr/0083-activity-log-over-the-protocol.md)): one
  `ActivityEntry` for each batch, undo, redo and action the audit log records (not
  `live.set` or `live.cancel`; a gesture that ends without a commit is its own entry),
  with its `seq` (1, 2, … per engine run), `time_ms`, `client_id`, `client_name`, `call`,
  `error` when it failed, and `summary`, one English sentence the engine writes from the
  collection just before the call ran ("Switched to “Desktop + Cam”", "Mic gain −1.5 →
  −3.0 dB"). The sentence names objects and gives numeric, on/off and enum values only,
  never settings, targets or keys, and is cut to 256 characters
  (`MAX_ACTIVITY_SUMMARY_CHARS`). The engine keeps the last 100 in memory; `limit`
  (default and most 100) and `after_seq` (only newer entries) narrow the answer, which
  also carries the engine's clock as `now_ms`. No event announces an entry yet: a client
  that shows the log asks again with `after_seq`. Runtime state, so no `rev`. WASM
  plugins can't call it. Added in 0.51.
- `device.list`: the devices paired with the listener this client reached, each with
  its `device_id`, `name`, and when it was paired and last used (Unix seconds). Not
  collection state, so no `rev`. Added in 0.21 (see *Pairing* below).
- `agent.approved`: whether the server asks before a new agent connects (`asking`), the
  approved list (`approved`, each `{client_name, transport}`) and the requests still
  open (`pending`, each `{approval_id, client_name, transport}`). Runtime state, so no
  `rev`. Added in 0.108 (see [Agent approval](#agent-approval)).
- `ui_settings.get {key}`: the UI setting kept under `key`, or no `value` when there is
  none. Not collection state, so no `rev`. Added in 0.52 (see *UI settings* below).
- `plugin.list`: the installed WASM automation plugins, ordered by ID. Not collection
  state, so no `rev`. Added in 0.58 (see *Plugins* below).
- `plugin.clap_scan`: the CLAP plugin scan, its folders, what it found and what failed.
  Runtime state, so no `rev`. Added in 0.95 (see *Plugins* below).
- `collection.info`: the project file the engine saves to (`path`) and up to ten
  `recent` ones, most recent first. Not collection state, so no `rev`. Added in 0.94
  (see *Projects* below).
- `codec.downloads`: the codec libraries the engine can download on a person's request,
  ordered by ID. Not collection state, so no `rev`. Added in 0.69 (see *Codec
  downloads* below).
- `delete.plan {target}`: the batch that deletes a scene, source or bus (`target` is
  `{"type":"scene","scene_id":…}`, `{"type":"source","source_id":…}` or, since 0.24,
  `{"type":"bus","bus_id":…}`), or a folder or label (`{"type":"folder","folder_id":…}`,
  `{"type":"label","label_id":…}`; their plans are described with folders and labels
  above), with everything that refers to it, as `edits` and the
  `rev` it was planned at, or `not_found` for a missing target (and
  `invalid_argument` on `target.bus_id` for the master, which is never deleted). In order, the edits remove the items that place a
  deleted source, set every settings reference to a deleted object to `null` (one
  `source.set_settings` per source, which keeps the source, and one
  `filter.set_settings` per filter), delete the `scene` sources
  that show a deleted scene, point every video mix that shows it at `program` (one
  `video_mix.update`, which keeps the mix), move every output that carries a deleted bus
  to the master (`output.set_audio_buses`, 0.59), and last delete the target. Deleting a
  scene deletes the
  `scene` sources that show it, and the cascade stops there, since deleting a source
  deletes no scene. A bus is held only by settings references, which the plan clears,
  and by the outputs that carry it, which the plan moves; its `bus.delete` takes its
  sends. A target nothing refers to plans just its own
  delete. `hotkeys` lists the bindings the batch removes with what they name, ordered
  by chord (0.25; omitted when there are none), and `bindings` the controller bindings,
  ordered by trigger (0.96; omitted when there are none); they need no edit of their
  own. The client
  shows the plan, then sends it as an ordinary batch with that `expected_rev`, so it is
  refused if the state moved on, and its undo is the batch's inverse.
- `video.snapshot`: the current program mix as a PNG, never a video mix (there is no
  per-mix snapshot yet). `png` is the file in standard
  base64 with padding and no line breaks. `width` and `height` are that image's size;
  the longest side is at most 1280. `rev` is the collection revision of the layout.
  Source pixels are runtime state and can change without a new `rev`. The query takes
  no arguments and changes nothing. On a machine with no graphics adapter, hardware or
  software, it fails with `action_failed`, and on Linux the reason says which driver to
  install.
  **Size.** The PNG is RGBA8 and nothing caps its byte size, so a noise-like frame,
  which PNG can't compress, can be as large as its raw pixels: the image is the
  canvas, scaled down so its longest side is at most 1280. The default 1920×1080
  canvas gives 1280×720, 3.7 MB raw, and `png` can reach about 4.9 MB of base64; a
  square canvas about 8.7 MB. A typical frame is far smaller. The 4 MiB limit `jstream-server` sets applies only to messages
  it receives, not to its answers
  ([jstream-server › Design](../jstream-server/README.md#design)), so a client must
  accept an answer of that size: `jstream-control`'s WebSocket client keeps
  tungstenite's default limits, which are well above it.
- **Frame sheets** (since 0.82, #1061). One picture can't show motion, so `video.frames
  {count, interval_ms, delay_ms?, scene_id?}` takes `count` pictures (2 to 16)
  `interval_ms` apart (at least 10), the first `delay_ms` after the call, of the
  program mix or of `scene_id`'s scene alone, and answers with one PNG: each picture
  scaled to fit 480 × 270 and tiled left to right, top to bottom, in time order, in a
  grid as square as it can be (`columns`, `rows`, `tile_width`, `tile_height`).
  `frames[i]` says when tile `i` was taken (`at_ms`, from the call, on the clock
  transitions run on) and its `rev`. The pictures are taken in real time, so the call
  takes at least `delay_ms + (count - 1) × interval_ms`, at most 10 s; each is what that
  mix or scene shows then, as `video.snapshot` draws it (a capture draws nothing). A
  scene off program shows what its preview stream would.
  `scene.switch_and_watch {scene_id, preset_id?, count, interval_ms, delay_ms?}` is
  `scene.switch` followed by a sheet of the program mix counted from the switch, in one
  call, so an agent sees a transition play: a stinger's cover at its `cut_ms`, or a
  600 ms move. It is an action (it switches), answering with an `ActionResult`
  ([ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md)), so the
  read-only `video` MCP tool stays read-only; it can't be bound to a hotkey, since its
  answer is for the session that sent it. Limits are checked before the switch. With no
  graphics adapter, a sheet fails as `video.snapshot` does.

**Pairing** (0.21, [ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md)).
`pairing.start` opens a one-time code and answers it in `result`
(`{"type":"pairing.start","code":"XXXX-XXXX","expires_in_secs":120}`). Since 0.97 a
server that also listens on the local network adds `network`, `{"urls":[…],
"fingerprint":"AB:CD:…"}`: the addresses a device opens and the SHA-256 fingerprint of
the listener's certificate, which a client shows so the person continues past the
browser's warning only when it matches
([ADR-0130](../../docs/adr/0130-browsers-keep-no-certificate-pin-v2.md)); an older client
ignores it. `device.list`
lists the paired devices, and `device.revoke {device_id}` forgets one (`not_found` with
entity `device` for an unknown one). They concern whom a listener admits, so the server
in front of the engine answers them
([ADR-0057](../../docs/adr/0057-server-answers-credential-calls.md),
[jstream-server › Design](../jstream-server/README.md#design)); a `Control` with no
server in front answers `unsupported`. The code is a `SecretString`: it never prints,
and nothing but this answer carries it ([R-SEC-001](../../docs/RULES.md#R-SEC-001)).

**UI settings** (0.52, [ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md)).
`ui_settings.set {key, value}` keeps any JSON value under a key, and a `null` or missing
`value` removes the key; `ui_settings.get {key}` reads it back. A UI owns each value's
shape: the web UI keeps its dock under `workspace.dock`, so the desktop app and every
browser of one engine show the same layout. A key is 1 to 64 of `a`-`z`, `0`-`9`, `.`,
`_` and `-`, a value at most 64 KiB as compact JSON, and at most 64 keys are kept
(`invalid_argument` on `key` or `value` otherwise). They are not collection state: no
`rev`, no undo, no event, and they outlive switching collections. The server in front of
the engine answers them and keeps them in its settings file, with pairing off too; a
`Control` with no server in front answers `unsupported`. `ui_settings.set` fails with
`action_failed` when the file can't be written, and the value is then kept until the
server stops.

**Projects** (0.94, [ADR-0121](../../docs/adr/0121-projects-open-new-and-save-as.md)).
A project is one collection file on the engine's machine. `collection.open {path}` loads
the file at `path` and saves there from then on; `collection.new {path}` creates an empty
one there; `collection.save_as {path}` writes the current collection there and saves
there from then on. New and save as never replace a file: anything at `path`, the open
file included, is `already_exists`, entity `file`. An open or a new is `action_failed`
while the open project can't be saved. A
path is absolute and follows the `absolute` asset path rules (`invalid_argument` on
`path` otherwise). A file that can't be read or loaded, such as one from a newer engine,
is `action_failed` with its reason, and the open project stays open. An open or a new
stops every running output, clears every client's undo history and idempotency keys,
cancels open gestures, stops timelines, puts the first scene on program, and moves `rev`
past the old collection's; every client gets `state.changed` (so an older client reads
the state again), `studio.changed` and `collection.file_changed {path, cause}`. A save as
to another folder rewrites asset references to keep naming the same files (a new `rev`
that clears undo when it rewrites any). Stream keys belong to one project path on the
engine's machine, and nothing in the file names where they are, so two projects sharing
an output ID never share a key. All
four answer `unsupported` when the host named no collection file, as the mock does. No
plugin is granted them and no hotkey binds them.

An item is reported with its `source_id`, `placement` form, `transform`, `geometry`,
`crop`, `visible`, `opacity`, `blend` and `transition`, and a source with its name, kind, settings and
filters: every property an
edit sets, so a client can compute any edit, or its inverse, from what it read
([Item geometry](#geometry) says what `transform` and `geometry` hold for each form). The shapes (`SourceInfo`, `SceneInfo`, `ItemInfo`) mirror `jstream-graph`'s model,
which produces them (`Graph::query`). As the model grows (settings, filters, effects)
they gain fields, which older clients ignore.

<a id="undo"></a>
## Undo
<!-- @section
covers = ["crates/jstream-protocol/src/history.rs"]
-->

Undo is per client ([ADR-0013](../../docs/adr/0013-command-model.md)): each applied batch
becomes one undo step on the history of the `client_id` that sent it. The protocol
doesn't carry inverses. `jstream-graph` computes each edit's inverse from the state
before it, and the per-client undo stacks live in `jstream-core`
([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)). What the protocol must
guarantee is that the edit set is **closed under inversion**: every edit's inverse is
itself a sequence of edits. Each `Edit` variant documents its inverse.

**The edit set is closed under inversion.** Every property of the state has an edit
that sets it: `scene.reorder` and `item.reorder` restore positions,
`item.set_visible` and `item.set_crop` restore what `item.add` doesn't take,
`item.set_opacity` and `item.set_blend` restore what they set,
`canvas.set_size` restores the canvas, and `item.set_geometry` and
`item.set_transform` each restore the previous placement in its own form. So undoing
`item.remove` is `item.add` (with `geometry` or `transform`, matching the removed
item's form) plus whichever of `item.reorder`, `item.set_crop`, `item.set_visible`,
`item.set_transition`, `item.set_locked`, `item.set_effects_space` and `item.set_motion`
the removed item needs, and undoing `scene.delete` is
`scene.create` with the scene's `role` (omitted for `scene`), `scene.reorder` unless it
was last, `scene.set_transitions` if it had an override, and each item's restore,
bottom to top. A variant's `scene.delete` is undone by `scene.create_variant`, its role
if it isn't `scene`, the same restores and the placement edits that write its
overrides back. Undoing `transition.delete` is `transition.create`, then a
`scene.set_transitions` for each override it cleared, and a `hotkey.set` for each
binding it removed. `item.move` and `scene.set_role` are each undone by the same
edit with the old scene and index, or the old role. Undoing
`source.delete` is `source.create` with the complete stored settings and its
`duplicated_from`, then `source.set_audio` and `send.set` (or `send.remove` of the
default master send) for whatever audio differs from the kind's defaults, then a
`filter.add` per filter; undoing a filter edit is its opposite (`filter.remove` and
`filter.add` with the filter's place, switch and settings) or the previous value,
undoing `item.set_source` is `item.set_source` back, and undoing
`source.set_settings` is a patch of the old values. Undoing `bus.delete` is
`bus.create`, `bus.set_fader` unless the fader was at its default, `bus.set_monitor` if
it had a monitor, and a `send.set` for each send the bus had; `source.set_audio`,
`bus.set_fader`, `bus.set_monitor` and `send.set` are each
undone by the same edit with the old values, and a new send by `send.remove`. Deleting a source that items still
place or a setting refers to, a scene that a `scene` source shows or a setting refers
to, or a bus a setting refers to, is rejected, so no edit removes more than the object
it names (a bus's sends are part of it), apart from the scene overrides a deleted preset
clears, the hotkey bindings a delete removes and the keyframe overrides an item's
removal or move drops, which its inverse restores. `jstream-graph`'s property
test checks that applying a random batch and then its inverse restores the state
exactly. A new edit that can't state its inverse in edits needs new edits first.

**Undo and redo requests** follow [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md):

```json
{"type":"request","id":"r4","call":{"kind":"history","op":{"type":"history.undo"},"expected_rev":15}}
{"type":"response","id":"r4","result":{"kind":"history","rev":16,"step_rev":13,"undo_depth":4,"redo_depth":1}}
```

- `history.undo` applies the inverse of the client's most recent step that isn't
  undone, and `history.redo` applies its most recently undone step again. Each is one
  `rev` bump and emits `state.changed` with `cause` `undo` or `redo`, but makes no new
  step: the step moves between the client's undo and redo stacks. The client's next
  batch clears its redo stack. Another client's batches never touch either stack.
- `expected_rev` and `idempotency_key` work as for a batch, and keys are shared with
  the client's batches. The engine checks in this order:
  1. **The key.** A key the client already used gets that call's stored result, whatever
     `expected_rev` says now, so a retried undo whose response was lost never undoes a
     second step. A batch's retry works the same way.
  2. **`expected_rev`.** A stale one fails with `rev_conflict` and leaves both stacks
     as they were. This is what ADR-0032 means by "checked first": no part of the
     history is read or changed before it passes.
  3. **The stack.** An empty one fails with `history_empty`.
- `rev` is the revision produced, `step_rev` the revision the step's batch originally
  produced (it names the step), and the depths say what can be undone and redone next.
- `undo_conflict` (`step_rev`, and the `client_id` and `rev` of the conflicting batch)
  refuses an undo or redo when another client's batch after it changed a property the
  step sets, created or deleted an object the step touches, touched an object the step
  creates or deletes, also reordered the scenes or also set the canvas size, or made its edits
  no longer apply. An item's transform and its geometry are one property, its
  placement, and which source it places (`item.set_source`) is another. Each audio
  field a `source.set_audio` or `bus.set_fader` sets is its own property, as is a bus's
  monitor, and a send is
  an object of its own, which uses its source and its bus. An
  edit touches the objects it creates, deletes or edits and the ones it places or
  names, such as an item's source. What the step touches is taken both from when it was recorded and from
  the edits it would apply now. Nothing is applied and the step is dropped, so the next
  undo reaches the step before it.
- A step whose edits fail for a reason no other client caused keeps its place and the
  call fails with the graph's own error. The usual case is a running output that blocks
  undoing its `output.create`, which fails with `invalid_argument` until the output
  stops.
- `history_empty` (`direction`) means there is nothing to undo or redo.
- Actions (outputs, stream keys, live values), queries and history calls are never
  steps; a gesture's step is its committing batch. The engine
  keeps 100 steps per client by default, in memory, for the session's lifetime.

<a id="live-values"></a>
## Live values
<!-- @section
covers = ["crates/jstream-protocol/src/live.rs"]
-->

Live values (0.26) keep a drag, a fader move or a live dialog to one batch and one undo
step ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)). A client opens a **gesture**,
named by a `gesture_id` it chooses, and sends its values as it goes:

```json
{"type":"request","id":"r7","call":{"kind":"action","action":{"type":"live.set","gesture_id":"drag_1","values":[{"type":"item.set_geometry","item_id":"itm_1","geometry":{"center":{"x":800,"y":450},"size":{"width":480,"height":270}}}]}}}
{"type":"response","id":"r7","result":{"kind":"action"}}
{"type":"request","id":"r9","call":{"kind":"batch","edits":[{"type":"item.set_geometry","item_id":"itm_1","geometry":{"center":{"x":820,"y":450},"size":{"width":480,"height":270}}}],"ends_gesture":"drag_1"}}
```

- `live.set {gesture_id, values}` opens the gesture if it is new and sets its values:
  setter edits, the same ones a batch carries. Each overrides one parameter on output
  from the next render tick, and replaces the gesture's earlier value for it. A live
  value is runtime state: it never changes `rev`, is never an undo step, is never saved,
  and `state.get` and `item.list` never show it. The live-adjustable setters are
  `item.set_transform`, `item.set_geometry` (one parameter, the placement) and
  `item.set_crop`, and since 0.67 `source.set_settings`, whose patch may name only
  the parameters its kind marks `live` (anything else is `values.<n>.patch.<param>`):
  the engine draws the collection with those values applied, rebuilding the render
  snapshot, and each parameter of each source is a parameter of its own
  ([ADR-0091](../../docs/adr/0091-source-settings-as-live-values.md)). Since 0.70
  `scene.set_camera` is one too, one parameter per scene, so an orbit drag moves a 3D
  scene's camera every frame and commits once
  ([ADR-0094](../../docs/adr/0094-scene-cameras.md)). Audio faders
  follow as their real-time path exists. Values are checked as a batch's edits are, named
  `values.<n>.<field>`. Any other edit (`values.<n>.type`), an empty list, more than 256
  values, or a 17th open gesture of the client is `invalid_argument`, and nothing in the
  message is applied. A value whose `scene_id` names a keyframe scene (refused with
  `values.<n>.scene_id` from 0.39 to 0.52) is, since 0.53, a parameter of that keyframe's, apart
  from the item's own, so a gesture on each can be open at once. It shows only where the
  keyframe draws the item. Where the keyframe draws it, the item's own live value shows
  only in a field the keyframe doesn't override, as an edit to the parent would
  (ADR-0049 §1). Its commit is a batch that writes the override, and deleting the
  keyframe drops it. The engine never refuses a `live.set` for arriving fast; a client
  keeps one per gesture in flight and sends its newest values when the answer comes.
- **Commit:** the client sends one ordinary batch with the final values and
  `ends_gesture`. It is one `rev`, one undo step whose inverse restores the values from
  before the gesture, and one audit entry. The gesture ends with it. A batch whose
  `ends_gesture` names no gesture the client has open is `invalid_argument` (field
  `ends_gesture`), and a batch that fails leaves the gesture open.
- **Cancel:** `live.cancel {gesture_id}` ends the gesture without a commit; it succeeds
  when there is nothing to cancel, so a retry is safe. A gesture also ends,
  `expired`, when its client's session ends.
- **Two gestures, one parameter:** a parameter is held by one gesture at a time. A
  `live.set` naming a parameter another gesture holds takes it over (last writer
  wins); the other keeps the rest. A gesture left holding nothing stays open, and
  counts toward the client's 16, until the client commits it, cancels it or
  disconnects, so its `ends_gesture` still works. A batch that changes a held parameter changes the
  collection, but the live value still shows until the gesture ends. A batch that
  deletes an item drops its live values.
- `live.list` returns every open gesture, ordered by client and gesture ID, with the
  values each holds now, for a client that connects during one. It carries no `rev`.

`live.set` and `live.cancel` are actions: never undoable, never batchable, answering
`{"kind":"action"}`.

<a id="events"></a>
## Events
<!-- @section
covers = ["crates/jstream-protocol/src/event.rs"]
-->

After `welcome`, the server pushes every event to every client.

- `state.changed {rev, client_id, request_id?, cause?}` follows every applied batch,
  undo and redo; `cause` is `undo` or `redo` for a history call (0.10). A client
  matches it to its own response by `request_id`; there is no ordering promise between
  the two. Every other client re-reads what it shows. Since each batch adds exactly one
  to `rev`, a gap shows that an event was missed.
- `output.state_changed {output_id, state}` reports runtime state, which is not part of
  the collection, so it carries no `rev`.
- `studio.changed {program_scene?, preview_scene?, transition?, client_id, request_id?}`
  reports a new scene on program or preview, with the request that caused it: a
  `scene.switch`, a `studio.set_preview`, or a batch, undo or redo that created the
  collection's first scene (it goes on program and preview) or deleted the program or
  preview scene (it moves to the first scene left, or to none). For edits it follows
  their `state.changed`. A switch, or setting preview, to the scene already there emits
  nothing. After a switch, `transition` names the preset it ran and how long the
  transition lasts in milliseconds (`duration_ms`, 0 for a cut), so a client can show
  progress; no event marks the end. Program and preview are runtime state, so it
  carries no `rev` ([ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md)).
  Added in 0.16; `preview_scene` and `transition` in 0.30.
- `live.changed {gesture_id, client_id, values}` carries the newest value of each
  parameter a gesture changed since its last one. The engine coalesces it per gesture
  to at most 30 a second, so a slow client gets fewer, newer values; the values a
  gesture had not sent yet always go out before it ends. Until
  [open question 7](#open-questions) every client receives it.
- `live.ended {gesture_id, client_id, outcome, rev?}` says a gesture ended: `committed`
  with the committing batch's `rev`, whose `state.changed` comes first, `cancelled` or
  `expired`. A client that doesn't know these events ignores them and shows committed
  state only.
- `privacy.changed {on, client_id, request_id?}`: deprecated
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)), removed in
  1.0. JStream has no privacy screen, so the engine never sends it. Added in 0.33.
- `source.size_changed {source_id, size?}` says a source's size changed: it became
  known (the first frame), changed, or became unknown (`size` omitted: the source
  stopped, failed or was deleted; a deletion's comes after its `state.changed`). The
  size is the one `source.sizes` reports, which a client reads once and then follows
  with these events. Runtime state, so no `rev`
  ([ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md) §4). Added in 0.40.
- `events.dropped {count}` says that `count` of this connection's events were dropped
  because the client read too slowly. A WebSocket server keeps a bounded event buffer
  per connection; when it overflows, it drops the queued events and sends this one
  notice instead, adding later drops to it until it is sent
  ([jstream-server › Design](../jstream-server/README.md#design)). Answers are never
  dropped. The client re-reads what it shows with `state.get`, which carries `rev`.
  Added in 0.4.
- `stats.tick {seq, drops, load, chains, outputs}` is one sample of the pipeline's drop counters
  ([R-RT-004](../../docs/RULES.md#R-RT-004)). `drops` is a running total since the
  engine started, not a delta, so a client that misses a tick subtracts the last one
  it did see. The fields are `capture_frames` (frames a capture mailbox replaced
  before they were read), `audio_frames` (input-ring overruns, audio that arrived
  after the mixer had passed it, and frames the mixer skipped because it fell behind
  the clock), `render_ticks` (ticks the render loop skipped to catch up with the media
  clock), `encode_frames` and `encode_blocks` (video frames and audio blocks
  dropped before an encoder accepted them) and `output_packets` (encoded packets
  dropped handing a stream to an output). Silence inserted to cover a gap is not a
  drop. A counter missing from `drops` parses as 0 (`DropCounts` is
  `#[serde(default)]`), so a counter added later is an optional field (minor). `seq`
  starts at 1 and increases by one each sample. While an output is
  running, the pipeline's `jstream-stats` thread emits a tick about once a second.
  While none runs, an engine emits none unless its host started the heartbeat, which
  the daemon and the desktop app do (0.48): it then ticks about once a second as well.
  Since 0.48 a tick carries `load` (`EngineLoad`), running totals beside the engine
  time they were read at: `elapsed_ms` since the engine started, `program_frames` the
  program stage has drawn (it grows only while an output runs), `cpu_ms` of CPU time
  the engine's process has used on every thread (left out where the host can't read
  it; the daemon reads it on Windows and macOS) and `cpus`, the logical CPUs. A client
  takes two ticks for a rate: frames over the elapsed time for the frame rate, and CPU
  time over the elapsed time times `cpus` for the share of every CPU. An older engine
  leaves `load` out, and a missing field in it parses as 0 or absent. A render tick that falls a whole period or more behind
  the clock is skipped rather than drawn late, and counted in `render_ticks`. A tick
  also carries `chains` (`ChainCost`), what scene filter chains and item effects add
  to each frame as the current render plan schedules them
  ([ADR-0065](../../docs/adr/0065-scene-chains-and-item-effects.md) §7): a gauge, not
  a running total. `scene_chains` counts the scenes whose video chain runs, and
  `scene_layers` and `scene_passes` their offscreen layers and GPU passes (each scene's
  canvas-sized composite and one per enabled filter); `item_effects` counts the items
  whose effects run, once for each scene that draws the item, and `item_layers` and
  `item_passes` theirs (each item's box layer and one per enabled effect);
  `layer_bytes` is the texture memory of all those layers (`Rgba16Float`, 8 bytes a
  pixel). A filter of a kind the engine doesn't draw is bypassed and not counted, nor
  are a source's own chain, a `scene_mask`'s scene, and the canvas-sized layer a
  nested scene without a chain is drawn into for an item with effects that shows it
  (it takes the place of the layer that scene would be drawn into anyway). Since 0.66
  `chains` also counts the output stage
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)), for the
  running outputs only, as it runs only for an output's frame, and as each one's frames
  are drawn since its `output.start` (a video filter or mix set on a running output
  counts from its next start, and a virtual camera is drawn as the program):
  `stage_chains` counts the master video chain once for each mix a running output gets
  through it, and the own video chain of each running output that started with one; `stage_layers` and `stage_passes` count each
  staged mix's canvas-sized layer and the pass compositing the mix into it, once
  however many outputs get the mix, and one per enabled filter of those chains; their
  bytes are in `layer_bytes`. A mix with neither a master chain nor an output's own is
  drawn straight to the output and adds nothing, and the pass fitting the result into
  each output's frame, which every output frame draws, is not counted. An older engine leaves `chains`
  out, and a count missing from it parses as 0. `chains` was added in 0.54. Since 0.57
  a tick also lists each running output in `outputs` (`OutputStatsInfo`, ordered by
  ID; left out while none runs, and on an engine with no media backends): `uptime_ms`
  since the engine received its `output.start` (opening the destination and
  starting the pipeline included), `bytes` and `frames` (the encoded bytes and video
  frames it handed its muxer this run while its link was up), `kbps` over the
  interval since the engine's previous sample, `dropped_frames` (video frames its
  encoder dropped since it started, plus encoded frames it fell behind on; a shared
  encoder session counts for each output on it), `dropped_packets` (audio packets it
  fell behind on, packets the muxer refused or a stop's drain gave up on, and what the
  publish target dropped while its link was down) and `health`: `waiting` until it has
  sent a video frame, `good` with nothing lost in the last ten seconds, `fair` with
  something lost there but under 2% of its video frames, and `poor` at 2% or more or
  while it is `reconnecting`. A health this version doesn't know parses as `other`. The
  counts are running totals for the run and start at 0 on each start; `kbps` and
  `health` come from the engine's own samples, so a missed tick doesn't change them. A
  virtual camera output is not listed yet. A field missing from an entry parses as 0
  (or `waiting`). Until
  [open question 7](#open-questions) every client receives it. Added in 0.6. A late plugin block is an audio drop in
  `audio_frames` once the mixer counts it
  ([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)); that host is not
  built. Render and encode times and GPU load wait until those meters exist, as later
  optional fields. Audience figures are `youtube.stats`
  ([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md)).
- `meters.tick {sources, buses}` is one reading of the audio levels (0.19), about ten
  a second while the mixer runs; an idle engine emits none. Each routed source and
  each bus has `peak` (the largest absolute sample since the previous tick) and `rms`
  (over the latest mixer block), linear full scale per channel, left then right. The
  master bus also has `true_peak` (BS.1770, since the previous tick) and `loudness`:
  `momentary`, `short_term` and `integrated` LUFS, each absent until there is enough
  audio above the absolute gate. Each tick is a full reading, so a client that misses
  one loses nothing; nothing numbers them.
- `youtube.status_changed {auth}` and `youtube.health_changed {output_id, broadcast_id,
  stream_id, status, health, issues}` (0.23), and `youtube.stats {output_id,
  broadcast_id, concurrent_viewers?, likes?, views?}` (0.38), are described under
  [YouTube](#youtube).
- `plugin.state_changed {plugin_id, state, reason?, remote_name?}` (0.35) is described
  under [Plugins](#plugins).
- `gpu.device_lost {cause, blamed, quarantined, recovering}` (0.61) and
  `gpu.effect_slow {kind_id, took_us, limit_us, quarantined}` (0.102) are described under
  [Effect quarantine](#quarantine).
- `timeline.changed {timeline_id, scene_id, change, playing, position_ms, pass}` and
  `timeline.cue_fired {timeline_id, cue, pass, at_ms, error?}` (0.87) are described
  under [Timelines](#timelines). Neither is sent once a frame: a cue fires once a pass.
- `scene.keyframe_changed {scene_id, keyframe_id?, client_id, request_id?}` (0.88) is
  described under [Keyframe tracks](#keyframe-tracks).
- `kinds.changed {cause}` (0.107, #1328) says the kinds `kind.list` and `kind.describe`
  answer changed without an edit. `cause` is `clap_scan` (a CLAP scan found a new
  effect, or one described differently; a rescan that finds the same ones sends
  nothing) or `effect_packs` (a pack added, changed or removed, or an effect quarantined
  or re-enabled). Runtime state, so no `rev`: a client that keeps the kinds reads
  `kind.list` again, and one that misses it sees the change on its next read.
- `jtunnel.senders_changed {}` (0.114, ADR-0123 §3) says the senders `jtunnel.senders`
  lists changed: one answered for the first time, said goodbye, or went unheard for
  35 s. A sender answering again sends nothing. Runtime state, so no `rev`: a client
  that shows them reads `jtunnel.senders` again.

<a id="youtube"></a>
## YouTube
<!-- @section
covers = ["crates/jstream-protocol/src/youtube.rs"]
-->

JStream signs in to one Google account, once, with the full `youtube` scope
([ADR-0058](../../docs/adr/0058-youtube-sign-in-with-a-jstream-client.md), keeping ADR-0041 §4), and every
YouTube command is an action or a query (0.23). They are for the UI: the MCP server
offers none of them, refuses a `youtube` output target and drops YouTube events, since
YouTube is for people, not agents (ADR-0058 §3):

- `youtube.sign_in` returns `{"type":"youtube.sign_in","url":…}`, Google's consent
  page. Once the user agrees, Google redirects the browser to a listener the engine
  opened on `127.0.0.1`, and the engine finishes the sign-in itself. No token ever
  travels in the protocol ([R-SEC-001](../../docs/RULES.md#R-SEC-001)): the refresh
  token goes to the OS credential store. Sent again while a sign-in waits, it returns
  the same URL.
- `youtube.sign_out` revokes the token and forgets it.
- `youtube.status` reports `auth`: `unavailable` (the engine has no Google OAuth client, as
  in a build whose built-in client is still empty, so YouTube's API features are off
  and clients hide them),
  `signed_out`, `signing_in` or `signed_in`. `youtube.status_changed {auth}` reports
  each change, including Google refusing the account's token.
- `youtube.list_broadcasts {status?, broadcast_type?, page_token?}` and
  `youtube.list_streams {page_token?}` read pages of up to 50. Both default to
  everything, the persistent "Stream now" broadcast included. A stream's key is never
  reported.
- `youtube.create_broadcast`, `youtube.bind_broadcast` and
  `youtube.transition_broadcast` return the broadcast as YouTube left it.
  `bind_broadcast` without a `stream_id` creates a reusable stream first.
- An output with the `youtube {broadcast_id}` target publishes to the `rtmps://`
  ingest of its broadcast's bound stream. While it runs, the engine polls the stream's
  health and sends `youtube.health_changed` when it changes.
- While it runs, the engine also reads the broadcast's audience about once a minute
  (ADR-0041 §3) and sends `youtube.stats {output_id, broadcast_id,
  concurrent_viewers?, likes?, views?}` when a reading differs from the last. A count
  YouTube doesn't report (hidden by the owner, no viewers now, or the broadcast ended)
  is omitted. The chat rate ADR-0041 §3 also names comes with the engine's chat
  polling, as a later optional field.
- `youtube.stats` (the query, 0.38) returns `outputs`, the last reading of each running
  `youtube` output that has one, by output ID, and `quota {used, daily, exhausted?,
  stats}`: the engine's count of the day's API units, the units it assumes a day has,
  whether YouTube has refused a call for quota since the day began, and how audience
  stats are polled (`normal`, `slowed` once less than a fifth of the day is left, or
  `stopped` below a tenth or once exhausted) (ADR-0041 §5). It never calls YouTube.
  The count is an estimate at YouTube's published costs and starts again at midnight
  Pacific Time; YouTube's own count, shared with anything else on the same Google
  project, can differ.
- A Live API failure is `action_failed`, queries included, and a used-up quota says so
  in its reason (ADR-0041 §5, kept by ADR-0058).

Values mirrored from YouTube (`life_cycle`, `privacy`, `status`, `health`, `severity`)
have an `other` value, so a value YouTube adds later still parses. Text from YouTube is
untrusted and shown as plain text. Chat and the featured message come later.

<a id="plugins"></a>
## Plugins
<!-- @section
covers = ["crates/jstream-protocol/src/plugin.rs"]
-->

WASM automation plugins (0.35, [ADR-0069](../../docs/adr/0069-wasm-plugin-limits-and-capabilities.md)) call the API through
a scoped `Control`, which checks every call against the plugin's grant before it reaches
the engine. This crate holds the wire half; the host is `jstream-plugin-wasm`.

- **Classes.** A person grants a plugin classes: `read` (the queries; always granted),
  `edit` (scene, item, source, filter, transition, bus, send, canvas, hotkey, binding and studio
  edits, among them a scene's folder, metadata and transitions, a source's metadata, a
  hotkey binding's labels (ADR-0133), an
  item's transition settings, the `transition.*` presets and `studio.set_preview`,
  a camera's motion and follow camera, a scene's PTZ shots (ADR-0146), `live.set` and `live.cancel`, and `undo`/`redo` on
  the plugin's own session), `live` (`scene.switch`, `scene.set_keyframe` (ADR-0116),
  `scene.zoom_camera`, which
  changes what is on air with no undo step, ADR-0113, and `source.restart`, ADR-0115) and `outputs` (`output.start`, `output.stop`). Everything else is
  never granted: the `ptz.*` actions and `ptz.list` (ADR-0146: a plugin moves no
  physical camera), `jtunnel.senders` (the LAN's addresses), creating, deleting and retargeting outputs, their encoders and stream
  keys, video mixes, `video.snapshot`, `video.frames`, `scene.switch_and_watch` and
  previews (a capture's pixels could leave through a granted host), `audio.reset_loudness`, `studio.set_warm` (an application setting), folders and labels themselves
  (`folder.*`, `label.*`), which ADR-0069 names in no class, the deprecated
  `privacy.set`, `youtube.*`, pairing and devices, `plugin.*` itself, and the
  `timeline.*` edits and actions, whose cues run actions the `edit` class can't
  (ADR-0115).
  `PLUGIN_SCOPES` places every command; a command it doesn't name is "never".
  `every_command_has_a_plugin_scope` fails until a new command is placed, so a new
  command can't reach plugins unreviewed.
- **Checking a call.** `plugin_classes` collects the class of every command a call
  carries: each edit of a batch, each edit a `live.set` carries, and for `hotkey.set`
  and `binding.set` the class of the action it binds, since a person's key press or
  pad runs it: with `edit`
  alone a plugin can't bind `output.start`. A call needing a class outside the grant,
  naming a never command, or an empty batch is `forbidden` (`PluginGrant::check`), with
  the call's first command as `call`.
- **Reach.** A class never widens network or filesystem reach (ADR-0069 S2): a
  `browser` source's URL and a media or image source's file need the grant's `hosts` or
  `folders` beside `edit`. `PluginGrant::check` reads the settings of `source.create`,
  `source.set_settings`, `filter.add`, `filter.set_settings`, `transition.create` and
  `transition.update` (in a batch or a `live.set`) with `settings_reach`, which needs no
  kind descriptor:
  - an asset reference under the `absolute` root must be inside a granted folder;
  - a string that starts as `scheme://` (after trimming ASCII spaces, control
    characters and Unicode white space from both ends, and dropping tabs and newlines,
    which covers what both a URL parser and the engine's `str::trim` remove) must be
    an `http`, `https`, `ws` or `wss` URL whose `scheme://host:port` (default port
    filled in, host compared without case) is granted. Text with an address later in
    it, such as a caption that ends with a link, is no URL and reaches nothing.
  Anything neither rule lets through is `forbidden`: a file under the collection's or the
  app's folder, another scheme, credentials in the URL, and a path with a `.` or `..`
  segment. A grant's folders can't have one either. A value the engine would resolve to
  a host without `://` (a `youtube_chat` video ID) isn't found, and the host's own
  network and filesystem checks still apply. Both path forms pass `problem`, since the
  client may not share the engine's OS, so an engine also refuses a folder its OS
  doesn't read as absolute (`/data` on Windows).
- **`plugin.install {plugin_id, path, grant}`**, **`plugin.grant {plugin_id, grant}`**
  and **`plugin.enable {plugin_id}`** are actions for a person: no MCP tool carries
  them, no plugin is granted them and no hotkey binds them. `PluginGrant` is
  `{classes, hosts, folders}`, each omitted while empty: hosts as `scheme://host:port`
  (`http`, `https`, `ws`, `wss`), folders as absolute paths on the engine's machine. A
  grant `problem` refuses is `invalid_argument` on `grant.hosts.N` or `grant.folders.N`,
  and so is a grant that gives more than the package's manifest asks for, on
  `grant.classes.N`, `grant.hosts.N` or `grant.folders.0` (R-SEC-003). A
  `plugin.grant` acknowledges the request `plugin.list` last showed the client that
  sends it (what the manifest asks for now, for a client that never listed it), so a
  plugin disabled as `needs_grant` can be re-enabled after it. An update loaded after
  that list isn't acknowledged, so one that asks for more still waits as `needs_grant`
  ([ADR-0090](../../docs/adr/0090-a-grant-acknowledges-the-request-its-client-saw.md)).
  An unknown plugin is `not_found` with entity
  `plugin`; an engine that can't run plugins
  answers `plugin.install` with `unsupported`, as the mock and an engine built without
  its `wasm-plugins` feature do.
- **`plugin.list`** lists the installed plugins for a person, ordered by ID, each a
  `PluginInfo`: `plugin_id`, `state` and `reason` as `plugin.state_changed` gives them,
  `remote_name`, `path` (its package, as installed), `request` (what its manifest asks
  for, a `PluginRequest`: `{classes, hosts, folders}`, with `folders` true when it wants
  folders a person picks) and `grant`. For a plugin disabled as `needs_grant`, `request`
  is the update's, which a `plugin.grant` from that client then acknowledges. A plugin
  being enabled is listed
  as it was until it loads. Like the actions, it is a person's: no MCP tool carries it
  and no plugin is granted it. An engine that runs no plugins, the mock among them,
  lists none.
- **`plugin.state_changed {plugin_id, state, reason?, remote_name?}`** reports a plugin
  `enabled` or `disabled`. The engine disables a plugin that runs past a limit or traps
  (`reason`: `cpu_limit`, `memory_limit`, `trap`), or whose update asks for more than
  its grant (`needs_grant`), and never re-enables one by itself. `remote_name` is the
  name the plugin's manifest gives itself: the plugin's text, never an instruction
  ([ADR-0059](../../docs/adr/0059-mcp-annotations-and-remote-text.md)), made plain text
  by the engine (control and bidirectional formatting characters as spaces, trimmed, at
  most 128 characters), and absent when nothing is left.

CLAP audio plugins ([ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md)) are
not WASM plugins: the engine scans for `.clap` files and offers each effect as a
`clap/<plugin id>` audio filter kind. Since 0.106 a person can see and redo that scan:
- **`plugin.clap_scan`** answers a `ClapScanInfo`: `available` (whether this engine
  hosts CLAP plugins, with a `reason` when it doesn't and knows why), `scanning`,
  `scanned` (whether a scan has finished yet), `folders` (each folder searched, in
  CLAP's order, as `{path, exists}`, so a client can say where to install one), `found`
  (each effect as `{kind_id, name, file}`, ordered by kind ID; `name` is the plugin's
  own text) and `failed` (each file the scan couldn't read, as `{file, reason}`).
- **`plugin.clap_rescan`** scans again and answers once the scan ends; a scan already
  running is waited for first. A kind found earlier stays until a restart. When a scan
  finds a new kind, every client gets `kinds.changed` (0.107, [Events](#events)). An engine
  that hosts no CLAP plugins, the mock among them, answers it `unsupported`.
- Both are a person's: no MCP tool carries them, no plugin is granted them (they name
  files on the machine) and no hotkey binds the rescan.

<a id="agent-approval"></a>
## Agent approval
<!-- @section
covers = ["crates/jstream-protocol/src/agent.rs"]
-->

Since 0.108 the desktop app with MCP on asks the person before a new agent's first
connection ([ADR-0131](../../docs/adr/0131-ask-before-a-new-agent-connects.md)). The
server, not the engine, answers all of this; a `Control` with no server in front
answers `unsupported`.
- A connection that presents the control token from the token file, and whose `hello`
  names a client the person hasn't approved over that `AgentTransport` (`websocket`,
  which `jstream-cli mcp` uses too, or `mcp_http`), is held: no `welcome` yet. Every
  admitted client gets **`agent.approval_requested {approval}`**, an `AgentApproval`
  `{approval_id, client_name, transport}`. `client_name` is the client's own text.
- **`agent.approve {approval_id, decision}`** answers it: `allow_once`, `always_allow`
  (which adds `{client_name, transport}` to the approved list) or `deny`. Any admitted
  client may answer; MCP agents get no tool for it. A request that isn't open is
  `not_found` (entity `approval`).
- With no answer in 2 minutes the `hello` fails as a denied one does: **`not_allowed`**,
  with a `reason`, and the server closes the connection. A client that connects again
  asks again. A `hello` sent again on a held connection waits on the same request.
- **`agent.approval_resolved {approval_id, outcome}`** tells every client how it ended:
  `allowed_once`, `always_allowed`, `denied`, `timed_out` or `closed` (the client went
  away first).
- **`agent.approved`** lists the approved list and the open requests, and
  **`agent.forget {client_name, transport}`** removes a row (`not_found` when the list
  doesn't hold it); the client's next connection asks again.
- None of these can be bound to a hotkey or granted to a WASM plugin.

It is a notice with a veto, not a control: a client can pass by naming itself after an
approved one, and an approved client is trusted no differently from any other token
holder.

<a id="quarantine"></a>
## Effect quarantine
<!-- @section
covers = ["crates/jstream-protocol/src/pack.rs"]
-->

A WGSL effect pack is validated, not bounded: a shader that loops too long can hang the
GPU, and the OS resets the device (#349,
[ADR-0087](../../docs/adr/0087-effect-quarantine-after-gpu-device-loss.md)). Since 0.61:
- **`gpu.device_lost {cause, blamed, quarantined, recovering}`** reports a lost device
  on the program stage. `blamed` lists the effect-pack kinds drawn in the frames before
  the loss (empty when none was, and the loss had another cause), `quarantined` the
  kinds the loss turned off: the blamed ones, or every kind of a pack blamed for a
  second loss in one run. `recovering` is `false` when the engine stopped opening new
  devices and the outputs fail. Runtime state, with no `rev`; agents see it.
- **`gpu.effect_slow {kind_id, took_us, limit_us, quarantined}`** (0.102) turns a kind
  off before it hangs the GPU: where the program stage's GPU has timestamp queries,
  each pack kind's pass is timed, and one that takes more than half the frame budget
  (`limit_us`) in 30 frames running is quarantined as a blamed kind is. `took_us` is
  its last such time; `quarantined` is the kind, or its whole pack when the pack was
  blamed before in the run (by either event). Runtime state, with no `rev`; agents
  see it.
- **`KindSummary.quarantined`**, in `kind.list`, marks a quarantined kind (omitted
  when `false`, so an older server's list reads as none). A quarantined kind's filters
  are bypassed, as a filter whose pack isn't loaded is, until a person re-enables it.
  The quarantine lasts across restarts and isn't lifted by editing the pack.
- **`effect.enable {kind_id}`** lifts a kind's quarantine, and its pack's. It succeeds
  for a kind that isn't quarantined, and is `invalid_argument` on `kind_id` for one no
  loaded pack has and no quarantine names. It is for a person, like `plugin.enable`: no
  MCP tool carries it, no plugin is granted it and no hotkey binds it.

Since 0.72 a client can write the packs themselves
([ADR-0097](../../docs/adr/0097-agents-write-effect-packs.md), `pack.rs`). These are
actions and queries, not edits: undo doesn't reach the disk, and there is no `rev`.
- **`pack.write {pack, files}`** replaces pack `pack` whole, or adds it, and loads it
  at once; it answers `ActionResult::PackWrite` with the pack's `PackInfo`. Each
  `PackFile` is a `path` and either `text` (`effect.toml`, `.wgsl`) or `base64` (`.png`,
  `.jpg`, `.jpeg`, standard with padding). `files` over 3 MiB as sent
  (`MAX_PACK_WRITE_BYTES`, so the request stays under ADR-0074's limit) is `too_large`
  before it is decoded; more than 64 files (`MAX_PACK_FILES`), an image over 2 MiB
  (`MAX_PACK_IMAGE_BYTES`), a bad pack ID or path, a pack another folder holds, a
  folder `pack` that holds something else, and a pack that doesn't load are
  `invalid_argument`, and the old version stays.
- **`pack.delete {pack}`** removes the folder whose `effect.toml` names the pack, or is
  `not_found` with `EntityKind::Pack`.
- **`pack.list`** answers each pack's `PackInfo`: its `id`, `folder`, `kinds` while it
  loads, `loaded`, and `written_by`, the client name of the last `pack.write` of that
  pack ID (omitted when it gave none).
- **`pack.read {pack, path?}`** answers `PackFileInfo`s: each file's `path`, `size` and
  BLAKE3 `digest`, the `text` of text files (up to 3 MiB of text in all; a text file
  over that read by `path` is `too_large`), and with `path` an image's `base64`.
- Agents may call all four. A plugin may call `pack.list` and `pack.read` (class
  `read`) and never the writes, and no hotkey binds a pack action.

<a id="codec-downloads"></a>
## Codec downloads
<!-- @section
covers = ["crates/jstream-protocol/src/encoder.rs"]
-->

Cisco's patent licence for H.264 covers only Cisco's own OpenH264 binary, downloaded
straight to the user's machine, so on Linux the engine downloads it from Cisco when a
person asks, and never ships it
([ADR-0140](../../docs/adr/0140-source-types-and-decoding-scope-v3.md)). Since 0.69:
- **`codec.downloads`** lists what the engine can download, ordered by ID, each a
  `CodecDownloadInfo`: its `download_id` (`openh264`), `name`, the `codec` it encodes,
  the pinned `version`, the `notice` and `license_url` a client shows wherever it offers
  it, the licence's full `license_text`, which the client reproduces there too, whether it is `installed`, and whether the machine has a `hardware_encoder` for
  its codec, so a client can tell when an output needs it. An engine whose platform
  downloads nothing (Windows, macOS), or that has no codec backend, lists none.
- **`codec.download {download_id}`** downloads it from the maker's server, checks it
  against the engine's pin and puts it in place, answering once that is done; outputs
  started afterwards can encode with it. A download that fails or doesn't match the pin
  is `action_failed` with the reason, and nothing is kept.
- **`codec.delete {download_id}`** deletes it again; a running output keeps its encoder.
- Both are `invalid_argument` on `download_id` for an ID the list doesn't hold. All
  three are for a person, like `plugin.install`: no MCP tool carries them, no plugin is
  granted them and no hotkey binds the actions.

<a id="shared-values"></a>
## Shared values
<!-- @section
covers = ["crates/jstream-protocol/src/values.rs"]
-->

Since 0.78 a client can keep **shared values**: named JSON values the engine holds for
browser source pages and agents to read
([ADR-0102](../../docs/adr/0102-page-api-for-browser-sources.md) decision 7, `values.rs`).
They are not collection state: never saved, no `rev`, no undo step. They are not
ADR-0032's live values either, which are a gesture's uncommitted edits.
- **`values.set {name, value?}`** is an action. It sets `name` to `value`, or removes it
  when `value` is `null` or left out; inside a value `null` is ordinary JSON. A name is
  dotted segments of `a`-`z`, `0`-`9` and `_`, at most 64 bytes (`check_value_name`); a
  value is at most 16 KiB as compact JSON (`check_value_size`, `MAX_VALUE_BYTES`); the
  store holds at most 256 names (`MAX_VALUES`). Each limit is `invalid_argument` on
  `name` or `value`, and nothing changes.
- **`values.changed {name, value?}`** goes to every client when a set changes something,
  with no `value` for a removal. Setting the value a name already has sends nothing.
- **`values.list {prefix?}`** answers `SharedValue`s ordered by name: all, or the name
  `prefix` and those below it (`prefix_covers`: `status` covers `status.text`, not
  `statusbar`).
- A hotkey may bind `values.set`, checked at bind time as the call is: a binding with a
  bad name or an oversized value is refused (`bind_problem`), so a saved binding never
  fails every press. Its value is then saved with the binding. A plugin may call
  `values.list` (class `read`), never `values.set`, which no class names yet.
- Since 0.112 the engine sets four values itself, from the MIDI clock its every-input
  MIDI state follows ([ADR-0147](../../docs/adr/0147-midi-tempo-and-state-for-pages.md),
  ADR-0122 §4): `tempo.bpm` (beats per minute to a tenth, removed while no clock runs),
  `tempo.playing` (`true` while the transport plays and its clock runs), and `tempo.beat`
  and `tempo.bar` (whole quarter notes and four-beat bars since the song's start). Its
  `values.changed` names the client `engine` and has no `request_id`. A client may still
  set these names, as any; the engine's next change overwrites them. A page reads them
  only with its source's `page_api_tempo`, never through `page_api_values`.

<a id="errors"></a>
## Errors
<!-- @section
covers = ["crates/jstream-protocol/src/error.rs"]
-->

`ProtocolError` is a `thiserror` enum ([R-RS-003](../../docs/RULES.md#R-RS-003)). On the
wire (`WireError`) it is its stable `code`, its detail fields, and a `message` for
people and agents. Clients match on `code`, never on `message`.

| Code | Meaning | Details |
|---|---|---|
| `unsupported_version` | The server can't speak the client's version | `client`, `server` |
| `handshake_required` | A request came before `hello` | — |
| `malformed` | Bad JSON, unknown `type`, missing or mistyped field | `reason` |
| `empty_batch` | A batch had no edits | — |
| `rev_conflict` | `expected_rev` is stale; nothing applied | `expected`, `current` |
| `not_found` | A command named a missing object | `entity`, `id` |
| `already_exists` | A creating edit reused an ID | `entity`, `id` |
| `invalid_argument` | A field's value was rejected | `field`, `reason`, and `blockers` for a refused delete |
| `undo_conflict` | An undo or redo would overwrite another client's later work; the step is dropped | `step_rev`, `client_id`, `rev` |
| `history_empty` | Nothing to undo or redo | `direction` |
| `action_failed` | An accepted action could not be carried out | `reason` |
| `unsupported` | A valid call this implementation doesn't serve (the pairing calls without a server in front, 0.21; the `plugin.*` calls from an engine that hosts no plugins, 0.35; the deprecated `privacy.set`, [ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)) | `call` |
| `forbidden` | A plugin's call is outside its grant, or is never granted to a plugin; it never reaches the engine (0.35) | `call`, `reason` |
| `hardware_session_limit` | `output.start` couldn't open a hardware video encoder while other outputs held hardware sessions, so the hardware session limit is taken to be reached; there is no fallback to software (0.44, [ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md) §3) | `outputs` (the holders, sorted), `reason` (the encoder's refusal) |
| `too_large` | The message was over the server's request limit (`Welcome.max_request_bytes`, 4 MiB); only its `id` was read (echoed when it is at most 256 bytes), nothing was applied, and the connection stays open. Retrying won't help: split the work (0.62, [ADR-0074](../../docs/adr/0074-too-large-requests.md)) | `size_bytes`, `limit_bytes` |
| `not_allowed` | Only for a `hello`: the person declined the connection, or didn't answer in time; the server then closes it (0.108, [ADR-0131](../../docs/adr/0131-ask-before-a-new-agent-connects.md)). A client that doesn't know the code still gets its `message`, so it can report it | `reason` |
| `internal` | An engine bug or unexpected condition; may be retried | `reason` |

An error that isn't tied to a request (a failed handshake, or a message too broken to
read an `id` from) has no `id`.

<a id="compatibility"></a>
## Schema and compatibility
<!-- @section
covers = ["crates/jstream-protocol/src/schema.rs", "crates/jstream-protocol/src/version.rs", "crates/jstream-protocol/schema/**"]
depends_on = ["docs/RULES.md#R-API-001"]
-->

`json_schema()` generates the JSON Schema (draft 2020-12) of every message from the
Rust types with `schemars`. Each type is a named definition, so the MCP server can hand
out per-group schemas. The test `schema_snapshot` compares it with
[`schema/protocol.snap`](schema/protocol.snap), and `wire_examples_snapshot` compares one
message of every variant with [`schema/examples.snap`](schema/examples.snap). Any API
change shows up as a diff of those files, and that diff is the API review
([R-TEST-002](../../docs/RULES.md#R-TEST-002)). After an intended change, regenerate them
with `INSTA_UPDATE=always cargo test -p jstream-protocol` (or `cargo insta review`), and
review the diff.

Compatibility follows [R-API-001](../../docs/RULES.md#R-API-001), at 0.x exactly as
after 1.0:

- **Minor (additive):** a new command, query, event, error code or `Call` kind; a new
  optional field. Old clients keep working, and a newer server still serves them
  (`can_serve`), because unknown fields are ignored (no type denies them), and because
  unknown server messages, events and error codes parse as an `Unknown` variant that
  clients skip. An unknown error code still has its `message`. From 0.9, an output's
  `OutputKind`, `RtmpTarget` and `RecordContainer` parse an unknown `type` or value as
  `Unknown` too, so later output kinds won't break a 0.9 client. Not yet covered: a new
  value of a string enum (`OutputState`, `EntityKind`, `BlendMode`) inside a known message breaks an
  older client's parse, so adding one needs open question 8 settled first. The same
  break happened once, in 0.9 itself: `OutputKind` had no fallback in 0.7 and 0.8, so a
  0.7 or 0.8 client can't parse `output.list` or `state.get` once a `record` output
  exists (the whole response fails, and `WsControl` ends the session). Whether that
  counts as a minor bump is part of open question 8. 0.13 adds the `EntityKind` value
  `encoder`, which only an error answering an encoder edit names, so a client older
  than 0.13, which sends none, never reads it. 0.19 adds `bus` the same way: only an error answering a bus or send edit
  (`bus.create`, `bus.rename`, `bus.set_fader`, `bus.delete`, `send.set`, `send.remove`,
  and since 0.42 `bus.set_monitor`) names it. An older client's undo of its own `source.delete` restores the source's
  sends, but if one's bus has since been deleted by another client, the undo fails with
  `undo_conflict`, which names no entity. 0.21 adds `device` the same way: only an error
  answering `device.revoke` names it, and 0.22 adds `filter` on the same terms for the
  filter edits. 0.30 adds the `EntityKind` value `transition` in the same way, but also
  the `KindCategory` value `transition`, which `kind.list` reports for every transition
  kind, so a client older than 0.30 can't parse `kind.list` from a 0.30 server. From
  0.30 `KindCategory` parses an unknown value as `Unknown`, so a later category won't
  break a 0.30 client. 0.31's organisation edits add `folder` and `label` to
  `EntityKind` for theirs, and 0.87 adds `timeline` the same way: only an error
  answering a timeline edit or action, or a `hotkey.set` naming a timeline, names it.
  0.94 adds `file` the same way: only an error answering `collection.new` or
  `collection.save_as` names it.
  0.96 adds `binding` the same way, named only by an error answering a `binding.*`
  edit.
  0.87's `SceneInfo.timelines` holds each cue's action as plain JSON, as a hotkey
  binding does, so a later action in a cue won't break a 0.87 client's `state.get`. The encoder enums (`VideoCodec`,
  `RateControl`, `HardwarePreference`, `AudioCodec`) parse an unknown value as
  `Unknown` from the start, and so do the motion enums (`MotionProperty`,
  `MotionShape`, `MotionKind`'s `kind` and `MotionClock`, 0.81, and `MotionMeasure` and
  `MotionCurve`, 0.86): a later kind or property won't break a 0.81 client's
  `state.get`. `check_motion` refuses an `Unknown`
  in an edit, and the engine draws one as still. 0.84's `impulse` kind and `triggered`
  clock are such values: a 0.81 client reads them as `Unknown`, and an `audio` modifier
  (0.86) as an unknown kind. `CameraProperty` (ADR-0113, 0.85) has the
  same `Unknown` fallback, which `check_camera_motion` refuses in an edit. 0.75 added the `BlendMode` values `normal_srgb` and
  `additive_srgb` (ADR-0105), which a client older than 0.75 couldn't parse. 0.101
  ([ADR-0126](../../docs/adr/0126-one-set-of-blend-modes-and-a-blending-space.md)) folds them back into `normal` and `additive`: the engine never
  sends them, and still reads them, so an older client that sends one keeps working,
  and its look then follows the collection's `blending`. `state.get`'s `blending` is
  always sent; missing, from an older server, it parses as `linear`, the only space
  that server drew.
  `BlendMode` still has no `Unknown` fallback; whether it gets one, and whether such a
  break counts as a minor bump, are part of open question 8. 0.99 adds the `Easing` and
  `ItemEasing` values `ease_in`, `hold` and `cubic_bezier` (ADR-0129) on the same
  terms: a client older than 0.99 can't parse `state.get` or an item listing once a
  preset, an item or a keyframe's timing uses one.
- **Major (breaking):** removing or renaming anything, changing a field's type or
  meaning, or making an optional field required. It needs an ADR and one minor release
  that deprecates the old form first.
- Every public enum is `#[non_exhaustive]`, so Rust clients must handle new variants
  too, except `FilterChain`: an owner has a video and an audio chain and no other
  ([ADR-0026](../../docs/adr/0026-source-filter-chains.md)), so code that picks a
  chain matches it exhaustively.

<a id="motion"></a>
## Motion
<!-- @section
covers = ["crates/jstream-protocol/src/motion.rs"]
-->

Since 0.81 an item carries continuous motion
([ADR-0099](../../docs/adr/0099-native-motion-modifiers-and-timelines.md), `motion.rs`):
`ItemInfo.motion`, an ordered list of at most 8 `MotionModifier`s that the engine
evaluates as each frame is drawn. A modifier never writes the item's values: `state.get`
reports the same geometry, crop and opacity whatever it adds. Each modifier has a
`modifier_id` (1 to 64 bytes of ASCII letters, digits, `_` or `-`, unique in the list),
the `property` it moves (`x`, `y`, `scale`, `rotation`, `tilt`, `turn`, `depth`,
`opacity` and the four `crop_*` edges, or since 0.109 `effect` with its `effect`), its
`kind`, its `clock` and `enabled`.
- **Kinds:** `wiggle` (smooth noise: `amplitude`, `frequency_hz` in `(0, 60]`, `seed`,
  `octaves` 1 to 4), `oscillate` (`shape` `sine`, `triangle`, `square` or `saw`,
  `amplitude`, `period_ms` 1 to 3 600 000, `phase` in `[0, 1)`), `drift` (a steady
  `rate_per_s`, wrapping back to 0 past `wrap` when it has one) and, since 0.84,
  `impulse` (a one-shot decaying shake,
  `amplitude · e^(−decay_per_s·t) · sin(2π·frequency_hz·t)` until `duration_ms`, then
  0: `frequency_hz` in `(0, 60]`, `decay_per_s` 0 to 1000, `duration_ms` 1 to
  3 600 000). Amplitudes are 0 to 100 000, rates within ±100 000.
- **`audio`** (since 0.86,
  [ADR-0100](../../docs/adr/0100-audio-reactive-motion-modifiers.md)) follows what it
  hears instead of a clock: the program mix (the master after its filters, as the
  outputs hear it), a bus named by `audio` after its fader, or a source named by
  `audio_source` after its own filters, fader, mute and follow, as a `spectrum` source
  with the same settings would draw. Its `measure` is `level` (the default, RMS),
  `peak`, `onset` (1 on a jump of the level above its recent average, else 0) or `band`
  (the spectrum between `low_hz` and `high_hz`, both `20..=20000` and only with `band`,
  as an amplitude). The measure in dBFS from `floor_db` (default -60, at least -90) to
  `ceiling_db` (default 0, at most 0, above the floor) maps onto 0 to 1, an onset as it
  is; that is followed with `attack_ms` (default 10) and `release_ms` (default 250),
  each `0..=10000`, bent by `curve` (`linear`, the default, `ease_in`, `ease_out` or
  `ease_in_out`, none overshooting), and mapped onto `from` (default 0) to `to`
  (default 1), each within ±100 000 and `to` below `from` to invert. Silence is `from`,
  and so is everything the engine doesn't hear: while no output runs the program mix
  is silent, and a preview reads every audio modifier as silence. Its `clock` is
  ignored.
- **How a value applies:** positions, angles, depth and crop edges add it (crop in
  source pixels, rounded, never below 0); `scale` and `opacity` multiply by `1 + value`,
  and opacity is then kept within 0 and `MAX_OPACITY` (4, since 0.83). Two modifiers on one property add up.
- **Effect settings** (since 0.109,
  [ADR-0138](../../docs/adr/0138-motion-drives-effect-settings.md)): `property`
  `effect` drives a number setting of one of the item's own effects instead, which
  `effect: {filter_id, param}` (`MotionEffect`) names: the effect by its ID among the
  effects the item draws (a keyframe's override in that keyframe) and a `float`, `int`
  or colour parameter of its kind by its ID, `param` 1 to 64 bytes of ASCII letters,
  digits and `_`. Only property `effect` takes an `effect`, and it needs one
  (`motion.<n>.effect`). Its value is added to the setting (during a Move, to the
  blended one): a number is kept within the parameter's range and rounded for an
  `int`; a colour has it added to each of its red, green and blue as a fraction of
  full, each kept within 0 and full, its alpha unchanged. One naming an effect or
  parameter the item doesn't draw, or one of another type, adds nothing. It never moves the box.
  `effect` is omitted for every other property, and a client before 0.109 reads the
  property as `Unknown`.
- **Clocks:** `engine` (the default) is the engine's animation time, so items with the
  same settings move in step; `shown` starts at 0 each time the engine starts drawing
  the item for program or a video mix (in any of their passes). A scene or source
  preview doesn't run it: there a `shown` modifier stays at its start. `triggered`
  (since 0.84) starts each time `item.trigger_motion` fires the modifier; until then,
  and in a preview, it adds nothing.
- **`item.set_motion {item_id, scene_id?, motion}`** writes the whole list, and is
  undone by the same edit with the old list. Addressed to a keyframe scene, it writes
  that keyframe's override: `ItemOverride.motion`, which `item.clear_override` with
  field `motion` removes. An empty override list stops the item's motion in that
  keyframe. A list that breaks the rules above is `invalid_argument` naming the field
  (one over 8 suggests a group around the item, whose motion moves its children),
  and `check_motion` is the check every implementation shares. Plugins need the `edit`
  class for it. It isn't a live value yet (ADR-0099 decision 3 plans that, so a slider
  previews as it drags): each change is one undo step. `depth` is a property beyond
  ADR-0099's list, for 3D scenes.

**`item.trigger_motion {item_id, modifier_id?}`** (an action, since 0.84, #1107) fires
the item's `triggered` modifiers from now: the one `modifier_id` names, or every one when
it is omitted. Firing a running one restarts it. A fire is runtime state like the program
scene: no `rev`, no undo, no event. A modifier on a group's item moves the whole group,
masks included. Firing a modifier the item doesn't hold, or one on another clock, does
nothing, since a keyframe scene can override the list; an item not in the collection is
`not_found`, and a malformed `modifier_id` is `invalid_argument`. Plugins need the `edit`
class. Its `named_objects` is the item, so a hotkey binding to it goes with the item.

During a scene switch a moving item keeps moving: a matched pair blends its two sides'
motion by the transition's progress, an appearing item moves as its new scene says and a
disappearing one as its old scene said.

<a id="timelines"></a>
## Timelines
<!-- @section
covers = ["crates/jstream-protocol/src/timeline.rs"]
depends_on = ["docs/adr/0115-timelines-on-the-wire.md"]
-->

Since 0.87 a scene holds timelines
([ADR-0099](../../docs/adr/0099-native-motion-modifiers-and-timelines.md) decision 4,
[ADR-0115](../../docs/adr/0115-timelines-on-the-wire.md), `timeline.rs`):
`SceneInfo.timelines`, ordered by ID, each a `TimelineInfo` of timed cues.
- **Shape:** a `timeline_id` (1 to 64 bytes of ASCII letters, digits, `_` or `-`,
  unique across the collection, so an action names it alone), a `name`, a
  `duration_ms` (at most one hour), a `loop` (`once`, `loop`, or
  `{"range": {"from_ms", "to_ms"}}` for an intro then a loop; a repeat is at least
  100 ms, `MIN_LOOP_MS`) and at most 256 `cues`,
  each `{at_ms, action}`, in time order. A cue's `action` is read as plain JSON, as a
  hotkey binding's is (`TimelineCue::action` parses it), so a client older than an
  action a later cue runs still reads `state.get`; `timeline.set` refuses an action
  it doesn't know at `cues.N.action`. A scene holds at most 32. A cue a loop would
  never reach (at or past where it jumps back, or past a `once` timeline's end) is
  refused. `TimelineInfo::problem` is the check every implementation shares. Since 0.100
  a timeline carries `labels` too, omitted for none
  ([ADR-0133](../../docs/adr/0133-labels-on-hotkeys-and-timelines.md)): each must exist
  (`not_found`) and be listed once (`invalid_argument` on `timeline.labels`), and they
  are kept ordered by ID ([Folders, labels and scene options](#organisation)).
- **Cue actions** are a closed set (`Action::cue_problem`): `scene.switch`,
  `scene.set_keyframe` (since 0.88, [Keyframe tracks](#keyframe-tracks)), `item.show`, `item.hide`, `item.toggle`, `item.trigger_motion`, `values.set`,
  `source.restart`, and `timeline.play`, `timeline.stop` and `timeline.seek`. None is an
  edit, so playing a timeline adds nothing to an undo history.
- **Edits:** `timeline.set {scene_id, timeline}` stores one whole, new or in place of
  the scene's own, and is undone by `timeline.delete` or by `timeline.set` with the old
  one. An ID another scene holds is `already_exists`. `timeline.delete {timeline_id}`
  removes one, with the hotkeys bound to it (`removed_hotkeys`). The objects a cue names
  are not checked: a cue naming a deleted object stays, inert, and fails when it fires.
  Plugins may not use either edit (`never`), since cues run actions the edit class
  can't.
- **Playing** is studio state, like the program scene. `timeline.play {timeline_id}`
  plays from the start, `timeline.seek {timeline_id, at_ms}` from `at_ms`, running at
  once the last cue before it for each state it sets (`Action::cue_state`: the program
  scene, one placement's visibility, one shared value), and `timeline.stop` stops it
  where it is. A cue runs as its call would, credited to the client that started the
  timeline, at the cue's exact time on the animation clock. `studio.get` lists the
  timelines playing (`PlayingTimeline`: where each is, and its pass).
  `timeline.changed` reports a start, a seek, a loop, an end, a stop or a removal
  (`TimelineChange`, which parses an unknown value as `unknown`), and
  `timeline.cue_fired` each cue, with the error its action got.
- **`source.restart {source_id}`** restarts a `browser` page, a `media` file or an
  effect pack's shader source from its beginning, whatever its `restart_on_show`; other
  kinds have nothing to restart. Plugins need the `live` class for it.

<a id="keyframe-tracks"></a>
## Keyframe tracks
<!-- @section
depends_on = ["docs/adr/0116-keyframe-tracks.md"]
-->

Since 0.88 a scene placed in another switches its own keyframe
([ADR-0116](../../docs/adr/0116-keyframe-tracks.md)), so independent states (a
background, a panel layout, an effect) don't each need a keyframe scene per
combination.
- **`scene.set_keyframe {scene_id, keyframe_id?, preset_id?}`** draws `scene_id`
  through its keyframe scene `keyframe_id` wherever another scene places it, or through
  its own values when `keyframe_id` is omitted. A missing scene, keyframe or preset is
  `not_found`; a keyframe scene as `scene_id`, or a `keyframe_id` that isn't one of its
  keyframe scenes, is `invalid_argument`. A hotkey can bind it, a timeline cue can run
  it (a state for `timeline.seek`, keyed by the scene), and plugins need the `live`
  class.
- **Studio state:** no `rev`, never undone or saved. `studio.get`'s `keyframes` lists
  each switched scene (`PlacedKeyframe`), and `scene.keyframe_changed` reports each
  change, including the release when a batch, undo or redo deletes the scene or the
  keyframe scene (`keyframe_id` omitted).
- **Drawing:** where the placing scene is drawn through a keyframe of its own that
  overrides an item in the placed scene's tree, that keyframe draws it; otherwise the
  switched one does. A switched scene on program itself, or through one of its own
  keyframe scenes, draws as that scene, not through the switch.
- **On program**, a switch with a `move` or `fade` preset (the default preset when
  `preset_id` is omitted) plays as a Move from what is on screen, each item paired with
  itself; any other kind switches at once.

<a id="geometry"></a>
## Item geometry
<!-- @section
covers = ["crates/jstream-protocol/src/geometry.rs", "crates/jstream-protocol/src/geometry/**"]
depends_on = ["docs/RULES.md#R-API-001"]
-->

[ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md) defines an item's box as
`ItemGeometry` ([`geometry.rs`](src/geometry.rs)): `center` and `size` in canvas pixels,
`fit_mode` (`stretch`, `fit` or `fill`, default `fit`), clockwise `rotation_deg` about
the centre, 3D `tilt_deg`, `turn_deg`, `depth_px` and optional `perspective_px`, and
the flips `flip_h` and `flip_v`. The 3D fields default to neutral: zeros, and
perspective absent. The flips default to `false` and are omitted from the wire when
`false`, so an unflipped geometry's JSON is what it was before they existed. The formula is a pure
function in this crate so the compositor and the UI share one implementation.

- `place` and `project` map a local-frame point, and the box's four corners, through
  rotation about z, then turn about y, then tilt about x, then depth, then per-item
  perspective, then add `center`. Corner order is top-left, top-right, bottom-right,
  bottom-left of the unrotated box. A point with z at or beyond `perspective_px` is
  behind the viewer. With the 3D fields neutral this is rotation about the box centre
  in the canvas plane. Depth alone, with no perspective, does not move the orthographic
  projection.
- In a scene with a camera (ADR-0094) the same box is a pose in the scene's space:
  `(center.x, center.y, depth_px)` its position, the three angles its orientation, and
  `perspective_px` ignored. `project_seen`, `drawn_polygon_seen` and `unplace_seen`
  take the camera's `CameraView` and build ADR-0050's homogeneous map with `w = D / f`
  (`D` the distance along the viewing axis, `f = (H / 2) / tan(fov / 2)`), so the near
  plane and the polygon clipping are the same. The picture's up is −y, or within
  0.01° of the pole −z looking straight down and +z looking straight up, so a camera
  facing −z keeps its picture upright as it tilts to either pole
  ([ADR-0111](../../docs/adr/0111-camera-up-at-the-poles.md), #1047). Through `SceneCamera::framing` an item
  with neutral 3D lands exactly on its 2D box. `CameraView::on_plane` goes the other way
  for an editor: the point of the plane `z = depth` under a canvas point, which the UI's canvas
  drags an item to. `CameraView::in_view` and `direction_in_view` take a point or a
  direction of the scene's space into the viewing frame a `Solid` seen through the
  camera uses, relative to the camera in `f64`: where the renderer places scene lights
  ([ADR-0143](../../docs/adr/0143-scene-lights-and-depth-aware-scene-filters.md)). `ItemGeometry::light_pose` gives a light's place
  and aim in the scene's space from the item that places it: the centre of its box at
  its `depth_px`, and its back normal, `[0, 0, -1]` (away from the viewer) unturned and
  turned by its rotations as a placed model's back is.
- `content` maps a cropped source into that local frame, before the placement above.
  `stretch` fills the box. `fit` is the largest rectangle of the source's aspect ratio
  inside the box, centred, and the rest stays transparent. `fill` is the smallest such
  rectangle that covers the box, centred, and may extend past it; the compositor clips.
  A crop that leaves no pixels draws nothing. Crop stays [`Crop`](src/command.rs), in
  source pixels, applied before fitting.
- `flip_h` and `flip_v` are the additive booleans ADR-0022 reserved for flips. They
  mirror the cropped, fitted content about the box centre, in the box's local frame
  before rotation and 3D: `content` swaps `u0` with `u1` or `v0` with `v1`. The box,
  its projected corners and `transform_view` do not change, so selection handles and
  0.3 clients see the same box. A negative `size` is still rejected, not read as a
  flip. The flips travel inside `ItemGeometry`, so `item.add`, `item.set_geometry` and
  its inverse carry them with no edit of their own. The compositor draws them, so the
  engine accepts both.
- `legacy_quad` and `ItemGeometry::from_legacy` evaluate a 0.3 `ItemTransform`: crop,
  then scale, top-left at `position`, clockwise about that corner. `from_legacy`
  returns that outline as a box with `fit_mode = stretch` and neutral 3D, so a
  non-uniform scale stays exact, plus whether a negative scale flipped an axis.
  A negative scale is not refused: it becomes the box's `flip_h` or `flip_v`, so the
  box draws the same pixels. Projecting that box gives its outline, not the reflected
  corner order, which `legacy_quad` keeps.
- `drawn_polygon` and `unplace` (`geometry/drawn.rs`,
  [ADR-0050](../../docs/adr/0050-3d-transforms-rendering.md) decisions 1 to 3) are what
  a compositor draws in 3D, and its inverse. `drawn_polygon` clips `content`'s rectangle
  to the box, then by the near plane `w ≥ NEAR_W` (1/256: nothing magnified more than 256
  times), both in the local frame where texture coordinates are linear. It returns three
  to five `DrawnVertex`es, each with its canvas point (`place` of its local point), local
  point, texture coordinate and `w = (p − z) / p` (1 without perspective), so the GPU
  can interpolate texture coordinates perspective-correctly. It returns nothing when the
  content misses the box, the whole item is past the near plane, it is seen edge-on, or
  it covers under 0.01 px².
  Edge-on is decided by the plane's facing (the z of its unit normal under a millionth:
  through the viewer with perspective, side-on without), whatever the item's size, and
  `unplace` makes the same test, so the two agree.
  With neutral 3D it is the 2D quad, its texture coordinates clipped to the box.
  `unplace` maps a canvas point back to the local point it shows: a 2×2 solve, in
  `f64`, with nothing returned edge-on or past the near plane. It doesn't check the box,
  so a hit test compares the point with it. `drawn_vertex` gives the vertex of one
  local point (canvas point, texture coordinate and `w`, as the polygon's own), or
  nothing past the near plane: the compositor uses it for the corners it pushes out
  for edge coverage. All three allocate nothing, for the render thread. They are pure
  functions with no wire form. `jstream-render` draws from them, and from 0.34 the
  engine accepts the 3D they can draw (`check_drawable`, Validation below).

**On the wire (0.11).** ADR-0022 names these additions as 0.4, which had already
shipped with `events.dropped`, so they ship as 0.11, an additive minor under
[R-API-001](../../docs/RULES.md#R-API-001). The accepted ADR is not edited
([R-DOC-009](../../docs/RULES.md#R-DOC-009)).

- **Canvas.** `state.get` reports `canvas { width, height }` in whole pixels, default
  1920×1080, and `canvas.set_size` changes it. Zero, or a side over the engine's
  maximum (`jstream-graph` accepts up to 8192 each way, so 7680×4320 and its portrait
  form fit), is `invalid_argument` on `edits.N.width` or `edits.N.height`. Items keep
  their canvas-pixel placements. Inverse: `canvas.set_size` with the previous size.
  `canvas.set_frame_rate { fps }` (0.89, ADR-0119) sets the collection's base frame
  rate to 30 or 60. A new collection is 30. The program stage keeps the rate it opened
  with, and a change applies the next time it opens. A mix's `fps_divisor` is relative
  to that rate, so 60 doubles every mix. Anything else is `invalid_argument` on
  `edits.N.fps`. `state.get` reports `base_fps`, omitted while it is 30. Inverse:
  `canvas.set_frame_rate` with the previous rate. `canvas.set_blending { blending }`
  (0.101, [ADR-0126](../../docs/adr/0126-one-set-of-blend-modes-and-a-blending-space.md)) sets the space every blend mode mixes in: `srgb`, browser-style
  and a new collection's, or `linear`. It applies to every scene from the next frame,
  and `state.get` reports it as `blending`. Inverse: `canvas.set_blending` with the
  previous space.
- **Two placement forms.** An item stores either a geometry or a legacy 0.3 transform,
  and only an edit changes which. `item.add` takes an optional `geometry` beside the
  deprecated `transform`; sending both is `invalid_argument` on `edits.N.geometry`, and
  sending neither stores the identity transform, its 0.3 meaning, until 1.0.
  `item.set_geometry` stores a geometry and the deprecated `item.set_transform` a
  transform. Each one's inverse restores the previous placement in its own form. A
  legacy transform keeps its 0.3 meaning, so any finite one is stored (ADR-0022); one
  whose corners for the source's current size leave f32's range draws nothing, and the
  frame goes on (#1130).
- **Validation.** A stored geometry has a size greater than zero
  (`edits.N.geometry.size`), a `perspective_px` greater than zero if present
  (`edits.N.geometry.perspective_px`), and finite corners (`edits.N.geometry`). From
  0.34 any `tilt_deg`, `turn_deg`, `depth_px` and `perspective_px` is accepted and
  drawn, provided the compositor can draw it: `ItemGeometry::check_drawable`, which the
  engine and the mock share, also bounds a non-neutral one by `MAX_DRAWN_PX` (10^15
  canvas pixels). Its centre's coordinates, its width and height, `|depth_px|` and
  `perspective_px` must be at most that, and so must every vertex of the polygon its
  whole box draws. Otherwise the edit is `invalid_argument` on `edits.N.geometry`
  (`TooFar`, or `NotFinite` for a vertex that overflows). Bounding the inputs is what
  keeps every content's polygon finite: in f32 the whole box's polygon doesn't bound a
  thin source's, and a huge item on an 8192×1 source would otherwise overflow while it
  is drawn and fail its whole frame. An item wholly behind the viewer or seen edge-on is
  valid and draws nothing.
  Before 0.34 a non-neutral one was `invalid_argument` naming that field, with
  `is_neutral_3d` as the predicate. ADR-0022 said accepting them is not a breaking
  change.
- **Reads.** `ItemInfo.placement` is `legacy` or `geometry` (missing from a pre-0.11
  server, it parses as `legacy`). A legacy item reports its transform exactly as
  written and no `geometry`, because its box depends on the source's runtime size. A
  geometry item reports its `geometry`, and a deprecated `transform` computed from it
  alone (`transform_view`): the box's top-left corner after rotation about the centre,
  its rotation, and scale (1, 1). Every read is a function of stored state, so it
  changes only with `rev`.
- **Deprecated** since 0.11, removed in 1.0: `ItemTransform`, `item.set_transform`,
  `item.add`'s `transform` and `ItemInfo.transform`. Their doc comments, and so the
  schema descriptions in `protocol.snap` and the MCP `tools.snap`, say so
  ([R-TEST-002](../../docs/RULES.md#R-TEST-002)).
- **Deprecated**, removed in 1.0: `privacy.set`, `privacy.get` and
  `privacy.changed`, since JStream has no privacy screen
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md) §4). Their
  wire forms are unchanged, so no minor is bumped: `privacy.set` is refused with
  `unsupported`, `privacy.get` answers `on: false`, and the event is never sent.
- **Flips (0.32).** `flip_h` and `flip_v` are optional fields, an additive minor under
  [R-API-001](../../docs/RULES.md#R-API-001): an older peer ignores them, so it shows a
  flipped item unflipped, and one that writes the geometry back drops the flip.
- **Not in 0.11:** `source.sizes` and `source.size_changed` (decision 4), which arrived
  in 0.40 ([Queries](#queries), [Events](#events)), and live geometry values, which
  arrived in 0.26 with `live.set` ([Live values](#live-values)).

<a id="binary-streams"></a>
## Binary streams
<!-- @section
covers = ["crates/jstream-protocol/src/preview.rs"]
-->

Preview video travels as WebSocket binary messages, one `BinaryFrame` each, alongside
the JSON text messages of the same connection (0.20,
[ARCH › Control protocol](../../docs/ARCHITECTURE.md#protocol),
[ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). Audio meters travel in the JSON
`meters.tick` event; a binary meter stream would be another kind of the same frame.

- **Opening and closing.** `preview.subscribe {channel, max_width?, max_height?, fps?,
  codecs?, scene_id?, source?, source_id?}` is an action: it opens a stream of the
  program mix, of the scene `scene_id` (0.46), of the draft source `source` (0.49), or
  of the collection's source `source_id` alone (0.56), for the calling
  session only, tagged with the `channel`
  number the client chose (a `u32`, not an ID: it only tells the session's streams
  apart in the frame header). The picture is the canvas scaled down to fit
  `max_width` × `max_height` (default 640 × 360, at most 1920 × 1080) with even sides,
  so it follows `canvas.set_size`. A picture is sent when what the stream shows
  changes, at most `fps` times a second (default 15, at most 60 since 0.95, 30 before; the engine
  runs it no faster than the collection's base rate), and at least every
  2 s as a keyframe. `codecs` lists what the client can decode, most preferred first
  (default `["h264", "jpeg"]`); the engine uses the first it can encode, and may fall
  back to a later one if an encoder fails to open, so each frame names its codec.
  `png` (0.71, [ADR-0096](../../docs/adr/0096-item-lock-and-png-preview-frames.md)) is
  lossless and never a default: a client taking a screenshot asks for it alone and
  keeps the first picture. A
  session has at most 16 streams (4 before 0.46), enough for a multiview.
  `preview.unsubscribe {channel}` closes one (and succeeds for a channel that isn't
  open); so does the session's end. The stream shows the mix before any master stage,
  which is the default ARCH asks for; a post-master option comes with that stage. Over
  MCP, which has no binary channel, `preview.subscribe` fails with `action_failed`.
- **Scene streams.** A `scene_id` stream shows that scene alone, as program would show
  it with no transition running, whether or not it is on program: what the UI's
  pop-out and projector windows and its multiview show. It never makes a source shown
  or heard ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)), and the program stage
  never draws it, so it costs an output nothing; the engine captures the displays,
  windows and webcams it shows for the preview itself. A scene the collection doesn't
  have fails with `not_found`; a scene deleted while its stream is open ends the stream
  with an end frame.
- **Draft sources.** A `source` stream (`PreviewDraft {kind, settings?}`, 0.49) shows a
  source the collection doesn't hold, alone, as an item covering the canvas would show
  it and with no filters: what a `display`, `window` or `webcam` source with those
  settings captures, so a client shows the device being picked before the source is
  created or its new settings are saved (the UI's Properties dialog). The settings are
  checked as `source.create` checks them; a refused one fails with `invalid_argument`
  naming `source.settings.<param>`, a kind that isn't a capture kind or that the engine
  doesn't have names `source.kind`, and a request with both `scene_id` and `source`
  names `source`. The stream keeps the canvas size it was opened at: a client
  subscribes again for other settings. A webcam or capture card already open is shared,
  not opened twice. Nothing reaches the collection: no `rev` changes and no event goes
  out.
- **Source streams.** A `source_id` stream (0.56) shows one of the collection's sources
  alone, fitted into the canvas as an item covering it would show it, with the source's
  own filters and none of an item's: what the UI's projector of a source shows. Like a
  scene's stream it follows the source's settings, filters and the canvas as they
  change, never makes the source shown or heard, and costs an output nothing. A source
  the collection doesn't have fails with `not_found`, a request with `scene_id` or
  `source` too (or both) fails with `invalid_argument` naming `source_id`, and a source deleted
  while its stream is open ends the stream with the end frame "the source was
  deleted".
- **Views report back.** A client that shows preview streams reports what each of its
  *views* (the canvas, a projector picture) did with them: `preview.report {view,
  counts}` (0.103, [ADR-0132](../../docs/adr/0132-bench-gates-views-and-quality-probe.md))
  carries the view's own label (1 to 64 characters, no control characters) and its
  running `ViewCounts` since it opened, over every stream it has shown: frames
  `received`, pictures `shown`, frames `skipped` to catch up, `decode_errors`, the
  `placeholders` it showed where a picture belongs (its first included), the `blanks` it
  showed when a stream ended or failed, and the streams it `reopens` after its first. A
  client sends it about once a second while the counts change. Counts only grow; a
  report whose counts fall below the view's last starts the view again (a reloaded
  page). It serves only its session: nothing reaches the collection, no `rev` changes,
  it is not in the activity log, and neither a hotkey nor a plugin may carry it. A
  session reports at most 16 views; a bad label or a 17th view fails with
  `invalid_argument` on `view`. `profile.get`'s `views` reads them back.
- **The frame.** A fixed 32-byte little-endian header, then the payload:

  | Offset | Size | Field |
  |---|---|---|
  | 0 | 1 | version, 1 |
  | 1 | 1 | kind: 1 = preview |
  | 2 | 1 | codec: 0 = none (an end frame), 1 = H.264, 2 = JPEG, 3 = PNG (0.71) |
  | 3 | 1 | flags: bit 0 keyframe, bit 1 end of stream |
  | 4 | 4 | channel |
  | 8 | 2 | width |
  | 10 | 2 | height |
  | 12 | 4 | reserved, 0 |
  | 16 | 8 | presentation time in µs since the stream opened (signed) |
  | 24 | 8 | the `rev` the picture shows |

  An H.264 payload is one access unit in Annex B form, with the parameter sets in band
  at keyframes, which WebCodecs' `VideoDecoder` takes with no `description`. A JPEG
  payload is a whole baseline JFIF picture, always a keyframe, and a PNG payload a whole
  RGBA PNG, always a keyframe too. An end frame (flag bit
  1, codec 0) says the engine ended the stream; its payload is the reason as UTF-8, and
  the channel is free again. A client ignores a frame whose version, kind or codec it
  doesn't know, and the reserved bytes and spare flag bits.
- **Overload.** Frames are never buffered without bound. Each hop that queues them (the
  engine, the server, the WebSocket client) drops a frame that finds its queue full,
  and every later frame of that channel until a keyframe finds room (`KeyframeGate`),
  so what a client receives always decodes. The 2 s keyframe bounds the gap.
- **Compatibility.** The header's version byte changes only with a new protocol major.
  A new kind, codec or flag is a minor change, and a frame a client can't read is
  skipped.

`video.snapshot` is not one of those frames, nor is a frame sheet. M1a uses it so an agent can check a scene
over the JSON API and over MCP ([ROADMAP › M1](../../ROADMAP.md#m1),
[ARCH › MCP](../../docs/ARCHITECTURE.md#mcp)), so the PNG is a field of the query
result. A binary snapshot frame would need a second channel those clients do not have.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

Decisions for the maintainer. Until one is settled, the first cut does what is
described above.

1. **Framing.** A custom envelope (as here), or JSON-RPC 2.0 framing (`method`,
   `params`, `id`)? JSON-RPC is what MCP speaks and has client libraries, but it has no
   handshake or typed error details of its own, and its errors are numeric.
2. **Single edits.** Every edit travels in a batch. Should there also be a shorthand
   that sends one edit without the `edits` array?
3. **IDs.** Clients choose the IDs of what they create (this design). Alternatives:
   server-assigned IDs, with batches referring to created objects by local names. And
   should the ID types move to `jstream-types`, once it has IDs and an optional serde
   feature?
4. **Undo and redo on the wire.** Settled by
   [ADR-0032](../../docs/adr/0032-live-edits-and-undo.md) and built in 0.10:
   `history.undo` and `history.redo` are a fourth kind of call, `history`, which
   carries no command (it applies a batch of edits the engine already holds), so
   R-ARCH-006 needs no change. The same ADR adds live values for continuous gestures
   (`live.set`, `live.cancel`, `live.changed`), committed by one batch; those are built
   for item placements and crops and, since 0.67, a source's live settings
   ([Live values](#live-values)), not yet for audio faders.
5. **Actions that change collection state.** Settled by
   [ADR-0030](../../docs/adr/0030-studio-state-and-scene-switching.md): the program
   scene is runtime state, not collection state, so `scene.switch` is an action that
   changes no `rev`, and no action changes collection state.
6. **Idempotency.** How long the engine remembers keys (count or time), and whether a
   reused key with a different batch is an error (`idempotency_key_reused`).
7. **Event subscriptions.** Every client gets every event in this cut, including
   `stats.tick` (0.6) and `live.changed`. High-rate events (`stats.tick`,
   `live.changed`, meters) will need opt-in subscriptions. Until then every client
   receives a tick about once a second while the pipeline runs, and up to 30
   `live.changed` a second per gesture while someone drags.
8. **Unknown values in string enums.** `OutputState`, `EntityKind` and `BlendMode` have no
   `Unknown` fallback, so adding a value breaks older clients that parse it. Add fallbacks, or treat
   a new value as a major change? `OutputKind`, `RtmpTarget` and `RecordContainer` have
   fallbacks from 0.9 and `KindCategory` from 0.30, but 0.9's `record` kind already breaks 0.7 and 0.8 clients reading
   outputs, and 0.75's `_srgb` blend modes break older clients reading items (see
   Compatibility); kept as a minor bump until this is decided.
9. **Schema delivery.** The schema lives in an `insta` snapshot. Should `xtask gen-schema`
   also publish a plain `.json` file for non-Rust clients?
10. **ADR.** Should the envelope, tagging and compatibility policy above be recorded as an
    ADR ([R-ARCH-004](../../docs/RULES.md#R-ARCH-004)) once they are settled?
11. **Source kinds.** Settled by
    [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md) and built in 0.7:
    `builtin` stays, its `kind` names a kind the engine describes (`kind.list`,
    `kind.describe`), and settings travel beside it as a map validated against that
    kind's descriptor ([Kinds and settings](#kinds)). No new `SourceKind` variants. Not
    built yet: the `kind.changed` event, `asset.list`, storing an
    absolute path under a known root in relative form, and kind versions on stored
    settings, which come with persistence.
12. **Deleting referenced objects.** Settled by
    [ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md) and built:
    rejection stays the only delete, with no cascading edit. The error lists every
    blocker in `blockers`, and the `delete.plan` query returns the batch that removes
    the target and its blockers ([Edits, actions and queries](#calls)).
