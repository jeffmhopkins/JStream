//! Command-line options.

pub const USAGE: &str = "\
s2-vcam: spike S2 producer and registration tool for the JStream S2 virtual camera

USAGE:
    s2-vcam --register hkcu|hklm [--no-acl]   Register s2_vcam.dll (next to this exe) for COM
    s2-vcam --unregister                      Remove the registration from HKCU and HKLM
    s2-vcam --status [--json]                 Show what is registered where
    s2-vcam [--seconds N] [--fps N] [--json]  Create the camera and feed it frames until
                                              Enter is pressed (or N seconds pass)

OPTIONS:
    --seconds <N>   Stop after N seconds instead of waiting for Enter
    --fps <N>       Producer frame rate, 1 to 60 [default: 30]
    --json          Print the report as one JSON object on stdout
    --no-vcam       Feed the shared textures but don't call MFCreateVirtualCamera
    --no-acl        With --register: don't grant LOCAL SERVICE read/execute on the DLL
    -h, --help      Print this help

`hklm` needs an elevated (Run as administrator) prompt.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Machine,
}

#[derive(Debug)]
pub enum Command {
    Register {
        scope: Scope,
        acl: bool,
    },
    Unregister,
    Status,
    Run {
        seconds: Option<f64>,
        fps: u32,
        vcam: bool,
    },
}

#[derive(Debug)]
pub struct Args {
    pub command: Command,
    pub json: bool,
}

impl Args {
    /// Parses the arguments. `Ok(None)` means help was requested.
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut register = None;
        let mut unregister = false;
        let mut status = false;
        let mut seconds = None;
        let mut fps = 30;
        let mut vcam = true;
        let mut acl = true;
        let mut json = false;
        while let Some(flag) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match flag.as_str() {
                "-h" | "--help" => return Ok(None),
                "--json" => json = true,
                "--no-vcam" => vcam = false,
                "--no-acl" => acl = false,
                "--unregister" => unregister = true,
                "--status" => status = true,
                "--register" => {
                    register = Some(match value("--register")?.to_ascii_lowercase().as_str() {
                        "hkcu" => Scope::User,
                        "hklm" => Scope::Machine,
                        other => {
                            return Err(format!("--register takes hkcu or hklm, not `{other}`"));
                        }
                    })
                }
                "--seconds" => {
                    let s: f64 = parse_num(&value("--seconds")?)?;
                    if !s.is_finite() || s <= 0.0 || s > 86_400.0 {
                        return Err("--seconds must be > 0 and at most 86400 (one day)".into());
                    }
                    seconds = Some(s);
                }
                "--fps" => fps = parse_num::<u32>(&value("--fps")?)?.clamp(1, 60),
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        let modes = usize::from(register.is_some()) + usize::from(unregister) + usize::from(status);
        if modes > 1 {
            return Err("use only one of --register, --unregister and --status".into());
        }
        let command = match (register, unregister, status) {
            (Some(scope), _, _) => Command::Register { scope, acl },
            (_, true, _) => Command::Unregister,
            (_, _, true) => Command::Status,
            _ => Command::Run { seconds, fps, vcam },
        };
        Ok(Some(Args { command, json }))
    }
}

fn parse_num<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("`{s}` is not a valid number"))
}
