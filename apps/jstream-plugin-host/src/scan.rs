//! `scan`: what a plugin file offers. Each audio effect in it is created, activated once
//! to check it is stereo with ports the host takes, and described with its parameters.
//! Instruments, note effects and effects that won't activate are left out.

use jstream_plugin_clap::{Library, Plugin};
use jstream_plugin_proto::control::PluginDesc;

use crate::session::param_desc;

/// The rate and block size the check activates each effect at.
const CHECK_RATE: u32 = 48_000;
const CHECK_FRAMES: u32 = 1024;

/// The file's hostable audio effects, in its order.
pub(crate) fn scan(library: &std::sync::Arc<Library>) -> Vec<PluginDesc> {
    library
        .plugins()
        .into_iter()
        .filter(jstream_plugin_clap::PluginInfo::is_audio_effect)
        .filter_map(|info| {
            let mut plugin = Plugin::new(library, &info.id).ok()?;
            let processor = plugin.activate(CHECK_RATE, CHECK_FRAMES).ok()?;
            drop(processor);
            plugin.deactivate().ok()?;
            Some(PluginDesc {
                params: plugin.params().iter().map(param_desc).collect(),
                id: info.id,
                name: info.name,
                vendor: info.vendor.unwrap_or_default(),
                version: info.version.unwrap_or_default(),
                description: info.description.unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_plugin_clap::test_plugin;

    #[test]
    fn lists_only_the_hostable_effects() {
        let library = test_plugin::library().unwrap();
        let found = scan(&library);
        let ids: Vec<&str> = found.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, [test_plugin::GAIN_ID, test_plugin::SIDECHAIN_ID]);
        let gain = &found[0];
        assert_eq!(gain.name, "Test gain");
        assert_eq!(gain.vendor, "JStream tests");
        assert_eq!(gain.params.len(), 1);
        assert_eq!(gain.params[0].id, test_plugin::GAIN_PARAM);
    }
}
