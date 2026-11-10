//! "Accept a PTZ controller" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! §5, [ADR-0153](../../../docs/adr/0153-ptz-controller-idle-stop-counters-and-setting.md)
//! §3), on the Settings dialog's Devices page (`settings.rs`), in the desktop app only.
//!
//! - **The switch** opens the VISCA controller endpoints on this machine. Off until the
//!   person turns it on.
//! - **The endpoints** are listed by the address and port they listen on, their
//!   transport, the VISCA address they answer as, and what they drive, each with
//!   Remove. The add row takes an address and port (one of this machine's LAN
//!   addresses), a transport, a VISCA address, and a target: Program, Preview, or one
//!   scene of the collection open now.
//! - **The allowed controllers** are listed by IP address, each with Remove, and the add
//!   row takes one. An endpoint answers only these.
//! - **Saving.** Each change asks the app to save the whole setting (`host::set_ptz`, the
//!   app's own command, which no engine client reaches); the app checks it as the engine
//!   would and the page shows why it refused one, undoing the change. It applies when
//!   the app next starts, and the page says so while the saved setting differs from the
//!   one the app started with.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::host::{PtzEndpoint, PtzSetting};
use crate::store::Studio;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The transports the add row offers: their wire names and labels.
const TRANSPORTS: [(&str, &str); 3] = [
    ("visca_ip", "VISCA over IP (UDP)"),
    ("udp", "Raw VISCA over UDP"),
    ("tcp", "Raw VISCA over TCP"),
];

/// The setting with one more endpoint, or why the page won't ask the app to add it. The
/// app checks the address itself; this only catches what is plainly missing.
pub fn with_endpoint(
    setting: &PtzSetting,
    address: &str,
    transport: &str,
    visca: u8,
    target: &str,
) -> Result<PtzSetting, &'static str> {
    let address = address.trim();
    if address.is_empty() {
        return Err("Type the address and port to listen on, such as 192.168.1.20:52381.");
    }
    if target.is_empty() {
        return Err("Pick what the controller drives.");
    }
    if setting
        .endpoints
        .iter()
        .any(|e| e.address == address && (e.transport == "tcp") == (transport == "tcp"))
    {
        return Err("An endpoint already listens on that address and port.");
    }
    let mut next = setting.clone();
    next.endpoints.push(PtzEndpoint {
        address: address.to_owned(),
        transport: transport.to_owned(),
        visca,
        target: target.to_owned(),
    });
    Ok(next)
}

/// The setting with one more allowed controller address, or why not.
pub fn with_allowed(setting: &PtzSetting, ip: &str) -> Result<PtzSetting, &'static str> {
    let ip = ip.trim();
    if ip.is_empty() {
        return Err("Type the controller's IP address, such as 192.168.1.50.");
    }
    if setting.allow.iter().any(|known| known == ip) {
        return Err("That controller is already allowed.");
    }
    let mut next = setting.clone();
    next.allow.push(ip.to_owned());
    Ok(next)
}

/// How a target reads: Program, Preview, or the scene's name (its ID when the open
/// collection has no such scene).
pub fn target_label(target: &str, scene_name: impl Fn(&str) -> Option<String>) -> String {
    match target {
        "program" => "Program".to_owned(),
        "preview" => "Preview".to_owned(),
        other => match other.strip_prefix("scene:") {
            Some(id) => scene_name(id).unwrap_or_else(|| id.to_owned()),
            None => other.to_owned(),
        },
    }
}

/// The switch, the endpoints and the allowed controllers. Shows nothing outside the
/// desktop app.
#[component]
pub fn PtzControllerSettingView() -> impl IntoView {
    let Some(started) = crate::host::desktop_ptz() else {
        return ().into_any();
    };
    let studio = use_context::<Studio>();
    let setting = RwSignal::new(started.clone());
    let problem = RwSignal::new(None::<String>);
    let address = RwSignal::new(String::new());
    let transport = RwSignal::new(TRANSPORTS[0].0.to_owned());
    let visca = RwSignal::new(1_u8);
    let target = RwSignal::new("program".to_owned());
    let ip = RwSignal::new(String::new());
    let save = move |next: PtzSetting, clear: Option<RwSignal<String>>| {
        let before = setting.get_untracked();
        setting.set(next.clone());
        spawn_local(async move {
            match crate::host::set_ptz(&next).await {
                None => {
                    problem.try_set(None);
                    if let Some(field) = clear {
                        field.try_set(String::new());
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
        save(next, None);
    };
    let remove_endpoint = move |index: usize| {
        let mut next = setting.get_untracked();
        if index < next.endpoints.len() {
            next.endpoints.remove(index);
        }
        save(next, None);
    };
    let remove_allowed = move |gone: String| {
        let mut next = setting.get_untracked();
        next.allow.retain(|known| *known != gone);
        save(next, None);
    };
    let add_endpoint = move || match with_endpoint(
        &setting.get_untracked(),
        &address.get_untracked(),
        &transport.get_untracked(),
        visca.get_untracked(),
        &target.get_untracked(),
    ) {
        Ok(next) => save(next, Some(address)),
        Err(why) => problem.set(Some(why.to_owned())),
    };
    let add_allowed = move || match with_allowed(&setting.get_untracked(), &ip.get_untracked()) {
        Ok(next) => save(next, Some(ip)),
        Err(why) => problem.set(Some(why.to_owned())),
    };
    let changed = {
        let started = started.clone();
        move || setting.with(|now| *now != started)
    };
    let scene_name = move |id: &str| {
        studio.and_then(|studio| {
            studio.collection.with(|c| {
                c.as_ref()?
                    .scenes
                    .iter()
                    .find(|s| s.scene_id.as_str() == id)
                    .map(|s| s.name.clone())
            })
        })
    };
    // The scenes the target picker offers: the open collection's, groups left out.
    let scenes = move || -> Vec<(String, String)> {
        studio.map_or_else(Vec::new, |studio| {
            studio.collection.with(|c| {
                c.as_ref().map_or_else(Vec::new, |c| {
                    c.scenes
                        .iter()
                        .filter(|s| s.role.is_scene())
                        .map(|s| (format!("scene:{}", s.scene_id), s.name.clone()))
                        .collect()
                })
            })
        })
    };
    view! {
        <section class="ptz-controller-setting">
            <label class="settings-check">
                <input
                    type="checkbox"
                    aria-label="Accept a PTZ controller"
                    prop:checked=move || setting.with(|now| now.on)
                    on:change=move |event| toggle(event_target_checked(&event))
                />
                <span>
                    <span class="settings-check-label">"Accept a PTZ controller"</span>
                    <span class="settings-check-note">
                        "When on, a PTZ joystick or control app can pan, tilt and zoom scene cameras over VISCA, as it drives a camera head. Each endpoint below listens on one of this machine's network addresses and answers only the controllers you allow. Nothing it does is saved or undone, and a drive stops if the controller goes quiet. Off until you turn it on."
                    </span>
                </span>
            </label>
            <h4 class="ptz-controller-title">"Endpoints"</h4>
            {move || {
                let list = setting.with(|now| now.endpoints.clone());
                if list.is_empty() {
                    return view! { <p class="placeholder">"No endpoint is set up."</p> }.into_any();
                }
                view! {
                    <ul class="device-list">
                        {list.into_iter().enumerate().map(|(index, e)| {
                            let kind = TRANSPORTS
                                .iter()
                                .find(|(name, _)| *name == e.transport)
                                .map_or(e.transport.clone(), |(_, label)| (*label).to_owned());
                            let drives = target_label(&e.target, scene_name);
                            view! {
                                <li class="device">
                                    <span class="device-name">{e.address.clone()}</span>
                                    <span class="device-dates">
                                        {format!("{kind}, VISCA address {}, drives {drives}", e.visca)}
                                    </span>
                                    <button type="button" on:click=move |_| remove_endpoint(index)>"Remove"</button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }
                .into_any()
            }}
            <form class="ptz-controller-add" on:submit=move |event| { event.prevent_default(); add_endpoint(); }>
                <input
                    type="text"
                    aria-label="Endpoint address and port"
                    placeholder="192.168.1.20:52381"
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
                <select
                    aria-label="Drives"
                    prop:value=move || target.get()
                    on:change=move |event| target.set(event_target_value(&event))
                >
                    <option value="program">"Program"</option>
                    <option value="preview">"Preview"</option>
                    {move || scenes().into_iter().map(|(value, name)| view! {
                        <option value=value>{name}</option>
                    }).collect_view()}
                </select>
                <button type="submit">"Add endpoint"</button>
            </form>
            <h4 class="ptz-controller-title">"Allowed controllers"</h4>
            {move || {
                let list = setting.with(|now| now.allow.clone());
                if list.is_empty() {
                    return view! { <p class="placeholder">"No controller is allowed."</p> }.into_any();
                }
                view! {
                    <ul class="device-list">
                        {list.into_iter().map(|known| {
                            let gone = known.clone();
                            view! {
                                <li class="device">
                                    <span class="device-name">{known}</span>
                                    <button type="button" on:click=move |_| remove_allowed(gone.clone())>"Remove"</button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }
                .into_any()
            }}
            <form class="ptz-controller-add" on:submit=move |event| { event.prevent_default(); add_allowed(); }>
                <input
                    type="text"
                    aria-label="Controller IP address"
                    placeholder="192.168.1.50"
                    prop:value=move || ip.get()
                    on:input=move |event| ip.set(event_target_value(&event))
                />
                <button type="submit">"Allow controller"</button>
            </form>
            {move || changed().then(|| view! {
                <p class="settings-check-note" role="status">"Restart JStream to apply this."</p>
            })}
            {move || problem.get().map(|why| view! { <p class="problem" role="alert">{why}</p> })}
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{target_label, with_allowed, with_endpoint};
    use crate::host::PtzSetting;

    #[test]
    fn the_setting_reads_and_writes_the_apps_json() {
        let text = r#"{"on":true,"endpoints":[{"address":"192.168.1.20:52381","transport":"visca_ip","visca":1,"target":"program"},{"address":7}],"allow":["192.168.1.50",3]}"#;
        let setting = PtzSetting::parse(text);
        assert!(setting.on);
        assert_eq!(
            setting.endpoints.len(),
            1,
            "an entry it can't read is left out"
        );
        assert_eq!(setting.endpoints[0].target, "program");
        assert_eq!(setting.allow, vec!["192.168.1.50".to_owned()]);
        assert_eq!(PtzSetting::parse(&setting.to_json()), setting);
        assert_eq!(PtzSetting::parse("not json"), PtzSetting::default());
        assert!(!PtzSetting::parse(r#"{"on":"yes"}"#).on);
    }

    #[test]
    fn an_endpoint_needs_an_address_a_target_and_a_socket_of_its_own() {
        let none = PtzSetting::default();
        let one =
            with_endpoint(&none, " 192.168.1.20:52381 ", "visca_ip", 1, "program").expect("added");
        assert_eq!(one.endpoints[0].address, "192.168.1.20:52381");
        assert!(with_endpoint(&none, " ", "udp", 1, "program").is_err());
        assert!(with_endpoint(&none, "192.168.1.20:52381", "udp", 1, "").is_err());
        assert!(
            with_endpoint(&one, "192.168.1.20:52381", "udp", 2, "preview").is_err(),
            "two UDP sockets on one port"
        );
        assert!(
            with_endpoint(&one, "192.168.1.20:52381", "tcp", 2, "preview").is_ok(),
            "TCP and UDP can share a port"
        );
    }

    #[test]
    fn a_controller_is_allowed_once() {
        let one = with_allowed(&PtzSetting::default(), " 192.168.1.50 ").expect("added");
        assert_eq!(one.allow, vec!["192.168.1.50".to_owned()]);
        assert!(with_allowed(&one, "192.168.1.50").is_err());
        assert!(with_allowed(&one, "").is_err());
    }

    #[test]
    fn a_target_reads_as_its_scenes_name() {
        let names = |id: &str| (id == "scn_a").then(|| "Interview".to_owned());
        assert_eq!(target_label("program", names), "Program");
        assert_eq!(target_label("preview", names), "Preview");
        assert_eq!(target_label("scene:scn_a", names), "Interview");
        assert_eq!(target_label("scene:scn_gone", names), "scn_gone");
    }
}
