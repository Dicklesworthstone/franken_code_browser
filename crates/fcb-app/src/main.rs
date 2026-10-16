#![forbid(unsafe_code)]

use std::{ffi::OsString, io, process::ExitCode};

#[cfg(any(target_os = "macos", test))]
mod native_launch;

fn main() -> ExitCode {
    // One extra argument lets the parser report overflow without collecting an
    // arbitrary vector. OS-owned argv allocation is outside managed source RAM.
    let args: Vec<OsString> = std::env::args_os().skip(1)
        .take(fcb_app::args::MAX_ARGUMENTS + 1).collect();
    let input = io::stdin();
    let output = io::stdout();
    let diagnostic = io::stderr();
    // Platform process launch belongs to this executable, not reusable headless
    // service construction. Machine requests always retain the existing route.
    #[cfg(target_os = "macos")]
    if let Some(exit) = native_launch::run(&args, &mut output.lock(), &mut diagnostic.lock(), || false) {
        return ExitCode::from(exit);
    }
    let exit = fcb_app::run(&args, &mut input.lock(), &mut output.lock(),
        &mut diagnostic.lock(), || false);
    ExitCode::from(exit)
}
