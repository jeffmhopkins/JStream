//! The Settings dialog's Audio plugins page (ADR-0035, protocol 0.95): where the engine
//! looks for CLAP audio effects, the ones its last scan found, the files it couldn't
//! scan and why, and Rescan, which scans again without a restart.
//!
//! - **Calls.** `plugin.clap_scan` reads the scan when the page opens and after a
//!   rescan; `plugin.clap_rescan` scans again, and answers once the scan ends. A rescan
//!   re-reads `kind.list` too, so a plugin it found is offered in Add filter at once.
//! - **An engine that hosts no CLAP plugins** says so, with the engine's reason.
//! - **Names and paths** are the plugin's and the machine's text, shown as data.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#beyond

use crate::app::SessionContext;
use crate::store::Studio;
use jstream_control::Control;
use jstream_control::protocol::{Action, Call, CallResult, ClapScanInfo, Query, QueryResult};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The scan (`plugin.clap_scan`).
pub async fn scan(control: &dyn Control) -> Result<ClapScanInfo, String> {
    let call = Call::Query {
        query: Query::PluginClapScan,
    };
    match control.call(call).await.map_err(|e| e.to_string())?.result {
        CallResult::Query {
            result: QueryResult::PluginClapScan { scan },
        } => Ok(scan),
        _ => Err("the engine answered `plugin.clap_scan` with something else".into()),
    }
}

/// Scans again (`plugin.clap_rescan`), answering once the scan ends.
pub async fn rescan(control: &dyn Control) -> Result<(), String> {
    let call = Call::Action {
        action: Action::PluginClapRescan,
    };
    control
        .call(call)
        .await
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The line over the lists: how many effects the scan found, and how many files failed.
pub fn summary(scan: &ClapScanInfo) -> String {
    if !scan.scanned {
        return if scan.scanning {
            "Scanning…".into()
        } else {
            "Not scanned yet.".into()
        };
    }
    let found = match scan.found.len() {
        0 => "No CLAP plugins found".to_owned(),
        1 => "1 CLAP plugin found".to_owned(),
        n => format!("{n} CLAP plugins found"),
    };
    match scan.failed.len() {
        0 => format!("{found}."),
        1 => format!("{found}; 1 file couldn't be read."),
        n => format!("{found}; {n} files couldn't be read."),
    }
}

/// Where to put a plugin: the first folder searched, or `None` when there are none.
pub fn install_folder(scan: &ClapScanInfo) -> Option<&str> {
    scan.folders.first().map(|f| f.path.as_str())
}

/// The Audio plugins page.
#[component]
pub fn ClapPlugins() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return ().into_any();
    };
    let studio = use_context::<Studio>();
    let info = RwSignal::new(None::<ClapScanInfo>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let reload = move || {
        let Some(current) = session.get() else {
            return;
        };
        spawn_local(async move {
            match scan(&*current.control).await {
                Ok(scan) => {
                    info.try_set(Some(scan));
                }
                Err(error) => {
                    problem.try_set(Some(error));
                }
            }
        });
    };
    reload();
    let rescan_now = move |_| {
        let Some(current) = session.get() else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        problem.set(None);
        spawn_local(async move {
            let done = rescan(&*current.control).await;
            if let Some(studio) = studio {
                studio.relist_kinds(&current).await;
            }
            busy.try_set(false);
            match done {
                Ok(()) => reload(),
                Err(error) => {
                    problem.try_set(Some(error));
                }
            }
        });
    };

    let body = move || {
        let scan = info.get()?;
        if !scan.available {
            let reason = scan
                .reason
                .clone()
                .unwrap_or_else(|| "This engine doesn't host CLAP plugins.".into());
            return Some(
                view! {
                    <p class="placeholder">"CLAP audio plugins aren't available here: " {reason}</p>
                }
                .into_any(),
            );
        }
        let hint = install_folder(&scan).map(|folder| {
            view! {
                <p class="clap-hint">
                    "Install a CLAP plugin by putting its " <code>".clap"</code> " file in "
                    <span class="mono">{folder.to_owned()}</span>
                    " or another folder below, then press Rescan."
                </p>
            }
        });
        let folders = scan
            .folders
            .iter()
            .map(|f| {
                view! {
                    <li class="clap-folder">
                        <span class="mono" title=f.path.clone()>{f.path.clone()}</span>
                        <span class="clap-tag" class:missing=!f.exists>
                            {if f.exists { "searched" } else { "doesn't exist yet" }}
                        </span>
                    </li>
                }
            })
            .collect_view();
        let found = scan
            .found
            .iter()
            .map(|p| {
                view! {
                    <li class="clap-plugin">
                        <span class="plugin-name">{p.name.clone()}</span>
                        <span class="plugin-meta mono" title=p.file.clone()>
                            {format!("{} · {}", p.kind_id, p.file)}
                        </span>
                    </li>
                }
            })
            .collect_view();
        let failed = scan
            .failed
            .iter()
            .map(|f| {
                view! {
                    <li class="clap-failed">
                        <span class="plugin-meta mono" title=f.file.clone()>{f.file.clone()}</span>
                        <span class="plugin-state">{f.reason.clone()}</span>
                    </li>
                }
            })
            .collect_view();
        let any_found = !scan.found.is_empty();
        let any_failed = !scan.failed.is_empty();
        Some(
            view! {
                {hint}
                <h4 class="clap-head">"Folders searched"</h4>
                <ul class="clap-list">{folders}</ul>
                <p class="clap-summary" role="status">{summary(&scan)}</p>
                {any_found.then(|| view! {
                    <h4 class="clap-head">"Found"</h4>
                    <ul class="clap-list">{found}</ul>
                })}
                {any_failed.then(|| view! {
                    <h4 class="clap-head">"Couldn't be read"</h4>
                    <ul class="clap-list">{failed}</ul>
                })}
            }
            .into_any(),
        )
    };

    view! {
        <section class="clap-plugins">
            <p class="settings-note">
                "CLAP audio effects found here are offered under Plugins when you add a filter to an audio chain."
            </p>
            <Show when=move || info.with(|i| i.as_ref().is_some_and(|s| s.available))>
                <button
                    class="primary"
                    aria-busy=move || busy.get().to_string()
                    on:click=rescan_now
                >
                    {move || if busy.get() { "Scanning…" } else { "Rescan" }}
                </button>
            </Show>
            {move || problem.get().map(|problem| view! {
                <p class="problem" role="alert">{problem}</p>
            })}
            {body}
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{install_folder, summary};
    use jstream_control::protocol::{ClapFailed, ClapFolder, ClapFound, ClapScanInfo};

    fn scan(found: usize, failed: usize) -> ClapScanInfo {
        ClapScanInfo::available(
            vec![
                ClapFolder::new("C:\\CLAP", true),
                ClapFolder::new("D:\\CLAP", false),
            ],
            false,
            true,
            (0..found)
                .map(|i| ClapFound::new(format!("clap/p{i}"), format!("P{i}"), "x.clap"))
                .collect(),
            (0..failed)
                .map(|_| ClapFailed::new("bad.clap", "it crashed"))
                .collect(),
        )
    }

    #[test]
    fn the_summary_counts_what_was_found_and_what_failed() {
        assert_eq!(summary(&scan(0, 0)), "No CLAP plugins found.");
        assert_eq!(summary(&scan(1, 0)), "1 CLAP plugin found.");
        assert_eq!(
            summary(&scan(3, 2)),
            "3 CLAP plugins found; 2 files couldn't be read."
        );
        assert_eq!(
            summary(&scan(0, 1)),
            "No CLAP plugins found; 1 file couldn't be read."
        );
        let mut first = ClapScanInfo::available(Vec::new(), true, false, Vec::new(), Vec::new());
        assert_eq!(summary(&first), "Scanning…");
        first.scanning = false;
        assert_eq!(summary(&first), "Not scanned yet.");
    }

    #[test]
    fn plugins_go_in_the_first_folder_searched() {
        assert_eq!(install_folder(&scan(0, 0)), Some("C:\\CLAP"));
        assert_eq!(
            install_folder(&ClapScanInfo::unavailable(None)),
            None,
            "an engine without CLAP has nowhere to put one"
        );
    }
}
