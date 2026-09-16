#![forbid(unsafe_code)]

use std::{ffi::OsString, io, process::ExitCode};

fn main() -> ExitCode {
    // One extra argument lets the parser report overflow without collecting an
    // arbitrary vector. OS-owned argv allocation is outside managed source RAM.
    let args: Vec<OsString> = std::env::args_os().skip(1)
        .take(fcb_app::args::MAX_ARGUMENTS + 1).collect();
    let input = io::stdin();
    let output = io::stdout();
    let diagnostic = io::stderr();
    let exit = fcb_app::run(&args, &mut input.lock(), &mut output.lock(),
        &mut diagnostic.lock(), || false);
    ExitCode::from(exit)
}
