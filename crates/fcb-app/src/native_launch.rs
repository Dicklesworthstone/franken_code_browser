#![forbid(unsafe_code)]

//! Binary-only native application handoff. The reusable `fcb_app::run` service
//! remains headless; only an explicit GUI invocation may inspect installations
//! or start Launch Services. No shell, repository executable, PATH lookup,
//! default application, or implicit stdin is involved.

use std::{ffi::{OsStr, OsString}, path::{Path, PathBuf}};
use fcb_app::args::{self, Command, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_SINGLE_ARGUMENT};

const PROJECT_MARKER: &str = "Contents/Resources/fcb-launch-project-v1";
const FILE_MARKER: &str = "Contents/Resources/fcb-launch-file-v1";
const LINE_MARKER: &str = "Contents/Resources/fcb-launch-line-v1";
const PICKER_MARKER: &str = "Contents/Resources/fcb-launch-picker-v1";
const NATIVE_EXECUTABLE: &str = "Contents/MacOS/FrankenCodeBrowser";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    Options, Path, Source, File, Line, Special, Symlink, Bundle, MissingBundle,
    RootGrant, Canceled, Spawn,
}
impl Failure {
    fn code(self) -> &'static str {
        match self {
            Self::Options => "CLI_NATIVE_LAUNCH_OPTIONS",
            Self::Path => "CLI_NATIVE_LAUNCH_PATH",
            Self::Source => "CLI_SOURCE_IO",
            Self::File => "CLI_NATIVE_FILE_OPEN_UNAVAILABLE",
            Self::Line => "CLI_NATIVE_LINE_OPEN_UNAVAILABLE",
            Self::Special => "CLI_SPECIAL_OBJECT_REFUSED",
            Self::Symlink => "CLI_SYMLINK_REFUSED",
            Self::Bundle => "CLI_NATIVE_APP_INVALID",
            Self::MissingBundle => "CLI_NATIVE_APP_NOT_FOUND",
            Self::RootGrant => "CLI_NATIVE_ROOT_GRANT_REQUIRED",
            Self::Canceled => "CLI_CANCELED",
            Self::Spawn => "CLI_NATIVE_LAUNCH_IO",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Options => "GUI launch accepts a project directory or regular file, with --line N on open FILE; use read or open --json for other reader options.",
            Self::Path => "Native launch needs a bounded, NUL-free UTF-8 source path and an absolute .app bundle path.",
            Self::Source => "The selected source or its parent directory could not be inspected.",
            Self::File => "This app does not support native file startup. Rebuild it with scripts/build_macos_app.sh, or use fcb read FILE.",
            Self::Line => "This app does not support native line startup. Rebuild it with scripts/build_macos_app.sh, or use fcb read FILE --line N.",
            Self::Special => "Native opening accepts directories or regular files, never devices, pipes or sockets.",
            Self::Symlink => "The selected final source or project component is a symbolic link.",
            Self::Bundle => "The selected app lacks a valid launcher contract or executable. Rebuild it with scripts/build_macos_app.sh.",
            Self::MissingBundle => "Install a current FrankenCodeBrowser.app, or set FCB_APP_BUNDLE to its absolute .app path.",
            Self::RootGrant => "This sandboxed build needs its native folder picker. Launch without a project argument and grant access there.",
            Self::Canceled => "Canceled before launch; no native launch was submitted.",
            Self::Spawn => "The fixed macOS launch helper could not be started; no launch was submitted.",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BundleMode { ProjectArgument, ProjectAndFileArgument, ProjectFileAndLineArgument, NativePicker }

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaunchRequest { path: Option<PathBuf>, line: Option<u64> }

/// None means the existing headless parser/service owns the request, including
/// all malformed requests and machine-readable output. Never reinterpret them.
fn request(arguments: &[OsString]) -> Option<Result<LaunchRequest, Failure>> {
    // Keep the human-only location option out of the general machine parser.
    // Apply the same admission bounds before allocating the filtered argv, and
    // leave every malformed/JSON request to its existing headless service.
    if arguments.len() > MAX_ARGUMENTS || args::json_requested(arguments) { return None; }
    let mut total = 0usize;
    for argument in arguments {
        if argument.len() > MAX_SINGLE_ARGUMENT { return None; }
        total = total.checked_add(argument.len())?;
        if total > MAX_ARGUMENT_BYTES { return None; }
    }
    let mut filtered = Vec::with_capacity(arguments.len());
    let mut line = None;
    let mut positional_only = false;
    let mut cursor = 0;
    while cursor < arguments.len() {
        let argument = &arguments[cursor]; cursor += 1;
        if !positional_only && argument == "--" { positional_only = true; }
        if !positional_only && argument == "--line" && arguments.first().is_some_and(|a| a == "open") {
            if line.is_some() { return None; }
            let value = args::decimal(arguments.get(cursor)?.to_str()?).ok()?;
            if value == 0 { return None; }
            line = Some(value); cursor += 1;
        } else {
            filtered.push(argument.clone());
        }
    }
    let parsed = args::parse(&filtered).ok()?;
    if parsed.json || !matches!(parsed.command, Command::Launch | Command::Open) { return None; }
    // The general reader parser also accepts options on `open`. They cannot be
    // silently ignored by the GUI, even when their values equal reader defaults.
    let start = usize::from(parsed.command == Command::Open);
    let mut positional_only = false;
    for argument in &filtered[start..] {
        if !positional_only && argument == "--" { positional_only = true; continue; }
        if !positional_only && argument.as_encoded_bytes().first() == Some(&b'-') {
            return Some(Err(Failure::Options));
        }
    }
    Some(Ok(LaunchRequest { path: parsed.file, line }))
}

fn bounded_path(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    !bytes.is_empty() && bytes.len() <= MAX_SINGLE_ARGUMENT && !bytes.contains(&0)
}

fn project_path(path: &Path, cwd: &Path) -> Result<PathBuf, Failure> {
    let result = if path.is_absolute() { path.to_owned() } else { cwd.join(path) };
    if !result.is_absolute() || !bounded_path(&result) || result.to_str().is_none() {
        return Err(Failure::Path);
    }
    // Do not lexically collapse `..` across a symlink or replace native bytes
    // with lossy display text. The native source host still admits the root.
    Ok(result)
}

fn bundle_path(path: &Path) -> bool {
    path.is_absolute() && bounded_path(path) && path.extension().is_some_and(|ext| ext == "app")
}

fn bundled_application(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && bundle_path(app))
        .then_some(app)
}

/// An explicit override is authoritative: never launch a different installation
/// when it is invalid. A CLI inside an app similarly belongs to that app.
fn choose_bundle(
    explicit: Option<&Path>, executable: Option<&Path>, home: Option<&Path>,
    mut inspect: impl FnMut(&Path) -> Result<BundleMode, Failure>,
) -> Result<(PathBuf, BundleMode), Failure> {
    if let Some(path) = explicit.or_else(|| executable.and_then(bundled_application)) {
        if !bundle_path(path) { return Err(Failure::Path); }
        return inspect(path).map(|mode| (path.to_owned(), mode));
    }
    if let Some(home) = home.filter(|home| home.is_absolute() && bounded_path(home)) {
        let path = home.join("Applications/FrankenCodeBrowser.app");
        if bundle_path(&path) {
            match inspect(&path) {
                Ok(mode) => return Ok((path, mode)),
                Err(Failure::MissingBundle) => {},
                Err(error) => return Err(error),
            }
        }
    }
    let path = PathBuf::from("/Applications/FrankenCodeBrowser.app");
    inspect(&path).map(|mode| (path, mode))
}

fn marker_mode(project: bool, picker: bool, file: bool, line: bool) -> Result<BundleMode, Failure> {
    match (project, picker, file, line) {
        (true, false, false, false) => Ok(BundleMode::ProjectArgument),
        (true, false, true, false) => Ok(BundleMode::ProjectAndFileArgument),
        (true, false, true, true) => Ok(BundleMode::ProjectFileAndLineArgument),
        (false, true, false, false) => Ok(BundleMode::NativePicker),
        _ => Err(Failure::Bundle),
    }
}

fn open_arguments(bundle: &Path, mode: BundleMode, root: Option<&Path>)
    -> Result<Vec<OsString>, Failure>
{
    if !bundle_path(bundle) { return Err(Failure::Path); }
    let mut arguments = Vec::with_capacity(6);
    if let Some(root) = root {
        if mode == BundleMode::NativePicker { return Err(Failure::RootGrant); }
        if !root.is_absolute() || !bounded_path(root) || root.to_str().is_none() { return Err(Failure::Path); }
        // Existing instances do not receive argv again. Each explicit project
        // gets its own native process rather than silently opening the old root.
        arguments.push("-n".into());
    }
    arguments.push("-a".into()); arguments.push(bundle.as_os_str().to_owned());
    if let Some(root) = root {
        // ContentView reads fcb.root from UserDefaults. NSArgumentDomain supplies
        // this per-launch override without writing the persistent defaults DB.
        arguments.push("--args".into()); arguments.push("-fcb.root".into());
        arguments.push(root.as_os_str().to_owned());
    }
    Ok(arguments)
}

/// A filename request opens only its immediate parent as the metadata project.
/// Never search ancestors for a VCS root or canonicalize away native path bytes.
fn file_target(path: &Path) -> Result<(PathBuf, OsString), Failure> {
    if !path.is_absolute() || !bounded_path(path) || path.to_str().is_none() {
        return Err(Failure::Path);
    }
    let root = path.parent().ok_or(Failure::Path)?;
    let name = path.file_name().ok_or(Failure::Path)?;
    if !root.is_absolute() || !bounded_path(root) { return Err(Failure::Path); }
    Ok((root.to_owned(), name.to_owned()))
}

/// A single reversible UTF-8 Unix component. Hex keeps leading dashes, quotes,
/// tabs/newlines and backslashes out of the native startup argument syntax.
fn filename_hex(name: &OsStr) -> Result<String, Failure> {
    let text = name.to_str().ok_or(Failure::Path)?;
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SINGLE_ARGUMENT || bytes.contains(&0)
        || bytes.contains(&b'/') || bytes == b"." || bytes == b".." {
        return Err(Failure::Path);
    }
    let mut hex = String::new();
    hex.try_reserve_exact(bytes.len() * 2).map_err(|_| Failure::Path)?;
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    Ok(hex)
}

fn open_file_arguments(bundle: &Path, mode: BundleMode, root: &Path, name: &OsStr, line: Option<u64>)
    -> Result<Vec<OsString>, Failure>
{
    let mut arguments = open_arguments(bundle, mode, Some(root))?;
    // Project-only older bundles must not silently discard the filename.
    if !matches!(mode, BundleMode::ProjectAndFileArgument | BundleMode::ProjectFileAndLineArgument) {
        return Err(Failure::File);
    }
    arguments.push("-fcb.file-hex".into());
    arguments.push(filename_hex(name)?.into());
    if let Some(line) = line {
        if line == 0 { return Err(Failure::Options); }
        if mode != BundleMode::ProjectFileAndLineArgument { return Err(Failure::Line); }
        arguments.push("-fcb.line".into());
        arguments.push(line.to_string().into());
    }
    Ok(arguments)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Handoff { Accepted, Rejected, CanceledAfterSubmission, TimedOut, LostStatus }

/// Model the launch helper, NOT native readiness. Once submitted, cancellation
/// or timeout is an uncertain effect, never a claim that the app was rolled back.
fn await_handoff(
    mut poll: impl FnMut() -> Result<Option<bool>, ()>,
    mut canceled: impl FnMut() -> bool, mut expired: impl FnMut() -> bool,
    mut pause: impl FnMut(),
) -> Handoff {
    loop {
        match poll() {
            Ok(Some(true)) => return Handoff::Accepted,
            Ok(Some(false)) => return Handoff::Rejected,
            Err(()) => return Handoff::LostStatus,
            Ok(None) => {},
        }
        if canceled() { return Handoff::CanceledAfterSubmission; }
        if expired() { return Handoff::TimedOut; }
        pause();
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::{fs, io::Write, os::unix::fs::PermissionsExt,
        process::{Command as Process, Stdio}, time::{Duration, Instant}};

    fn inspect_bundle(path: &Path) -> Result<BundleMode, Failure> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound { Failure::MissingBundle } else { Failure::Bundle }
        })?;
        if !metadata.is_dir() { return Err(Failure::Bundle); }
        let executable = fs::symlink_metadata(path.join(NATIVE_EXECUTABLE)).map_err(|_| Failure::Bundle)?;
        if !executable.is_file() || executable.permissions().mode() & 0o111 == 0 { return Err(Failure::Bundle); }
        // Zero-byte regular marker files are inspected, never opened. A replaced
        // marker cannot trick the launcher into blocking on a FIFO or reading a
        // source-sized file. This is a compatibility contract, not code signing.
        let marker = |name: &str| fs::symlink_metadata(path.join(name))
            .is_ok_and(|entry| entry.is_file() && entry.len() == 0);
        marker_mode(marker(PROJECT_MARKER), marker(PICKER_MARKER), marker(FILE_MARKER), marker(LINE_MARKER))
    }

    fn prepare(path: Option<&Path>, line: Option<u64>) -> Result<Vec<OsString>, Failure> {
        let target = path.map(|path| {
            let cwd = if path.is_absolute() { PathBuf::from("/") }
                else { std::env::current_dir().map_err(|_| Failure::Source)? };
            let path = project_path(path, &cwd)?;
            let metadata = fs::symlink_metadata(&path).map_err(|_| Failure::Source)?;
            if metadata.file_type().is_symlink() { return Err(Failure::Symlink); }
            if metadata.is_file() {
                let (root, name) = file_target(&path)?;
                let parent = fs::symlink_metadata(&root).map_err(|_| Failure::Source)?;
                if parent.file_type().is_symlink() { return Err(Failure::Symlink); }
                if !parent.is_dir() { return Err(Failure::Source); }
                return Ok((root, Some(name)));
            }
            if !metadata.is_dir() { return Err(Failure::Special); }
            if line.is_some() { return Err(Failure::Options); }
            Ok((path, None))
        }).transpose()?;
        let explicit = std::env::var_os("FCB_APP_BUNDLE").map(PathBuf::from);
        let executable = std::env::current_exe().ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let (bundle, mode) = choose_bundle(explicit.as_deref(), executable.as_deref(), home.as_deref(), inspect_bundle)?;
        match target {
            Some((root, Some(name))) => open_file_arguments(&bundle, mode, &root, &name, line),
            Some((root, None)) => open_arguments(&bundle, mode, Some(&root)),
            None => open_arguments(&bundle, mode, None),
        }
    }

    pub(crate) fn run(arguments: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
        mut canceled: impl FnMut() -> bool) -> Option<u8>
    {
        let selected = request(arguments)?;
        let ready = selected.and_then(|request| {
            if canceled() { return Err(Failure::Canceled); }
            let argv = prepare(request.path.as_deref(), request.line)?;
            if canceled() { return Err(Failure::Canceled); }
            Process::new("/usr/bin/open").args(argv)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .spawn().map_err(|_| Failure::Spawn)
        });
        let mut child = match ready {
            Ok(child) => child,
            Err(error) => {
                let _ = writeln!(stderr, "{}: {}", error.code(), error.message());
                return Some(if error == Failure::Canceled { fcb_app::EXIT_CANCELED } else { fcb_app::EXIT_ERROR });
            }
        };
        let started = Instant::now();
        let outcome = await_handoff(
            || child.try_wait().map(|status| status.map(|status| status.success())).map_err(|_| ()),
            &mut canceled, || started.elapsed() >= Duration::from_secs(5),
            || std::thread::sleep(Duration::from_millis(10)),
        );
        if !matches!(outcome, Handoff::Accepted | Handoff::Rejected) {
            // Reap only our /usr/bin/open helper, never the GUI it may already
            // have launched. Keeping that distinction is essential on timeout.
            let _ = child.kill();
            let _ = child.wait();
        }
        if outcome == Handoff::Accepted {
            // Do not turn a late cancellation or a broken receipt pipe into a
            // false no-launch claim. Submission is not successful project load.
            let delivered = stdout.write_all(b"CLI_NATIVE_LAUNCH_SUBMITTED: macOS accepted the app launch; project loading and errors are reported in the native window.\n")
                .and_then(|_| stdout.flush());
            if delivered.is_err() {
                let _ = stderr.write_all(b"CLI_OUTPUT_INTERRUPTED: native launch was submitted; its receipt was not delivered.\n");
                return Some(fcb_app::EXIT_ERROR);
            }
            return Some(fcb_app::EXIT_OK);
        }
        let (code, exit) = match outcome {
            Handoff::Rejected => ("CLI_NATIVE_LAUNCH_REJECTED", fcb_app::EXIT_ERROR),
            Handoff::CanceledAfterSubmission => ("CLI_NATIVE_LAUNCH_CANCELED", fcb_app::EXIT_CANCELED),
            Handoff::TimedOut => ("CLI_NATIVE_LAUNCH_TIMEOUT", fcb_app::EXIT_PARTIAL),
            _ => ("CLI_NATIVE_LAUNCH_UNCERTAIN", fcb_app::EXIT_PARTIAL),
        };
        let _ = writeln!(stderr, "{code}: launch was submitted but not confirmed; the native app may already be open. Check before retrying.");
        Some(exit)
    }
}
#[cfg(target_os = "macos")]
pub(crate) use native::run;

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    fn launch(path: Option<&str>, line: Option<u64>) -> Option<Result<LaunchRequest, Failure>> {
        Some(Ok(LaunchRequest { path: path.map(PathBuf::from), line }))
    }
    #[test]
    fn headless_and_invalid_requests_never_enter_native_launch() {
        for values in [&["--json"][..], &["capabilities"], &["doctor"], &["--help"],
            &["read", "source.rs"], &["open", "source.rs", "--json"],
            &["search", "source.rs", "--text", "needle"], &["desk", "--stdio"],
            &["open"], &["open", "a", "b"], &["open", "root", "--unknown"]] {
            assert_eq!(request(&args(values)), None, "{values:?}");
        }
    }
    #[test]
    fn native_requests_preserve_explicit_scope() {
        assert_eq!(request(&[]), launch(None, None));
        assert_eq!(request(&args(&["project"])), launch(Some("project"), None));
        assert_eq!(request(&args(&["open", "project"])), launch(Some("project"), None));
        assert_eq!(request(&args(&["--", "-project"])), launch(Some("-project"), None));
        assert_eq!(request(&args(&["open", "--", "--json"])), launch(Some("--json"), None));
    }
    #[test]
    fn reader_options_are_not_silently_discarded() {
        for values in [&["open", "root", "--bytes", "65536"][..],
            &["open", "root", "--offset", "0"], &["open", "--stdin"],
            &["open", "root", "--encoding", "auto"]] {
            assert_eq!(request(&args(values)), Some(Err(Failure::Options)), "{values:?}");
        }
    }
    #[test]
    fn native_line_requests_are_exact_bounded_and_leave_machine_output_headless() {
        for values in [&["open", "main.rs", "--line", "120"][..],
            &["open", "--line", "120", "main.rs"]] {
            assert_eq!(request(&args(values)), launch(Some("main.rs"), Some(120)));
        }
        assert_eq!(request(&args(&["open", "--line", "18446744073709551615", "--", "--json"])),
            launch(Some("--json"), Some(u64::MAX)));
        assert_eq!(request(&args(&["open", "--", "--line"])), launch(Some("--line"), None));
        for values in [&["open", "main.rs", "--line", "120", "--json"][..],
            &["read", "main.rs", "--line", "120"], &["open", "--line", "120"],
            &["open", "main.rs", "--line"], &["open", "main.rs", "--line", "1", "--line", "2"],
            &["open", "--", "main.rs", "--line", "2"]] {
            assert_eq!(request(&args(values)), None, "{values:?}");
        }
        for number in ["0", "01", "+1", "-1", "1.0", " 1", "1 ", "18446744073709551616", "--json"] {
            assert_eq!(request(&args(&["open", "main.rs", "--line", number])), None, "{number}");
        }
        let mut excessive = args(&["open", "main.rs", "--line", "1"]);
        excessive.extend((0..MAX_ARGUMENTS).map(|_| OsString::from("--line")));
        assert_eq!(request(&excessive), None);
        assert_eq!(request(&args(&["open", &"x".repeat(MAX_SINGLE_ARGUMENT + 1), "--line", "1"])), None);
        assert_eq!(request(&args(&["open", "main.rs", "--line", "1", "--bytes", "100"])),
            Some(Err(Failure::Options)));
    }
    #[test]
    fn relative_paths_use_the_invoking_directory_without_shell_expansion() {
        assert_eq!(project_path(Path::new("./my project/../資料"), Path::new("/work")).unwrap().as_os_str(),
            std::ffi::OsStr::new("/work/./my project/../資料"));
        assert_eq!(project_path(Path::new("~/code"), Path::new("/work")).unwrap(), Path::new("/work/~/code"));
        assert_eq!(project_path(Path::new("x\0y"), Path::new("/work")), Err(Failure::Path));
        assert_eq!(project_path(Path::new(&"x".repeat(MAX_SINGLE_ARGUMENT)), Path::new("/work")), Err(Failure::Path));
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_projects_are_refused_instead_of_lossily_retargeted() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(b"/source/\xff".to_vec()));
        assert_eq!(project_path(&path, Path::new("/")), Err(Failure::Path));
    }
    #[test]
    fn explicit_bundle_failure_never_falls_back_to_another_installation() {
        let mut visited = Vec::new();
        let result = choose_bundle(Some(Path::new("/chosen/FCB.app")), None, Some(Path::new("/home/user")), |path| {
            visited.push(path.to_owned()); Err(Failure::Bundle)
        });
        assert_eq!(result, Err(Failure::Bundle));
        assert_eq!(visited, [PathBuf::from("/chosen/FCB.app")]);
    }
    #[test]
    fn bundled_cli_uses_its_own_application_not_cwd_or_an_unrelated_app() {
        let result = choose_bundle(None, Some(Path::new("/local/FCB.app/Contents/MacOS/fcb")),
            Some(Path::new("/home/user")), |path| {
                assert_eq!(path, Path::new("/local/FCB.app")); Ok(BundleMode::ProjectArgument)
            }).unwrap();
        assert_eq!(result.0, Path::new("/local/FCB.app"));
        assert_eq!(bundled_application(Path::new("/repository/Contents/MacOS/fcb")), None);
    }
    #[test]
    fn conventional_installations_are_checked_in_a_fixed_order() {
        let mut visited = Vec::new();
        let result = choose_bundle(None, None, Some(Path::new("/Users/test")), |path| {
            visited.push(path.to_owned());
            if visited.len() == 1 { Err(Failure::MissingBundle) } else { Ok(BundleMode::ProjectArgument) }
        }).unwrap();
        assert_eq!(visited, [PathBuf::from("/Users/test/Applications/FrankenCodeBrowser.app"),
            PathBuf::from("/Applications/FrankenCodeBrowser.app")]);
        assert_eq!(result.0, visited[1]);
    }
    #[test]
    fn bundle_markers_distinguish_direct_access_from_sandbox_grants() {
        assert_eq!(marker_mode(true, false, false, false), Ok(BundleMode::ProjectArgument));
        assert_eq!(marker_mode(false, true, false, false), Ok(BundleMode::NativePicker));
        assert_eq!(marker_mode(false, false, false, false), Err(Failure::Bundle));
        assert_eq!(marker_mode(true, true, false, false), Err(Failure::Bundle));
        assert_eq!(open_arguments(Path::new("/App.app"), BundleMode::NativePicker, Some(Path::new("/repo"))),
            Err(Failure::RootGrant));
        assert!(open_arguments(Path::new("/App.app"), BundleMode::NativePicker, None).is_ok());
    }
    #[test]
    fn launch_arguments_are_exact_and_root_requests_force_a_new_instance() {
        let root = Path::new("/work/quotes\" $(touch nope)\n資料");
        let arguments = open_arguments(Path::new("/My App.app"), BundleMode::ProjectArgument, Some(root)).unwrap();
        assert_eq!(arguments, args(&["-n", "-a", "/My App.app", "--args", "-fcb.root", root.to_str().unwrap()]));
        assert_eq!(open_arguments(Path::new("/My App.app"), BundleMode::ProjectArgument, None).unwrap(),
            args(&["-a", "/My App.app"]));
        assert_eq!(open_arguments(Path::new("relative.app"), BundleMode::ProjectArgument, None), Err(Failure::Path));
    }
    #[test]
    fn accepted_and_rejected_helper_statuses_are_not_readiness_claims() {
        assert_eq!(await_handoff(|| Ok(Some(true)), || true, || true, || panic!("no pause")), Handoff::Accepted);
        assert_eq!(await_handoff(|| Ok(Some(false)), || false, || false, || panic!("no pause")), Handoff::Rejected);
    }
    #[test]
    fn cancellation_timeout_and_lost_status_preserve_uncertain_launch_effects() {
        assert_eq!(await_handoff(|| Ok(None), || true, || false, || panic!("no pause")), Handoff::CanceledAfterSubmission);
        assert_eq!(await_handoff(|| Ok(None), || false, || true, || panic!("no pause")), Handoff::TimedOut);
        assert_eq!(await_handoff(|| Err(()), || false, || false, || panic!("no pause")), Handoff::LostStatus);
        let mut count = 0;
        assert_eq!(await_handoff(|| { count += 1; Ok((count == 3).then_some(true)) },
            || false, || false, || {}), Handoff::Accepted);
        assert_eq!(count, 3);
    }
    #[test]
    fn file_opening_uses_the_immediate_parent_not_a_guessed_repository_root() {
        let path = project_path(Path::new("src/main.rs"), Path::new("/work/project")).unwrap();
        assert_eq!(file_target(&path).unwrap(), (PathBuf::from("/work/project/src"), OsString::from("main.rs")));
        assert_eq!(file_target(Path::new("/main.rs")).unwrap(), (PathBuf::from("/"), OsString::from("main.rs")));
        assert_eq!(file_target(Path::new("relative.rs")), Err(Failure::Path));
        assert_eq!(file_target(Path::new("/")), Err(Failure::Path));
        assert_eq!(file_target(Path::new("/work/..")), Err(Failure::Path));
    }
    #[test]
    fn file_launch_preserves_the_exact_native_name_as_one_hex_argument() {
        let (root, name) = file_target(Path::new("/work/My Project/é.rs")).unwrap();
        let arguments = open_file_arguments(Path::new("/My App.app"), BundleMode::ProjectAndFileArgument, &root, &name, None).unwrap();
        assert_eq!(arguments, args(&["-n", "-a", "/My App.app", "--args", "-fcb.root",
            "/work/My Project", "-fcb.file-hex", "c3a92e7273"]));
        assert_ne!(filename_hex(OsStr::new("é.rs")).unwrap(), filename_hex(OsStr::new("e\u{301}.rs")).unwrap());
    }
    #[test]
    fn quoted_control_and_flag_shaped_filenames_never_become_arguments() {
        for (name, expected) in [("-fcb.root", "2d6663622e726f6f74"), ("a b", "612062"),
            ("a\nb", "610a62"), ("a\tb", "610962"), ("a\\b", "615c62"), ("a\"b", "612262")] {
            assert_eq!(filename_hex(OsStr::new(name)).unwrap(), expected);
            let arguments = open_file_arguments(Path::new("/App.app"), BundleMode::ProjectAndFileArgument,
                Path::new("/repo"), OsStr::new(name), None).unwrap();
            assert_eq!(arguments.len(), 8);
            assert_eq!(arguments[7], OsStr::new(expected));
        }
    }
    #[test]
    fn file_markers_are_additive_but_cannot_cross_the_sandbox_boundary() {
        for project in [false, true] { for picker in [false, true] { for file in [false, true] { for line in [false, true] {
            let expected = match (project, picker, file, line) {
                (true, false, false, false) => Ok(BundleMode::ProjectArgument),
                (true, false, true, false) => Ok(BundleMode::ProjectAndFileArgument),
                (true, false, true, true) => Ok(BundleMode::ProjectFileAndLineArgument),
                (false, true, false, false) => Ok(BundleMode::NativePicker),
                _ => Err(Failure::Bundle),
            };
            assert_eq!(marker_mode(project, picker, file, line), expected);
        } } } }
        assert!(open_arguments(Path::new("/App.app"), BundleMode::ProjectAndFileArgument, None).is_ok());
        assert!(open_arguments(Path::new("/App.app"), BundleMode::ProjectAndFileArgument, Some(Path::new("/repo"))).is_ok());
    }
    #[test]
    fn old_bundles_and_sandboxed_bundles_do_not_silently_drop_file_selection() {
        assert_eq!(open_file_arguments(Path::new("/App.app"), BundleMode::ProjectArgument,
            Path::new("/repo"), OsStr::new("main.rs"), None), Err(Failure::File));
        assert_eq!(open_file_arguments(Path::new("/App.app"), BundleMode::NativePicker,
            Path::new("/repo"), OsStr::new("main.rs"), None), Err(Failure::RootGrant));
    }
    #[test]
    fn line_handoff_requires_a_matching_bundle_and_preserves_full_width_values() {
        let arguments = open_file_arguments(Path::new("/App.app"), BundleMode::ProjectFileAndLineArgument,
            Path::new("/repo"), OsStr::new("-fcb.line"), Some(u64::MAX)).unwrap();
        assert_eq!(arguments, args(&["-n", "-a", "/App.app", "--args", "-fcb.root", "/repo",
            "-fcb.file-hex", "2d6663622e6c696e65", "-fcb.line", "18446744073709551615"]));
        assert_eq!(open_file_arguments(Path::new("/App.app"), BundleMode::ProjectAndFileArgument,
            Path::new("/repo"), OsStr::new("main.rs"), Some(120)), Err(Failure::Line));
        assert_eq!(open_file_arguments(Path::new("/App.app"), BundleMode::ProjectFileAndLineArgument,
            Path::new("/repo"), OsStr::new("main.rs"), Some(0)), Err(Failure::Options));
        assert_eq!(open_file_arguments(Path::new("/App.app"), BundleMode::NativePicker,
            Path::new("/repo"), OsStr::new("main.rs"), Some(120)), Err(Failure::RootGrant));
    }
    #[test]
    fn file_argument_validation_rejects_traversal_and_preserves_size_limits() {
        for name in ["", ".", "..", "a/b", "/absolute", "a\0b"] {
            assert_eq!(filename_hex(OsStr::new(name)), Err(Failure::Path));
        }
        assert_eq!(filename_hex(OsStr::new(&"x".repeat(MAX_SINGLE_ARGUMENT))).unwrap().len(), MAX_SINGLE_ARGUMENT * 2);
        assert_eq!(filename_hex(OsStr::new(&"x".repeat(MAX_SINGLE_ARGUMENT + 1))), Err(Failure::Path));
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_file_names_are_never_lossily_retargeted() {
        use std::os::unix::ffi::OsStringExt;
        let name = OsString::from_vec(vec![0xff]);
        assert_eq!(filename_hex(&name), Err(Failure::Path));
        assert_eq!(file_target(&Path::new("/repo").join(name)), Err(Failure::Path));
    }

}
