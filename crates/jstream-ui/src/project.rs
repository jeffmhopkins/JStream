//! Projects in the File menu (ADR-0121): New project…, Open project…, Save project
//! as…, the recent projects, and the open project's name.
//!
//! The engine says which file it saves to (`collection.info`) and tells every client
//! when that changes (`collection.file_changed`), so the menu follows a switch made by
//! another client or an agent. An engine with no collection file answers `unsupported`,
//! and the menu then offers none of this.
//!
//! A path is a path on the engine's machine. The dialog takes it typed, and offers
//! Browse… where the page runs in the desktop app connected to its own engine.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::app::SessionContext;
use crate::browse::{BrowseKind, browse_button, file_name};
use crate::dialog::Modal;
use crate::store::Studio;
use jstream_control::protocol::{Action, Call, CallResult, Query, QueryResult};
use jstream_control::{Control, ControlError};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The project the engine saves to, and the ones opened before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Project {
    /// The open project's file, on the engine's machine.
    pub path: String,
    /// The projects opened most recently, most recent first, the open one among them.
    pub recent: Vec<String>,
}

/// The open project, provided by the shell: `None` until `collection.info` answers, and
/// while the engine has no collection file.
#[derive(Clone, Copy)]
pub struct OpenProject(pub RwSignal<Option<Project>>);

impl OpenProject {
    /// An empty one.
    pub fn new() -> Self {
        OpenProject(RwSignal::new(None))
    }
}

impl Default for OpenProject {
    fn default() -> Self {
        Self::new()
    }
}

/// The folder `collection` asset paths are relative to: the open project's, or else the
/// one the desktop app named at start. `open` is read from the context when the caller
/// is drawn, since an event handler has none.
pub fn collection_folder(open: Option<OpenProject>) -> Option<String> {
    open.and_then(|open| {
        open.0
            .with_untracked(|p| p.as_ref().map(|p| folder_of(&p.path)))
    })
    .filter(|folder| !folder.is_empty())
    .or_else(|| crate::host::desktop_host().and_then(|host| host.collection))
}

/// What a project is called in the menu: its file name without `.json`.
pub fn project_name(path: &str) -> String {
    let name = file_name(path);
    match name.len().checked_sub(5) {
        Some(at) if name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(".json") => {
            name[..at].to_owned()
        }
        _ => name,
    }
}

/// The folder `path` is in, with no trailing separator; empty for a bare name.
pub fn folder_of(path: &str) -> String {
    let path = path.trim().trim_end_matches(['/', '\\']);
    match path.rfind(['/', '\\']) {
        Some(0) => path[..1].to_owned(),
        Some(at) => path[..at].to_owned(),
        None => String::new(),
    }
}

/// `name` in `folder`, with the folder's own separator.
pub fn in_folder(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        return name.to_owned();
    }
    let sep = if folder.contains('\\') { '\\' } else { '/' };
    let folder = folder.trim_end_matches(['/', '\\']);
    format!("{folder}{sep}{name}")
}

/// `path` as typed, trimmed, with `.json` added when its file name has no extension.
pub fn with_extension(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || file_name(path).contains('.') {
        return path.to_owned();
    }
    format!("{path}.json")
}

/// What the dialog does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectAsk {
    /// `collection.new`.
    New,
    /// `collection.open`.
    Open,
    /// `collection.save_as`.
    SaveAs,
}

impl ProjectAsk {
    fn title(self) -> &'static str {
        match self {
            ProjectAsk::New => "New project",
            ProjectAsk::Open => "Open project",
            ProjectAsk::SaveAs => "Save project as",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            ProjectAsk::New => "Create",
            ProjectAsk::Open => "Open",
            ProjectAsk::SaveAs => "Save",
        }
    }

    /// The path the field starts with, beside the open project.
    pub fn suggested(self, open: Option<&str>) -> String {
        let folder = open.map(folder_of).unwrap_or_default();
        match self {
            ProjectAsk::New => in_folder(&folder, "Untitled.json"),
            ProjectAsk::Open => String::new(),
            ProjectAsk::SaveAs => {
                let name = open.map(project_name).unwrap_or_else(|| "Project".into());
                in_folder(&folder, &format!("{name} copy.json"))
            }
        }
    }

    /// The call it sends for `path`.
    pub fn action(self, path: String) -> Action {
        match self {
            ProjectAsk::New => Action::CollectionNew { path },
            ProjectAsk::Open => Action::CollectionOpen { path },
            ProjectAsk::SaveAs => Action::CollectionSaveAs { path },
        }
    }

    /// What a switch costs, when outputs are running: an open and a new stop them.
    pub fn warning(self, running: usize) -> Option<String> {
        if self == ProjectAsk::SaveAs {
            return None;
        }
        let stops = match running {
            0 => String::new(),
            1 => "It stops the output that is running, and it ".into(),
            n => format!("It stops the {n} outputs that are running, and it "),
        };
        let lead = if stops.is_empty() { "It " } else { "" };
        Some(format!(
            "{stops}{lead}clears undo history for everyone connected. The open project keeps every change made so far."
        ))
    }
}

/// `collection.info`, or `None` when the engine has no collection file or is older
/// than 0.94.
pub async fn info(control: &dyn Control) -> Option<Project> {
    let answer = control
        .call(Call::Query {
            query: Query::CollectionInfo,
        })
        .await
        .ok()?;
    match answer.result {
        CallResult::Query {
            result: QueryResult::CollectionInfo { path, recent },
        } => Some(Project { path, recent }),
        _ => None,
    }
}

/// Sends `ask` for `path` and says what went wrong, if anything.
async fn send(control: &dyn Control, ask: ProjectAsk, path: String) -> Result<(), String> {
    control
        .call(Call::Action {
            action: ask.action(path),
        })
        .await
        .map(drop)
        .map_err(|error| describe(ask, &error))
}

/// A refused call, as the dialog says it.
fn describe(ask: ProjectAsk, error: &ControlError) -> String {
    match error
        .protocol()
        .map(jstream_control::protocol::ProtocolError::code)
    {
        Some("already_exists") if ask != ProjectAsk::Open => {
            "A file is already there. Projects are only saved to new files, so pick another name."
                .into()
        }
        _ => error.to_string(),
    }
}

/// Opens `path` at once, as Open recent does when nothing is running.
pub fn open_now(session: Option<crate::session::Session>, studio: Studio, path: String) {
    let Some(session) = session else {
        return;
    };
    let name = project_name(&path);
    spawn_local(async move {
        match send(&*session.control, ProjectAsk::Open, path).await {
            Ok(()) => studio.notify(format!("Opened {name}")),
            Err(problem) => {
                studio.problem.try_set(Some(problem));
            }
        }
    });
}

/// The New, Open and Save as dialog.
#[component]
pub fn ProjectDialog(
    /// What it does.
    ask: ProjectAsk,
    /// The path the field starts with.
    start: String,
    /// How many outputs are running, which an open or a new stops.
    running: usize,
    /// Closes the dialog.
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let session = use_context::<SessionContext>().and_then(|s| s.get());
    let studio = expect_context::<Studio>();
    let path = RwSignal::new(start);
    let busy = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let control = StoredValue::new_local(session.map(|session| session.control));
    let submit = move || {
        let target = with_extension(&path.get_untracked());
        if target.is_empty() || busy.get_untracked() {
            return;
        }
        let Some(control) = control.get_value() else {
            return;
        };
        busy.set(true);
        problem.set(None);
        spawn_local(async move {
            let name = project_name(&target);
            let sent = send(&*control, ask, target).await;
            busy.try_set(false);
            match sent {
                Ok(()) => {
                    studio.notify(match ask {
                        ProjectAsk::New => format!("Created {name}"),
                        ProjectAsk::Open => format!("Opened {name}"),
                        ProjectAsk::SaveAs => format!("Saved as {name}"),
                    });
                    close();
                }
                Err(reason) => {
                    problem.try_set(Some(reason));
                }
            }
        });
    };
    let kind = if ask == ProjectAsk::Open {
        BrowseKind::Open
    } else {
        BrowseKind::Save
    };
    let browse = browse_button(
        kind,
        vec!["json".into()],
        ask.title().to_owned(),
        || true,
        move || file_name(&path.get_untracked()),
        move || folder_of(&path.get_untracked()),
        move |picked: String| path.set(picked),
    );
    let warning = ask.warning(running);
    view! {
        <Modal label=ask.title() class="project-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">{ask.title()}</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="project-body">
                <label class="project-path">
                    <span>"Project file on the engine's computer"</span>
                    <span class="project-path-row">
                        <input
                            type="text"
                            aria-label="Project file"
                            placeholder="/path/to/show.json"
                            prop:value=move || path.get()
                            on:input=move |event| path.set(event_target_value(&event))
                        />
                        {browse}
                    </span>
                </label>
                {(ask != ProjectAsk::Open).then(|| view! {
                    <p class="project-note">"A project is saved only to a new file, never over one."</p>
                })}
                {warning.map(|text| view! { <p class="project-note project-warning">{text}</p> })}
                {move || problem.get().map(|text| view! { <p class="project-problem" role="alert">{text}</p> })}
            </div>
            <div class="modal-footer">
                <button type="button" on:click=move |_| close()>"Cancel"</button>
                <button
                    type="button"
                    class="primary"
                    data-primary
                    disabled=move || busy.get() || path.with(|p| p.trim().is_empty())
                    on:click=move |_| submit()
                >
                    {ask.verb()}
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_named_by_its_file() {
        assert_eq!(project_name("/home/me/shows/Talk.json"), "Talk");
        assert_eq!(project_name(r"C:\Shows\Weekly.JSON"), "Weekly");
        assert_eq!(project_name("/home/me/notes"), "notes");
        assert_eq!(project_name("/x/.json"), "");
    }

    #[test]
    fn paths_get_a_json_extension_and_a_folder() {
        assert_eq!(with_extension(" /a/show "), "/a/show.json");
        assert_eq!(with_extension("/a/show.json"), "/a/show.json");
        assert_eq!(with_extension("/a/show.v2"), "/a/show.v2");
        assert_eq!(with_extension(""), "");
        assert_eq!(folder_of("/a/b/show.json"), "/a/b");
        assert_eq!(folder_of(r"C:\Shows\show.json"), r"C:\Shows");
        assert_eq!(folder_of("/show.json"), "/");
        assert_eq!(in_folder(r"C:\Shows", "a.json"), r"C:\Shows\a.json");
        assert_eq!(in_folder("/", "a.json"), "/a.json");
    }

    #[test]
    fn dialogs_start_beside_the_open_project() {
        let open = Some("/home/me/shows/talk.json");
        assert_eq!(
            ProjectAsk::SaveAs.suggested(open),
            "/home/me/shows/talk copy.json"
        );
        assert_eq!(
            ProjectAsk::New.suggested(open),
            "/home/me/shows/Untitled.json"
        );
        assert_eq!(ProjectAsk::Open.suggested(open), "");
        assert_eq!(
            ProjectAsk::SaveAs.action("/p.json".into()),
            Action::CollectionSaveAs {
                path: "/p.json".into()
            }
        );
    }

    #[test]
    fn a_switch_says_what_it_stops() {
        assert_eq!(ProjectAsk::SaveAs.warning(3), None);
        assert!(
            ProjectAsk::Open
                .warning(2)
                .is_some_and(|w| w.starts_with("It stops the 2 outputs"))
        );
        assert!(
            ProjectAsk::New
                .warning(0)
                .is_some_and(|w| w.starts_with("It clears undo history"))
        );
    }
}
