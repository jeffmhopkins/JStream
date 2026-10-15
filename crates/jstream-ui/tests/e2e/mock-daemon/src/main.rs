//! `jstream-ui-e2e`: the mock daemon the web UI's Playwright tests run against. It puts
//! `jstream-control`'s `MockEngine` behind `jstream-server` on `127.0.0.1`, serves a
//! trunk build of the UI from the same port, seeds the engine with a small studio, and
//! prints the page's URL on its first line of output. It stops when its standard input
//! closes, so a test runner that dies doesn't leave it running. Test-only: never shipped.
//!
//! ```text
//! jstream-ui-e2e --ui-dir crates/jstream-ui/dist --token <token>
//! ```
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-ui/tests/e2e/README.md#harness

#![forbid(unsafe_code)]

use anyhow::{Context as _, Result, bail};
use jstream_control::protocol::{
    Action, Batch, Call, Edit, EncoderProfile, EncoderProfileId, Hello, ItemGeometry,
    PROTOCOL_VERSION, Settings, Size, SourceId, SourceKind, Vec2,
};
use jstream_control::{Control, MockEngine};
use jstream_server::{Connector, Server, ServerConfig, Token};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

/// What the command line asks for.
struct Args {
    /// The trunk build to serve (`dist/`).
    ui_dir: PathBuf,
    /// The control token; the browser presents it as its `Basic` password.
    token: String,
}

fn parse_args() -> Result<Args> {
    let mut ui_dir = None;
    let mut token = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("`{arg}` needs a value"))
        };
        match arg.as_str() {
            "--ui-dir" => ui_dir = Some(PathBuf::from(value()?)),
            "--token" => token = Some(value()?),
            other => bail!("unknown argument `{other}`"),
        }
    }
    Ok(Args {
        ui_dir: ui_dir.context("`--ui-dir <dist>` is required")?,
        token: token.context("`--token <token>` is required")?,
    })
}

/// The studio every test starts from: two scenes with `Main` on program, holding one
/// colour item as a 640×360 box at the canvas's centre, and one encoder profile. No
/// outputs: the tests create them through the UI.
async fn seed(engine: &MockEngine) -> Result<()> {
    let control = engine.connect();
    control
        .hello(Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "jstream-ui-e2e".into(),
        })
        .await?;
    let center = Vec2::new(960.0, 540.0).context("finite centre")?;
    let size = Size::try_new(640.0, 360.0).context("positive size")?;
    let geometry = ItemGeometry::try_new(center, size).context("valid geometry")?;
    let edits = vec![
        Edit::SceneCreate {
            scene_id: "intro".into(),
            name: "Intro".into(),
            role: None,
        },
        Edit::SceneCreate {
            scene_id: "main".into(),
            name: "Main".into(),
            role: None,
        },
        Edit::SourceCreate {
            source_id: SourceId::new("color"),
            name: "Backdrop".into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: Settings::default(),
            duplicated_from: None,
        },
        Edit::ItemAdd {
            scene_id: "main".into(),
            item_id: "box".into(),
            source_id: SourceId::new("color"),
            transform: None,
            geometry: Some(geometry),
        },
        Edit::EncoderCreate {
            profile_id: EncoderProfileId::new("hq"),
            name: "High quality".into(),
            profile: EncoderProfile::DEFAULT,
        },
    ];
    control
        .call(Call::Batch(Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
        }))
        .await?;
    control
        .call(Call::Action {
            action: Action::SceneSwitch {
                scene_id: "main".into(),
            },
        })
        .await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let engine = MockEngine::new();
    seed(&engine).await.context("seeding the mock engine")?;

    let mut config = ServerConfig::new(Token::new(args.token)?);
    config.ui_dir = Some(args.ui_dir);
    let sessions = engine.clone();
    let connector: Connector = Arc::new(move || Box::new(sessions.connect()));
    let server = Server::bind(config, connector).await?;
    let url = server
        .ui_url()
        .context("the server serves no UI despite `ui_dir`")?;

    // The test runner reads this line to learn the port.
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{url}")?;
    stdout.flush()?;
    drop(stdout);

    let (closed, stdin_closed) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        // Read until end of file or an error: either way the runner is gone.
        let mut sink = [0_u8; 256];
        let mut stdin = std::io::stdin();
        while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
        let _ = closed.send(());
    });
    server
        .serve_with_shutdown(async {
            let _ = stdin_closed.await;
        })
        .await?;
    Ok(())
}
