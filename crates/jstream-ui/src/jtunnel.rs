//! "Receive from JTunnel senders" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! decisions 1 and 3), on the Settings dialog's Devices page (`settings.rs`), in the
//! desktop app only.
//!
//! - **The switch** turns the JTunnel receiver on for this machine. Off until the person
//!   turns it on.
//! - **The accepted senders** are listed by tunnel name and the address and port they
//!   send from, each with Remove. The add row takes a name and an `address:port`; the
//!   app checks the sender as the engine would (a private IPv4 address, a name no other
//!   sender has) and the page shows why it refused one.
//! - **Saving.** Each change asks the app to save the whole setting
//!   (`host::set_jtunnel`, the app's own command, which no engine client reaches). A
//!   refused change is undone on the page. It applies when the app next starts, and the
//!   page says so while the saved setting differs from the one the app started with.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::host::JtunnelSetting;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The setting with one more sender, or why the page won't ask the app to add it. The
/// app checks the address itself; this only catches what is plainly missing.
pub fn with_sender(
    setting: &JtunnelSetting,
    name: &str,
    address: &str,
) -> Result<JtunnelSetting, &'static str> {
    let name = name.trim();
    let address = address.trim();
    if name.is_empty() {
        return Err("Type the sender's tunnel name.");
    }
    if address.is_empty() {
        return Err("Type the address and port it sends from, such as 192.168.1.5:9000.");
    }
    if setting.accepted.iter().any(|(known, _)| known == name) {
        return Err("A sender with that name is already accepted.");
    }
    let mut next = setting.clone();
    next.accepted.push((name.to_owned(), address.to_owned()));
    Ok(next)
}

/// The switch and the accepted senders. Shows nothing outside the desktop app.
#[component]
pub fn JtunnelSettingView() -> impl IntoView {
    let Some(started) = crate::host::desktop_jtunnel() else {
        return ().into_any();
    };
    let setting = RwSignal::new(started.clone());
    let problem = RwSignal::new(None::<String>);
    let name = RwSignal::new(String::new());
    let address = RwSignal::new(String::new());
    let save = move |next: JtunnelSetting, added: bool| {
        let before = setting.get_untracked();
        setting.set(next.clone());
        spawn_local(async move {
            match crate::host::set_jtunnel(&next).await {
                None => {
                    problem.try_set(None);
                    if added {
                        name.try_set(String::new());
                        address.try_set(String::new());
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
    let remove = move |gone: String| {
        let mut next = setting.get_untracked();
        next.accepted.retain(|(known, _)| *known != gone);
        save(next, false);
    };
    let add = move || match with_sender(
        &setting.get_untracked(),
        &name.get_untracked(),
        &address.get_untracked(),
    ) {
        Ok(next) => save(next, true),
        Err(why) => {
            problem.set(Some(why.to_owned()));
        }
    };
    let changed = {
        let started = started.clone();
        move || setting.with(|now| *now != started)
    };
    view! {
        <section class="jtunnel-setting">
            <label class="settings-check">
                <input
                    type="checkbox"
                    aria-label="Receive from JTunnel senders"
                    prop:checked=move || setting.with(|now| now.on)
                    on:change=move |event| toggle(event_target_checked(&event))
                />
                <span>
                    <span class="settings-check-label">"Receive from JTunnel senders"</span>
                    <span class="settings-check-note">
                        "When on, JStream takes audio and MIDI from the JTunnel senders below, over UDP on this network. Only the senders you accept here get in, each from the address and port it sends from. Off until you turn it on."
                    </span>
                </span>
            </label>
            {move || {
                let list = setting.with(|now| now.accepted.clone());
                if list.is_empty() {
                    return view! { <p class="placeholder">"No sender is accepted."</p> }.into_any();
                }
                view! {
                    <ul class="device-list">
                        {list.into_iter().map(|(sender, from)| {
                            let gone = sender.clone();
                            view! {
                                <li class="device">
                                    <span class="device-name">{sender}</span>
                                    <span class="device-dates">{from}</span>
                                    <button type="button" on:click=move |_| remove(gone.clone())>"Remove"</button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }
                .into_any()
            }}
            <form class="jtunnel-add" on:submit=move |event| { event.prevent_default(); add(); }>
                <input
                    type="text"
                    aria-label="Sender name"
                    placeholder="Name"
                    prop:value=move || name.get()
                    on:input=move |event| name.set(event_target_value(&event))
                />
                <input
                    type="text"
                    aria-label="Sender address and port"
                    placeholder="192.168.1.5:9000"
                    prop:value=move || address.get()
                    on:input=move |event| address.set(event_target_value(&event))
                />
                <button type="submit">"Add sender"</button>
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
    use super::with_sender;
    use crate::host::JtunnelSetting;

    #[test]
    fn the_setting_reads_and_writes_the_apps_json() {
        let text =
            r#"{"on":true,"accepted":[{"name":"Bitwig","address":"192.168.1.5:9000"},{"name":7}]}"#;
        let setting = JtunnelSetting::parse(text);
        assert!(setting.on);
        assert_eq!(
            setting.accepted,
            vec![("Bitwig".to_owned(), "192.168.1.5:9000".to_owned())],
            "an entry it can't read is left out"
        );
        assert_eq!(JtunnelSetting::parse(&setting.to_json()), setting);
        assert_eq!(JtunnelSetting::parse("not json"), JtunnelSetting::default());
        assert!(!JtunnelSetting::parse(r#"{"on":"yes"}"#).on);
    }

    #[test]
    fn a_sender_needs_a_name_an_address_and_a_name_of_its_own() {
        let none = JtunnelSetting::default();
        let one = with_sender(&none, " Bitwig ", " 192.168.1.5:9000 ").expect("added");
        assert_eq!(
            one.accepted,
            vec![("Bitwig".to_owned(), "192.168.1.5:9000".to_owned())]
        );
        assert!(with_sender(&none, "  ", "192.168.1.5:9000").is_err());
        assert!(with_sender(&none, "Bitwig", "").is_err());
        assert!(with_sender(&one, "Bitwig", "192.168.1.6:9000").is_err());
    }
}
