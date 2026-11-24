//! Media files dropped on the Show panel's tracks. A video, audio or image file dropped
//! on a track becomes a real scene, in one undo step: one batch adds a media source (an
//! image source for an image) for the file, a scene named after it holding that source,
//! filed in the "Show media" folder of the Scenes panel, and the clip that places it.
//!
//! On V1 the source fills the frame; on an overlay track it keeps its own size and
//! transparency. An audio file dropped on an audio track is an audio clip with the file
//! as its source and default fades, and no scene; dropped on a V track it is a scene with
//! only that sound. Dropping the same file again reuses its scene (or its source on an
//! audio track), not a copy.
//!
//! A video or audio clip takes the file's length and is timed: a jump into it seeks. An
//! image has no length, so its clip takes the show's image length (10 s unless the show
//! says otherwise). A clip dropped on V1 ends in a pausing bookmark, except a timed one,
//! which ends with its media.
//!
//! Everything here is a pure function of the collection and the drop, so it is tested
//! natively; [`dropped_paths`] reads a browser drop's file paths.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::model::{ClipKind, ClipView, TrackKind};
use super::time;
use crate::store::Collection;
use jstream_control::protocol::{
    AssetRoot, Edit, FolderId, FolderInfo, ItemGeometry, ItemId, Reference, SceneId, SceneInfo,
    SceneRole, Settings, SourceId, SourceInfo, SourceKind,
};
use leptos::wasm_bindgen::JsValue;
use serde_json::json;
use std::fmt;

/// The folder in the Scenes panel that scenes made by dropping files go into.
pub const SHOW_MEDIA_FOLDER: &str = "Show media";

/// How long an image's clip is, unless the show sets its own: 10 s.
pub const DEFAULT_IMAGE_MS: u32 = 10_000;

/// An audio clip's fade in and out, in ms, as it is dropped.
pub const DEFAULT_FADE_MS: u32 = 500;

/// The video files the `media` kind plays.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm"];

/// The audio files the `media` kind plays.
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "flac", "ogg", "m4a"];

/// The image files the `image` kind shows. SVG is not among them: the engine's image
/// source doesn't draw it, so an SVG is refused rather than made into a broken scene.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// What a dropped file is, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaClass {
    /// A video file, played by a `media` source.
    Video,
    /// An audio file, played by a `media` source.
    Audio,
    /// An image, shown by an `image` source.
    Image,
    /// Anything else: refused.
    Unsupported,
}

impl MediaClass {
    /// The source kind that plays it, if any.
    pub fn kind(self) -> Option<&'static str> {
        match self {
            MediaClass::Video | MediaClass::Audio => Some("media"),
            MediaClass::Image => Some("image"),
            MediaClass::Unsupported => None,
        }
    }

    /// Whether a clip of it is timed: it holds a video or audio file, so a jump into its
    /// middle seeks (the clock badge).
    pub fn is_timed(self) -> bool {
        matches!(self, MediaClass::Video | MediaClass::Audio)
    }

    /// The word on the clip's second line.
    fn word(self) -> &'static str {
        match self {
            MediaClass::Video => "video",
            MediaClass::Audio => "audio",
            MediaClass::Image => "image",
            MediaClass::Unsupported => "file",
        }
    }
}

/// What `path` is, by its extension, ignoring case.
pub fn classify(path: &str) -> MediaClass {
    let name = file_name(path);
    let Some((_, ext)) = name.rsplit_once('.') else {
        return MediaClass::Unsupported;
    };
    let ext = ext.to_ascii_lowercase();
    let is = |list: &[&str]| list.contains(&ext.as_str());
    if is(VIDEO_EXTENSIONS) {
        MediaClass::Video
    } else if is(AUDIO_EXTENSIONS) {
        MediaClass::Audio
    } else if is(IMAGE_EXTENSIONS) {
        MediaClass::Image
    } else {
        MediaClass::Unsupported
    }
}

/// The last part of a path, split at `/` or `\`: `intro.mp4`.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `name`, or `name 2`, `name 3`… : the first no scene in the collection is called.
pub fn scene_name(collection: &Collection, name: &str) -> String {
    let taken = |n: &str| collection.scenes.iter().any(|s| s.name == n);
    if !taken(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| name.to_owned())
}

/// A file dropped on the panel, as the engine will open it.
#[derive(Clone, Debug, PartialEq)]
pub struct DroppedFile {
    /// Which folder `path` is under ([`crate::browse::place_asset`] picks it).
    pub root: AssetRoot,
    /// The path, under `root`.
    pub path: String,
    /// The file's length in ms, when known. The engine doesn't report a media file's
    /// length, so this comes from the page (a `<video>` element reading the file) or
    /// stays `None`, and the clip is made at a stand-in length the caller updates later.
    pub length_ms: Option<u32>,
    /// The picture's own size in pixels, when known, for an overlay that keeps it.
    pub size: Option<(u32, u32)>,
}

impl DroppedFile {
    /// A file at `path` under `root`, with no length or size known.
    pub fn new(root: AssetRoot, path: impl Into<String>) -> DroppedFile {
        DroppedFile {
            root,
            path: path.into(),
            length_ms: None,
            size: None,
        }
    }

    /// Its file name: `intro.mp4`.
    pub fn name(&self) -> &str {
        file_name(&self.path)
    }

    /// What it is.
    pub fn class(&self) -> MediaClass {
        classify(&self.path)
    }

    /// The asset reference a source's `file` setting holds for it.
    pub fn reference(&self) -> serde_json::Value {
        serde_json::to_value(Reference::Asset {
            root: self.root,
            path: self.path.clone(),
        })
        .unwrap_or(serde_json::Value::Null)
    }

    /// Whether `source` is a `media` or `image` source playing this file.
    pub fn is_held_by(&self, source: &SourceInfo) -> bool {
        let media = matches!(
            &source.kind,
            SourceKind::Builtin { kind } if kind == "media" || kind == "image"
        );
        let file = source
            .settings
            .0
            .get("file")
            .and_then(Reference::from_value);
        let ours = Reference::Asset {
            root: self.root,
            path: self.path.clone(),
        };
        media && file.as_ref() == Some(&ours)
    }

    /// The settings a new source for it starts with: the file, and for a media file no
    /// looping, so its clip ends when the media does.
    fn settings(&self) -> Settings {
        let mut settings: Settings = [("file", self.reference())].into_iter().collect();
        if self.class() != MediaClass::Image {
            settings.0.insert("loop".to_owned(), json!(false));
        }
        settings
    }
}

/// The source already playing `file`, if one does.
pub fn source_for<'a>(collection: &'a Collection, file: &DroppedFile) -> Option<&'a SourceInfo> {
    collection.sources.iter().find(|s| file.is_held_by(s))
}

/// The scene an earlier drop of `file` made, to reuse: a listed scene whose only item
/// plays the file. One in the "Show media" folder is preferred.
pub fn scene_for<'a>(collection: &'a Collection, file: &DroppedFile) -> Option<&'a SceneInfo> {
    let folder = show_media_folder(collection).map(|f| &f.folder_id);
    let holds = |scene: &&SceneInfo| {
        scene.role == SceneRole::Scene
            && scene.variant_of.is_none()
            && matches!(scene.items.as_slice(), [item] if collection
                .source(&item.source_id)
                .is_some_and(|s| file.is_held_by(s)))
    };
    let mut matching = collection.scenes.iter().filter(holds);
    let first = matching.clone().next();
    matching
        .find(|s| folder.is_some() && s.folder.as_ref() == folder)
        .or(first)
}

/// The "Show media" folder, if the collection has one: a top-level one first.
pub fn show_media_folder(collection: &Collection) -> Option<&FolderInfo> {
    let named = |f: &&FolderInfo| f.name == SHOW_MEDIA_FOLDER && f.scene_id.is_none();
    collection
        .folders
        .iter()
        .filter(named)
        .find(|f| f.parent.is_none())
        .or_else(|| collection.folders.iter().find(named))
}

/// The new IDs a drop may take. The caller makes them all ([`crate::store::Studio::fresh_id`]);
/// a drop that reuses what exists leaves some unused.
#[derive(Clone, Debug, PartialEq)]
pub struct DropIds {
    /// The "Show media" folder's, if it has to be made.
    pub folder: FolderId,
    /// The new source's.
    pub source: SourceId,
    /// The new scene's.
    pub scene: SceneId,
    /// The scene's item's.
    pub item: ItemId,
    /// The clip's, within its show.
    pub clip: String,
}

/// The scene a drop puts on a V track, and the edits that make it.
#[derive(Clone, Debug, PartialEq)]
pub struct DroppedScene {
    /// The scene the clip shows.
    pub scene: SceneId,
    /// Its name, the clip's label.
    pub name: String,
    /// The source that plays the file.
    pub source: SourceId,
    /// The edits that make what didn't exist: the folder, the source, the scene and its
    /// item. Empty when an earlier drop's scene is reused.
    pub edits: Vec<Edit>,
}

/// Where the file's item sits in its scene: over the whole canvas on program, or for an
/// audio file anywhere; on an overlay, at its own size, centred and scaled down only if
/// larger than the canvas. With no size known, no placement is sent, and the engine draws
/// the source at its own pixel size from the canvas's top-left.
pub fn placement(
    collection: &Collection,
    file: &DroppedFile,
    overlay: bool,
) -> Option<ItemGeometry> {
    let canvas = collection.canvas;
    let fill = || ItemGeometry::covering_canvas(canvas.width, canvas.height).ok();
    if !overlay || file.class() == MediaClass::Audio {
        return fill();
    }
    let (w, h) = file.size?;
    crate::panels::add_source::native_geometry((w as f32, h as f32), canvas)
}

/// The edits that make the source for `file`, unless one already plays it, and its ID.
pub fn source_edits(
    collection: &Collection,
    file: &DroppedFile,
    ids: &DropIds,
) -> Result<(SourceId, Vec<Edit>), DropRefusal> {
    let Some(kind) = file.class().kind() else {
        return Err(DropRefusal::Unsupported(file.name().to_owned()));
    };
    if let Some(source) = source_for(collection, file) {
        return Ok((source.source_id.clone(), Vec::new()));
    }
    let edit = Edit::SourceCreate {
        source_id: ids.source.clone(),
        name: file.name().to_owned(),
        kind: SourceKind::Builtin {
            kind: kind.to_owned(),
        },
        settings: file.settings(),
        duplicated_from: None,
    };
    Ok((ids.source.clone(), vec![edit]))
}

/// The scene for `file` on a V track: an earlier drop's, or a new one made by one batch
/// that adds the "Show media" folder if missing, the source if no source plays the file,
/// the scene named after the file, filed in the folder, and the item placed by
/// [`placement`].
pub fn scene_edits(
    collection: &Collection,
    file: &DroppedFile,
    overlay: bool,
    ids: &DropIds,
) -> Result<DroppedScene, DropRefusal> {
    if file.class() == MediaClass::Unsupported {
        return Err(DropRefusal::Unsupported(file.name().to_owned()));
    }
    if let Some(scene) = scene_for(collection, file) {
        let source = scene
            .items
            .first()
            .map(|i| i.source_id.clone())
            .ok_or_else(|| DropRefusal::Unsupported(file.name().to_owned()))?;
        return Ok(DroppedScene {
            scene: scene.scene_id.clone(),
            name: scene.name.clone(),
            source,
            edits: Vec::new(),
        });
    }
    let mut edits = Vec::new();
    let folder = match show_media_folder(collection) {
        Some(folder) => folder.folder_id.clone(),
        None => {
            edits.push(Edit::FolderCreate {
                scene_id: None,
                folder_id: ids.folder.clone(),
                name: SHOW_MEDIA_FOLDER.to_owned(),
                parent: None,
            });
            ids.folder.clone()
        }
    };
    let (source, made) = source_edits(collection, file, ids)?;
    edits.extend(made);
    let name = scene_name(collection, file.name());
    edits.extend([
        Edit::SceneCreate {
            scene_id: ids.scene.clone(),
            name: name.clone(),
            role: None,
        },
        Edit::SceneSetFolder {
            scene_id: ids.scene.clone(),
            folder_id: Some(folder),
        },
        Edit::ItemAdd {
            scene_id: ids.scene.clone(),
            item_id: ids.item.clone(),
            source_id: source.clone(),
            transform: None,
            geometry: placement(collection, file, overlay),
            opacity: None,
            blend: None,
        },
    ]);
    Ok(DroppedScene {
        scene: ids.scene.clone(),
        name,
        source,
        edits,
    })
}

/// A dropped file's clip length: the file's own for video and audio, else `image_ms`
/// (the show's image length). A media file whose length isn't known yet takes
/// `image_ms` too, until its length is known.
pub fn clip_len_ms(file: &DroppedFile, image_ms: u32) -> u32 {
    match file.class() {
        MediaClass::Video | MediaClass::Audio => file.length_ms.unwrap_or(image_ms),
        MediaClass::Image | MediaClass::Unsupported => image_ms,
    }
}

/// Whether a clip of `class` dropped on `track` ends in a pausing bookmark: on V1, unless
/// it is timed, since a timed clip ends with its media.
pub fn ends_in_pause(class: MediaClass, track: TrackKind) -> bool {
    track == TrackKind::Program && !class.is_timed()
}

/// Where a file was dropped: on which track, at what time, in a show whose images last
/// `image_ms`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropTarget {
    /// The track it landed on.
    pub track: TrackKind,
    /// Where on it, in show time.
    pub at_ms: u32,
    /// The show's image clip length.
    pub image_ms: u32,
}

impl DropTarget {
    /// A drop on `track` at `at_ms`, in a show with the default image length.
    pub fn new(track: TrackKind, at_ms: u32) -> DropTarget {
        DropTarget {
            track,
            at_ms,
            image_ms: DEFAULT_IMAGE_MS,
        }
    }
}

/// What a drop does: the batch that makes its scene or source, and the clip to place.
#[derive(Clone, Debug, PartialEq)]
pub struct DropPlan {
    /// The edits, sent as one batch (one undo step). Empty when everything is reused.
    pub edits: Vec<Edit>,
    /// The clip, at the drop's time. Placing it in the show is the show protocol's.
    pub clip: ClipView,
    /// The source that plays the file.
    pub source: SourceId,
    /// Whether a pausing bookmark goes at the clip's end.
    pub pause_at_end: bool,
    /// Whether the clip's length is a stand-in, to update once the file's length is
    /// known (a media file dropped with no length).
    pub length_pending: bool,
}

/// Why a drop does nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropRefusal {
    /// The file, by name, is not a video, audio or image file JStream plays.
    Unsupported(String),
    /// The file, by name, can't go on that kind of track: a picture on an audio track,
    /// or anything on the keyframe or sequence tracks.
    WrongTrack(String, TrackKind),
}

impl fmt::Display for DropRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DropRefusal::Unsupported(name) => {
                write!(
                    f,
                    "{name} isn't a video, audio or image file JStream can play."
                )
            }
            DropRefusal::WrongTrack(name, track) => {
                write!(f, "{name} can't go on {}.", track.tag())
            }
        }
    }
}

/// What dropping `file` on `target` does: on a V track, a scene (made or reused) and its
/// clip; on an audio track, a source (made or reused) and its audio clip with default
/// fades. A video dropped on an audio track plays its sound there. Anything else is
/// refused.
pub fn plan_drop(
    collection: &Collection,
    file: &DroppedFile,
    target: DropTarget,
    ids: &DropIds,
) -> Result<DropPlan, DropRefusal> {
    let class = file.class();
    if class == MediaClass::Unsupported {
        return Err(DropRefusal::Unsupported(file.name().to_owned()));
    }
    let wrong = || DropRefusal::WrongTrack(file.name().to_owned(), target.track);
    let len_ms = clip_len_ms(file, target.image_ms);
    let clip = |label: String, kind: ClipKind, fades: Option<(u32, u32)>| ClipView {
        id: ids.clip.clone(),
        start_ms: target.at_ms,
        len_ms,
        label,
        detail: format!("{} · {}", time::clock(len_ms), class.word()),
        kind,
        transition_in: None,
        timed: class.is_timed(),
        missing: false,
        fades,
        curve: Vec::new(),
    };
    let length_pending = class.is_timed() && file.length_ms.is_none();
    match target.track {
        TrackKind::Program | TrackKind::Overlay(_) => {
            let overlay = matches!(target.track, TrackKind::Overlay(_));
            let made = scene_edits(collection, file, overlay, ids)?;
            Ok(DropPlan {
                clip: clip(made.name, ClipKind::Scene(made.scene.to_string()), None),
                edits: made.edits,
                source: made.source,
                pause_at_end: ends_in_pause(class, target.track),
                length_pending,
            })
        }
        TrackKind::Audio(_) if class.is_timed() => {
            let (source, edits) = source_edits(collection, file, ids)?;
            let label = collection
                .source(&source)
                .map_or_else(|| file.name().to_owned(), |s| s.name.clone());
            Ok(DropPlan {
                clip: clip(
                    label,
                    ClipKind::Audio(source.to_string()),
                    Some((DEFAULT_FADE_MS, DEFAULT_FADE_MS)),
                ),
                edits,
                source,
                pause_at_end: false,
                length_pending,
            })
        }
        TrackKind::Audio(_)
        | TrackKind::Keyframes
        | TrackKind::Sequences(_)
        | TrackKind::Linked(_) => Err(wrong()),
    }
}

/// The local paths in a `text/uri-list`: each `file://` line, percent-decoded. A Windows
/// path (`file:///C:/clips/a.mp4`) loses the slash before its drive letter. Comments and
/// other schemes are skipped.
pub fn paths_from_uri_list(list: &str) -> Vec<String> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("file://"))
        .map(|rest| rest.strip_prefix("localhost").unwrap_or(rest))
        .filter_map(percent_decode)
        .map(|path| {
            let drive = path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
                && path.as_bytes().get(2) == Some(&b':');
            if path.starts_with('/') && drive {
                path[1..].to_owned()
            } else {
                path
            }
        })
        .filter(|path| !path.is_empty())
        .collect()
}

/// `%xx` escapes decoded as UTF-8; `None` for a bad escape or bytes that aren't UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The local paths a drop event carries, read from its `dataTransfer`'s `text/uri-list`:
/// what a file manager puts there when a file is dragged into a browser. Empty when the
/// drop carries none.
///
/// This is all a page can learn of a dropped file's place on disk. A browser hands the
/// page `File` objects with a name but no path, so a drop from a source that writes no
/// URI list (Windows Explorer, macOS Finder into most browsers) yields nothing here. The
/// desktop app's window handles drops natively and passes their paths through
/// `host::on_file_drop` instead, so there this is empty.
pub fn dropped_paths(event: &JsValue) -> Vec<String> {
    use leptos::wasm_bindgen::JsCast;
    use leptos::web_sys::js_sys::{Function, Reflect};

    let read = || -> Option<String> {
        let transfer = Reflect::get(event, &JsValue::from_str("dataTransfer"))
            .ok()
            .filter(JsValue::is_object)?;
        let get = Reflect::get(&transfer, &JsValue::from_str("getData"))
            .ok()?
            .dyn_into::<Function>()
            .ok()?;
        get.call1(&transfer, &JsValue::from_str("text/uri-list"))
            .ok()?
            .as_string()
    };
    read()
        .map(|list| paths_from_uri_list(&list))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::store::{apply, query};
    use jstream_control::MockEngine;
    use jstream_control::protocol::{FitMode, Query};
    use std::pin::pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    /// A mock engine with one scene named "intro.mp4", and its session.
    fn engine() -> Session {
        let engine = MockEngine::new();
        let session = now(Session::open(Rc::new(engine.connect())))
            .unwrap_or_else(|e| panic!("hello failed: {e}"));
        let edits = vec![Edit::SceneCreate {
            scene_id: "main".into(),
            name: "intro.mp4".into(),
            role: None,
        }];
        now(apply(&session, edits, None)).unwrap_or_else(|e| panic!("batch failed: {e}"));
        session
    }

    fn state(session: &Session) -> Collection {
        now(query(session, Query::StateGet))
            .ok()
            .and_then(Collection::from_result)
            .unwrap_or_else(|| panic!("no state"))
    }

    fn ids(n: u32) -> DropIds {
        DropIds {
            folder: FolderId::new(format!("folder-{n}")),
            source: SourceId::new(format!("source-{n}")),
            scene: SceneId::new(format!("scene-{n}")),
            item: ItemId::new(format!("item-{n}")),
            clip: format!("clip-{n}"),
        }
    }

    fn file(path: &str, length_ms: Option<u32>) -> DroppedFile {
        DroppedFile {
            length_ms,
            ..DroppedFile::new(AssetRoot::Absolute, path)
        }
    }

    fn plan(c: &Collection, f: &DroppedFile, track: TrackKind, n: u32) -> DropPlan {
        plan_drop(c, f, DropTarget::new(track, 5_000), &ids(n))
            .unwrap_or_else(|e| panic!("refused: {e}"))
    }

    #[test]
    fn files_are_classed_by_extension_ignoring_case() {
        assert_eq!(classify("/clips/intro.MP4"), MediaClass::Video);
        assert_eq!(classify("C:\\music\\bed.flac"), MediaClass::Audio);
        assert_eq!(classify("logo.WebP"), MediaClass::Image);
        assert_eq!(classify("logo.svg"), MediaClass::Unsupported);
        assert_eq!(classify("README"), MediaClass::Unsupported);
        assert_eq!(classify("/dir.mp4/notes"), MediaClass::Unsupported);
        assert_eq!(file_name("C:\\a\\b.png"), "b.png");
    }

    #[test]
    fn timing_lengths_and_pauses_follow_the_class() {
        assert!(MediaClass::Video.is_timed() && MediaClass::Audio.is_timed());
        assert!(!MediaClass::Image.is_timed());
        assert_eq!(clip_len_ms(&file("a.mp4", Some(12_345)), 10_000), 12_345);
        assert_eq!(clip_len_ms(&file("a.mp4", None), 7_000), 7_000);
        assert_eq!(clip_len_ms(&file("a.png", Some(1)), 7_000), 7_000);
        assert!(ends_in_pause(MediaClass::Image, TrackKind::Program));
        assert!(!ends_in_pause(MediaClass::Video, TrackKind::Program));
        assert!(!ends_in_pause(MediaClass::Audio, TrackKind::Program));
        assert!(!ends_in_pause(MediaClass::Image, TrackKind::Overlay(2)));
    }

    #[test]
    fn a_video_on_v1_makes_a_filed_scene_filling_the_frame_in_one_batch() {
        let session = engine();
        let c = state(&session);
        let f = file("/clips/intro.mp4", Some(60_000));
        let p = plan(&c, &f, TrackKind::Program, 1);
        assert_eq!(p.edits.len(), 5, "folder, source, scene, folder, item");
        assert!(
            matches!(&p.edits[0], Edit::FolderCreate { name, .. } if name == SHOW_MEDIA_FOLDER)
        );
        let Some(Edit::ItemAdd { geometry, .. }) = p.edits.last() else {
            panic!("no item");
        };
        let canvas = ItemGeometry::covering_canvas(c.canvas.width, c.canvas.height).ok();
        assert_eq!(*geometry, canvas);
        assert_eq!(p.clip.label, "intro.mp4 2", "a scene already has the name");
        assert_eq!((p.clip.start_ms, p.clip.len_ms), (5_000, 60_000));
        assert_eq!(p.clip.kind, ClipKind::Scene("scene-1".into()));
        assert_eq!(p.clip.detail, "1:00 · video");
        assert!(p.clip.timed && !p.pause_at_end && !p.length_pending);

        now(apply(&session, p.edits, None)).unwrap_or_else(|e| panic!("batch failed: {e}"));
        let c = state(&session);
        let scene = c
            .scene(&SceneId::new("scene-1"))
            .unwrap_or_else(|| panic!("no scene"));
        let folder = show_media_folder(&c).unwrap_or_else(|| panic!("no folder"));
        assert_eq!(scene.folder.as_ref(), Some(&folder.folder_id));
        let source = c
            .source(&SourceId::new("source-1"))
            .unwrap_or_else(|| panic!("no source"));
        assert!(f.is_held_by(source));
        assert_eq!(source.settings.0.get("loop"), Some(&json!(false)));

        // The same file again reuses the scene, with nothing to make.
        let again = plan(&c, &f, TrackKind::Program, 2);
        assert!(again.edits.is_empty());
        assert_eq!(again.clip.kind, ClipKind::Scene("scene-1".into()));
        assert_eq!(again.source, SourceId::new("source-1"));

        // Another file goes into the folder that now exists.
        let logo = file("/art/logo.png", None);
        let next = plan(&c, &logo, TrackKind::Program, 3);
        assert!(
            !next
                .edits
                .iter()
                .any(|e| matches!(e, Edit::FolderCreate { .. }))
        );
        assert!(next.pause_at_end && !next.clip.timed && !next.length_pending);
        assert_eq!(next.clip.len_ms, DEFAULT_IMAGE_MS);
        now(apply(&session, next.edits, None)).unwrap_or_else(|e| panic!("batch failed: {e}"));
        let c = state(&session);
        let scene = c
            .scene(&SceneId::new("scene-3"))
            .unwrap_or_else(|| panic!("no scene"));
        assert_eq!(scene.folder.as_ref(), Some(&folder.folder_id));
    }

    #[test]
    fn an_image_on_an_overlay_keeps_its_own_size() {
        let session = engine();
        let c = state(&session);
        let mut logo = file("/art/logo.png", None);
        logo.size = Some((300, 100));
        let p = plan(&c, &logo, TrackKind::Overlay(2), 1);
        let geometry = p.edits.iter().find_map(|e| match e {
            Edit::ItemAdd { geometry, .. } => Some(*geometry),
            _ => None,
        });
        let geometry = geometry.flatten().unwrap_or_else(|| panic!("no geometry"));
        assert_eq!(
            (geometry.size.width.get(), geometry.size.height.get()),
            (300.0, 100.0)
        );
        assert_eq!(geometry.fit_mode, FitMode::Fit);
        assert!(!p.pause_at_end);
        assert!(p.edits.iter().any(|e| matches!(
            e,
            Edit::SourceCreate { kind: SourceKind::Builtin { kind }, .. } if kind == "image"
        )));
        // With no size known, the engine draws it at its own pixel size.
        let bare = file("/art/logo.png", None);
        assert_eq!(placement(&c, &bare, true), None);
    }

    #[test]
    fn audio_on_an_audio_track_is_a_faded_source_clip_and_no_scene() {
        let session = engine();
        let c = state(&session);
        let bed = file("/music/bed.mp3", None);
        let p = plan(&c, &bed, TrackKind::Audio(1), 1);
        assert_eq!(p.edits.len(), 1);
        assert!(matches!(&p.edits[0], Edit::SourceCreate { .. }));
        assert_eq!(p.clip.kind, ClipKind::Audio("source-1".into()));
        assert_eq!(p.clip.fades, Some((DEFAULT_FADE_MS, DEFAULT_FADE_MS)));
        assert!(p.clip.timed && p.length_pending && !p.pause_at_end);
        now(apply(&session, p.edits, None)).unwrap_or_else(|e| panic!("batch failed: {e}"));
        let c = state(&session);
        let again = plan(&c, &bed, TrackKind::Audio(2), 2);
        assert!(again.edits.is_empty());
        assert_eq!(again.clip.kind, ClipKind::Audio("source-1".into()));
        // Dropped on V1, the same file makes a scene with only that sound, from the
        // same source.
        let v = plan(&c, &bed, TrackKind::Program, 3);
        assert!(
            !v.edits
                .iter()
                .any(|e| matches!(e, Edit::SourceCreate { .. }))
        );
        assert!(v.edits.iter().any(|e| matches!(
            e,
            Edit::ItemAdd { source_id, .. } if source_id.as_str() == "source-1"
        )));
    }

    #[test]
    fn wrong_files_and_tracks_are_refused() {
        let c = Collection::default();
        let refuse = |path: &str, track: TrackKind| {
            plan_drop(&c, &file(path, None), DropTarget::new(track, 0), &ids(1)).err()
        };
        assert_eq!(
            refuse("notes.txt", TrackKind::Program),
            Some(DropRefusal::Unsupported("notes.txt".into()))
        );
        assert_eq!(
            refuse("logo.png", TrackKind::Audio(1)),
            Some(DropRefusal::WrongTrack(
                "logo.png".into(),
                TrackKind::Audio(1)
            ))
        );
        assert!(refuse("intro.mp4", TrackKind::Keyframes).is_some());
        assert!(refuse("intro.mp4", TrackKind::Sequences(1)).is_some());
        assert!(refuse("intro.mp4", TrackKind::Audio(1)).is_none());
        assert_eq!(
            DropRefusal::WrongTrack("logo.png".into(), TrackKind::Audio(1)).to_string(),
            "logo.png can't go on A1."
        );
    }

    #[test]
    fn uri_lists_give_local_paths() {
        let list = "# from a file manager\r\nfile:///home/me/My%20Clips/intro.mp4\r\n\
                    https://example.com/a.png\r\nfile://localhost/tmp/b.wav\r\n\
                    file:///C:/clips/c%C3%A9.mov\r\nfile:///bad%zz\r\n";
        assert_eq!(
            paths_from_uri_list(list),
            vec![
                "/home/me/My Clips/intro.mp4".to_owned(),
                "/tmp/b.wav".to_owned(),
                "C:/clips/cé.mov".to_owned(),
            ]
        );
    }
}
