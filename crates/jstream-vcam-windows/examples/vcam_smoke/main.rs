//! Hardware smoke test for the media source: a stand-in for the engine side, which is G3.
//!
//! It publishes a ring of shared textures, a shared fence and the control block exactly as
//! the `jstream-vcam-proto` contract describes (in this session's namespace, with the
//! contract's DACL for this user), creates the virtual camera with
//! `MFCreateVirtualCamera`, renders a test picture for a while, then prints what the DLL
//! recorded in the control block.
//!
//! The picture is dark teal with a white bar moving right. The DLL's own fallback is colour
//! bars, so a viewer can tell which one Frame Server shows.
//!
//! ```text
//! cargo run --release -p jstream-vcam-windows --example vcam_smoke -- [--seconds N] [--no-camera] [--no-producer]
//! ```
//!
//! The DLL must be registered first (`regsvr32`, elevated); see the crate README.

#[cfg(windows)]
mod smoke;

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    return smoke::main();
    #[cfg(not(windows))]
    {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "vcam_smoke runs only on Windows");
        std::process::ExitCode::FAILURE
    }
}
