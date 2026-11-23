//! "Control PTZ cameras" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! §4, [ADR-0146](../../../docs/adr/0146-ptz-cameras-shots-and-actions.md) §1,
//! [ADR-0209](../../../docs/adr/0209-value-bindings-aim-ptz-cameras.md) decision 1), on the
//! Settings dialog's Devices page (`settings.rs`), in the desktop app only. The PTZ panel's
//! Add camera… opens it (`panels/ptz.rs`).
//!
//! - **The switch** lets JStream drive this machine's PTZ heads. Off until the person
//!   turns it on.
//! - **The cameras** are listed by name, with their address and port, transport and VISCA
//!   address, each with Edit and Remove. The form below takes a name, an address and port
//!   (a LAN address), a transport and a VISCA address; Edit fills it with a camera's, and
//!   its button then reads Save camera. A name is what scenes' shots, `ptz.*` actions and
//!   mappings name the head by, so renaming a camera leaves them naming the old name.
//! - **Saving.** Each change asks the app to save the whole setting
//!   (`host::set_ptz_cameras`, the app's own command, which no engine client reaches);
//!   the app checks it as the engine would, and the page shows why it refused one,
//!   undoing the change. The app applies a saved list at once; when a camera's
//!   connection can't be opened, the list stays saved and the page says why.
//!
//! Outside the desktop app the list is the daemon's `--ptz-camera` flags, and the page
//! shows nothing.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::host::{PtzCameraEntry, PtzCameraSetting};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::cell::RefCell;

/// The transports the form offers: their wire names and labels.
pub const TRANSPORTS: [(&str, &str); 3] = [
    ("visca_ip", "VISCA over IP (UDP)"),
    ("udp", "Raw VISCA over UDP"),
    ("tcp", "Raw VISCA over TCP"),
];

/// The port VISCA over IP listens on, which a typed address without one gets.
pub const VISCA_IP_PORT: u16 = 52381;

thread_local! {
    /// The setting as last saved, so the page shows it again when the dialog reopens.
    /// `None` until the page first reads the app's.
    static SAVED: RefCell<Option<PtzCameraSetting>> = const { RefCell::new(None) };
}

/// The setting the page starts from: the one last saved in this window, or the one the
/// app read at start. `None` outside the desktop app.
pub fn current() -> Option<PtzCameraSetting> {
    let started = crate::host::desktop_ptz_cameras()?;
    Some(
        SAVED
            .with(|saved| saved.borrow().clone())
            .unwrap_or(started),
    )
}

/// How a transport reads.
pub fn transport_label(transport: &str) -> String {
    TRANSPORTS
        .iter()
        .find(|(name, _)| *name == transport)
        .map_or_else(|| transport.to_owned(), |(_, label)| (*label).to_owned())
}

/// The setting with `camera` added, or put in place of the camera at `replacing`, or
/// why the page won't ask the app to. The app checks the address itself; this catches
/// what is plainly missing, and adds VISCA over IP's port to an address typed without
/// one.
pub fn with_camera(
    setting: &PtzCameraSetting,
    replacing: Option<usize>,
    name: &str,
    address: &str,
    transport: &str,
    visca: u8,
) -> Result<PtzCameraSetting, &'static str> {
    let name = name.trim();
    let address = address.trim();
    if name.is_empty() {
        return Err("Type a name for the camera, such as Wide or Stage left.");
    }
    if address.is_empty() {
        return Err("Type the camera's address, such as 192.168.1.40:52381.");
    }
    if setting
        .cameras
        .iter()
        .enumerate()
        .any(|(i, c)| Some(i) != replacing && c.name == name)
    {
        return Err("A camera with that name is listed already.");
    }
    // An IPv4 address without a port; an IPv6 one goes in brackets with its port.
    let address = if !address.contains(':') && transport == "visca_ip" {
        format!("{address}:{VISCA_IP_PORT}")
    } else {
        address.to_owned()
    };
    let camera = PtzCameraEntry {
        name: name.to_owned(),
        address,
        transport: transport.to_owned(),
        visca,
    };
    let mut next = setting.clone();
    match replacing {
        Some(i) if i < next.cameras.len() => next.cameras[i] = camera,
        _ => next.cameras.push(camera),
    }
    Ok(next)
}

/// The switch and the camera list. Shows nothing outside the desktop app.
#[component]
pub fn PtzCameraSettingView() -> impl IntoView {
    let Some(started) = current() else {
        return ().into_any();
    };
    let setting = RwSignal::new(started);
    let problem = RwSignal::new(None::<String>);
    let editing = RwSignal::new(None::<usize>);
    let name = RwSignal::new(String::new());
    let address = RwSignal::new(String::new());
    let transport = RwSignal::new(TRANSPORTS[0].0.to_owned());
    let visca = RwSignal::new(1_u8);
    let clear_form = move || {
        editing.try_set(None);
        name.try_set(String::new());
        address.try_set(String::new());
        transport.try_set(TRANSPORTS[0].0.to_owned());
        visca.try_set(1);
    };
    let save = move |next: PtzCameraSetting, from_form: bool| {
        let before = setting.get_untracked();
        setting.set(next.clone());
        spawn_local(async move {
            match crate::host::set_ptz_cameras(&next).await {
                None => {
                    SAVED.with(|saved| *saved.borrow_mut() = Some(next));
                    problem.try_set(None);
                    if from_form {
                        clear_form();
                    }
                }
                // Saved, but the engine couldn't open a camera's connection: the list
                // stands, and the page says why it isn't running.
                Some(why) if why.starts_with("Saved,") => {
                    SAVED.with(|saved| *saved.borrow_mut() = Some(next));
                    problem.try_set(Some(why));
                    if from_form {
                        clear_form();
                    }
                }
                Some(why) => {
                    setting.try_set(before);
                    problem.try_set(Some(why));
                }
            }
        });
    };
    let toggle = move |on: bool| {
        let mut next = setting.get_untracked();
        next.on = on;
        save(next, false);
    };
    let remove = move |index: usize| {
        let mut next = setting.get_untracked();
        if index < next.cameras.len() {
            next.cameras.remove(index);
        }
        if editing.get_untracked() == Some(index) {
            clear_form();
        }
        save(next, false);
    };
    let edit = move |index: usize| {
        let Some(camera) = setting.with_untracked(|s| s.cameras.get(index).cloned()) else {
            return;
        };
        editing.set(Some(index));
        name.set(camera.name);
        address.set(camera.address);
        transport.set(camera.transport);
        visca.set(camera.visca);
        problem.set(None);
    };
    let submit = move || match with_camera(
        &setting.get_untracked(),
        editing.get_untracked(),
        &name.get_untracked(),
        &address.get_untracked(),
        &transport.get_untracked(),
        visca.get_untracked(),
    ) {
        Ok(next) => save(next, true),
        Err(why) => problem.set(Some(why.to_owned())),
    };
    view! {
        <section class="ptz-camera-setting" id="ptz-camera-setting">
            <label class="settings-check">
                <input
                    type="checkbox"
                    aria-label="Control PTZ cameras"
                    prop:checked=move || setting.with(|now| now.on)
                    on:change=move |event| toggle(event_target_checked(&event))
                />
                <span>
                    <span class="settings-check-label">"Control PTZ cameras"</span>
                    <span class="settings-check-note">
                        "When on, JStream pans, tilts, zooms and recalls presets on the camera heads listed below, over VISCA, from the PTZ panel, scene shots, hotkeys and mappings. It sends only to the addresses you add here, on your local network. Changes apply at once. Off until you turn it on."
                    </span>
                </span>
            </label>
            <h4 class="ptz-controller-title">"Cameras"</h4>
            {move || {
                let list = setting.with(|now| now.cameras.clone());
                if list.is_empty() {
                    return view! { <p class="placeholder">"No PTZ camera is listed."</p> }.into_any();
                }
                view! {
                    <ul class="device-list">
                        {list.into_iter().enumerate().map(|(index, c)| {
                            let kind = transport_label(&c.transport);
                            view! {
                                <li class="device" class:editing=move || editing.get() == Some(index)>
                                    <span class="device-name">{c.name.clone()}</span>
                                    <span class="device-dates">
                                        {format!("{}, {kind}, VISCA address {}", c.address, c.visca)}
                                    </span>
                                    <button type="button" on:click=move |_| edit(index)>"Edit"</button>
                                    <button type="button" on:click=move |_| remove(index)>"Remove"</button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }
                .into_any()
            }}
            <form class="ptz-controller-add ptz-camera-add" on:submit=move |event| { event.prevent_default(); submit(); }>
                <input
                    type="text"
                    aria-label="Camera name"
                    placeholder="Name"
                    maxlength="64"
                    prop:value=move || name.get()
                    on:input=move |event| name.set(event_target_value(&event))
                />
                <input
                    type="text"
                    aria-label="Camera address and port"
                    placeholder="192.168.1.40:52381"
                    prop:value=move || address.get()
                    on:input=move |event| address.set(event_target_value(&event))
                />
                <select
                    aria-label="Transport"
                    prop:value=move || transport.get()
                    on:change=move |event| transport.set(event_target_value(&event))
                >
                    {TRANSPORTS.iter().map(|(name, label)| view! {
                        <option value=*name>{*label}</option>
                    }).collect_view()}
                </select>
                <select
                    aria-label="VISCA address"
                    prop:value=move || visca.get().to_string()
                    on:change=move |event| {
                        if let Ok(n) = event_target_value(&event).parse::<u8>() {
                            visca.set(n);
                        }
                    }
                >
                    {(1_u8..=7).map(|n| view! {
                        <option value=n.to_string()>{format!("VISCA {n}")}</option>
                    }).collect_view()}
                </select>
                <button type="submit">{move || if editing.get().is_some() { "Save camera" } else { "Add camera" }}</button>
                {move || editing.get().is_some().then(|| view! {
                    <button type="button" on:click=move |_| clear_form()>"Cancel"</button>
                })}
            </form>
            {move || problem.get().map(|why| view! { <p class="problem" role="alert">{why}</p> })}
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{transport_label, with_camera};
    use crate::host::PtzCameraSetting;

    #[test]
    fn a_camera_needs_a_name_of_its_own_and_an_address() {
        let none = PtzCameraSetting::default();
        let one = with_camera(&none, None, " Wide ", " 192.168.1.40:52381 ", "visca_ip", 1)
            .expect("added");
        assert_eq!(one.cameras[0].name, "Wide");
        assert_eq!(one.cameras[0].address, "192.168.1.40:52381");
        assert!(with_camera(&none, None, "", "192.168.1.40", "udp", 1).is_err());
        assert!(with_camera(&none, None, "Wide", " ", "udp", 1).is_err());
        assert!(
            with_camera(&one, None, "Wide", "192.168.1.41:52381", "udp", 1).is_err(),
            "two cameras with one name"
        );
        // Editing a camera may keep its own name.
        let edited =
            with_camera(&one, Some(0), "Wide", "192.168.1.41:5678", "tcp", 2).expect("edited");
        assert_eq!(edited.cameras.len(), 1);
        assert_eq!(edited.cameras[0].transport, "tcp");
    }

    #[test]
    fn visca_over_ip_gets_its_port() {
        let none = PtzCameraSetting::default();
        let one = with_camera(&none, None, "Wide", "192.168.1.40", "visca_ip", 1).expect("added");
        assert_eq!(one.cameras[0].address, "192.168.1.40:52381");
        let raw = with_camera(&none, None, "Wide", "192.168.1.40", "tcp", 1).expect("added");
        assert_eq!(
            raw.cameras[0].address, "192.168.1.40",
            "the app refuses it, saying why"
        );
        assert_eq!(transport_label("udp"), "Raw VISCA over UDP");
        assert_eq!(transport_label("serial"), "serial");
    }
}
