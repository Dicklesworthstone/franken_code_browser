#![forbid(unsafe_code)]

//! Explicit native-directory -> renderer-neutral atlas plan. The CLI consumes
//! the public WorkspaceAtlas, camera and visible-query APIs, not a second map
//! algorithm. A plan is never acknowledged as a physically presented frame.
use std::{ffi::OsString, fs, io::Write, path::{Component, Path, PathBuf}};
use fcb::{ByteLength, CameraGeneration, DisplayGeneration, DisplayMetrics, Point2D, Rect2D, Size2D};
use fcb::map::{AtlasDetail, AtlasError, AtlasIndex, AtlasNodeId, Camera2D, CameraError,
    DisplayColorConfig, LayoutOptions, LayoutRevision, LodThresholds, VisibleLimits, VisibleQuery, VisibleState};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasError, WorkspaceAtlasLimits};
use fcb::search::{RawPath, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, RuleLimits, WorkspaceCatalog, WorkspaceError, WorkspaceLimits, WorkspaceStage};
use fcb::source::CancelFlag;
use crate::{AppError, EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED, MANAGED_BYTES,
    allocation, file_id, generation, owner, input, workspace};
use crate::args::{self, ArgumentError, Arguments};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};

const HELP: &str = "Repository atlas plans\n\n\
  fcb atlas ROOT [--json] [--focus RELATIVE_PATH] [--select RELATIVE_FILE]\n\
  --width N --height N        Viewport logical points (default 1024 x 768)\n\
  --scale F                  Physical pixels per point (default 1)\n\
  --zoom F                   Zoom about viewport center after fitting focus\n\
  --pan-x F --pan-y F         Content translation in logical points, after zoom\n\
  --detail-pixels F           Expand threshold in physical pixels (default 48)\n\
  --limit N --max-visits N    Parcel and traversal limits (default 1024 / 32768)\n\
  --max-files N               Discovery file limit (default 4096)\n\
  --respect-ignores OR --include-excluded\n\n\
This command explicitly authorizes bounded directory discovery. No source\n\
payloads are read. Repository rule files require --respect-ignores separately.\n\
Files retain native byte paths and response-local FileIds. Only catalogued\n\
regular files and their ancestors are mapped; empty/unavailable directories\n\
are not a complete directory inventory. Partial discovery stays explicit.\n\
Rectangles are clipped logical-viewport points, NOT native presented pixels.\n\
Budget pressure produces labelled aggregates, never invented file hits.\n\
The selected file remains identified even when aggregated or outside focus.\n\
Exit: 0 complete plan, 2 error, 3 incomplete discovery/detail, 130 canceled.\n\
Implemented-unqualified; independent Rust/RCH and native verification pending.\n";

#[derive(Debug)]
struct Options {
    workspace: Arguments,
    focus: Option<PathBuf>, select: Option<PathBuf>,
    width: f64, height: f64, scale: f64, zoom: f64, pan_x: f64, pan_y: f64,
    detail_pixels: f64, max_visits: usize,
}
#[derive(Clone, Copy, Debug)]
enum Error { App(AppError), Atlas(WorkspaceAtlasError), FocusMissing, SelectionMissing }
impl From<AppError> for Error { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<ArgumentError> for Error { fn from(e: ArgumentError) -> Self { Self::App(e.into()) } }
impl From<OutputError> for Error { fn from(e: OutputError) -> Self { Self::App(e.into()) } }
impl From<WorkspaceError> for Error { fn from(e: WorkspaceError) -> Self { Self::App(e.into()) } }
impl From<WorkspaceAtlasError> for Error { fn from(e: WorkspaceAtlasError) -> Self { Self::Atlas(e) } }
impl From<AtlasError> for Error { fn from(e: AtlasError) -> Self { Self::Atlas(e.into()) } }
impl From<CameraError> for Error { fn from(e: CameraError) -> Self { AtlasError::Camera(e).into() } }
impl Error {
    fn code(self) -> String {
        match self {
            Self::App(e) => e.code(), Self::Atlas(e) => e.to_string(),
            Self::FocusMissing => "CLI_ATLAS_FOCUS_NOT_CATALOGUED".to_owned(),
            Self::SelectionMissing => "CLI_ATLAS_SELECTION_NOT_CATALOGUED".to_owned(),
        }
    }
    fn canceled(self) -> bool {
        match self {
            Self::App(e) => e.is_canceled(),
            Self::Atlas(WorkspaceAtlasError::Atlas(AtlasError::Canceled)
                | WorkspaceAtlasError::Workspace(WorkspaceError::Canceled)) => true,
            _ => false,
        }
    }
}
fn takes_value(option: &str) -> bool {
    matches!(option, "--focus" | "--select" | "--width" | "--height" | "--scale" | "--zoom"
        | "--pan-x" | "--pan-y" | "--detail-pixels" | "--limit" | "--max-visits" | "--max-files")
}
fn json_requested(arguments: &[OsString]) -> bool {
    let mut cursor = 0;
    while cursor < arguments.len().min(args::MAX_ARGUMENTS + 1) {
        let arg = &arguments[cursor]; cursor += 1;
        if arg == "--" { break; }
        if arg == "--json" { return true; }
        if arg.to_str().is_some_and(takes_value) { cursor += 1; }
    }
    false
}
fn real(text: &str, min: f64, max: f64) -> Result<f64, Error> {
    let n = text.parse::<f64>().map_err(|_| ArgumentError::InvalidNumber)?;
    if !n.is_finite() || !(min..=max).contains(&n) { return Err(ArgumentError::InvalidNumber.into()); }
    Ok(n)
}
fn parse(arguments: &[OsString]) -> Result<Options, Error> {
    if arguments.len() > args::MAX_ARGUMENTS { return Err(ArgumentError::Limit.into()); }
    let mut total = 0usize;
    for arg in arguments {
        total = total.checked_add(arg.len()).ok_or(ArgumentError::Limit)?;
        if arg.len() > args::MAX_SINGLE_ARGUMENT || total > args::MAX_ARGUMENT_BYTES {
            return Err(ArgumentError::Limit.into());
        }
    }
    // Reuse the established workspace permission/parser contract. View-specific
    // values are consumed here; they can never turn into discovery switches.
    let mut forwarded = vec![OsString::from("inspect"), OsString::from("--workspace")];
    let (mut focus, mut select) = (None, None);
    let (mut width, mut height, mut scale, mut zoom) = (1024.0, 768.0, 1.0, 1.0);
    let (mut pan_x, mut pan_y, mut detail_pixels, mut max_visits) = (0.0, 0.0, 48.0, 32_768usize);
    let (mut cursor, mut seen) = (0usize, 0u32);
    let (mut positional, mut limit_seen) = (false, false);
    while cursor < arguments.len() {
        let arg = &arguments[cursor]; cursor += 1;
        if !positional && arg == "--" { positional = true; forwarded.push(arg.clone()); continue; }
        let option = if positional { None } else { arg.to_str().filter(|s| s.starts_with('-')) };
        let Some(option) = option else { forwarded.push(arg.clone()); continue; };
        if matches!(option, "--json" | "--respect-ignores" | "--include-excluded") {
            forwarded.push(arg.clone()); continue;
        }
        if matches!(option, "--limit" | "--max-files") {
            let value = arguments.get(cursor).ok_or(ArgumentError::MissingValue)?; cursor += 1;
            forwarded.push(arg.clone()); forwarded.push(value.clone());
            limit_seen |= option == "--limit"; continue;
        }
        let bit = match option {
            "--focus" => 1, "--select" => 2, "--width" => 4, "--height" => 8,
            "--scale" => 16, "--zoom" => 32, "--pan-x" => 64, "--pan-y" => 128,
            "--detail-pixels" => 256, "--max-visits" => 512,
            _ => return Err(ArgumentError::UnknownOption.into()),
        };
        if seen & bit != 0 { return Err(ArgumentError::DuplicateOption.into()); } seen |= bit;
        let value = arguments.get(cursor).ok_or(ArgumentError::MissingValue)?; cursor += 1;
        if option == "--focus" || option == "--select" {
            if value.is_empty() { return Err(ArgumentError::MissingValue.into()); }
            let path = PathBuf::from(value);
            if option == "--focus" { focus = Some(path); } else { select = Some(path); }
            continue;
        }
        let value = value.to_str().ok_or(ArgumentError::InvalidNumber)?;
        match option {
            "--width" | "--height" => {
                let n = args::decimal(value)?;
                if !(64..=16_384).contains(&n) { return Err(ArgumentError::Limit.into()); }
                if option == "--width" { width = n as f64; } else { height = n as f64; }
            }
            "--scale" => scale = real(value, 0.25, 8.0)?,
            "--zoom" => zoom = real(value, 1.0 / 65_536.0, 65_536.0)?,
            "--pan-x" => pan_x = real(value, -1e9, 1e9)?,
            "--pan-y" => pan_y = real(value, -1e9, 1e9)?,
            "--detail-pixels" => detail_pixels = real(value, 0.001, 4096.0)?,
            "--max-visits" => {
                let n = args::decimal(value)?;
                if !(1..=1_000_000).contains(&n) { return Err(ArgumentError::Limit.into()); }
                max_visits = n as usize;
            }
            _ => unreachable!(),
        }
    }
    // Insert before a possible positional-only delimiter, not after it.
    if !limit_seen { forwarded.splice(2..2, [OsString::from("--limit"), OsString::from("1024")]); }
    let workspace = args::parse(&forwarded)?;
    if workspace.limit == 0 { return Err(ArgumentError::Limit.into()); }
    Ok(Options { workspace, focus, select, width, height, scale, zoom, pan_x, pan_y, detail_pixels, max_visits })
}

pub(crate) fn run(arguments: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    let json = json_requested(arguments);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(b) => b, Err(_) => { let _ = stderr.write(b"CLI_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(1)) {
        Ok(o) => o, Err(_) => { let _ = stderr.write(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let help = !arguments.is_empty() && arguments.len() <= 2
        && arguments.iter().any(|arg| arg == "--help" || arg == "-h")
        && arguments.iter().all(|arg| arg == "--help" || arg == "-h" || arg == "--json");
    let result = if help {
        if json { out.literal("{\"schema\":\"fcb.cli/1\",\"command\":\"atlas-help\",\"text\":")
            .and_then(|_| out.quoted(HELP)).and_then(|_| out.literal("}\n")).map(|_| EXIT_OK).map_err(Error::from) }
        else { out.literal(HELP).map(|_| EXIT_OK).map_err(Error::from) }
    } else { parse(arguments).and_then(|options| execute(&options, &mut out, &budget, &mut canceled)) };
    let result = if canceled() && result.is_ok() { Err(AppError::Canceled.into()) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            let encoded = if json {
                out.literal("{\"schema\":\"fcb.cli/1\",\"command\":\"atlas\",\"status\":\"error\",\"complete\":false,\"error\":{\"code\":")
                    .and_then(|_| out.quoted(&error.code()))
                    .and_then(|_| out.literal(",\"next_action\":\"Run fcb atlas --help; check root, relative paths and limits.\"}}\n"))
            } else { out.literal(&error.code()).and_then(|_| out.literal("; run fcb atlas --help\n")) };
            if encoded.is_err() { let _ = stderr.write(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR; }
            if error.canceled() { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut delivery_canceled = false;
    let mut stop = || { if exit != EXIT_CANCELED && canceled() { delivery_canceled = true; true } else { false } };
    let delivery = if json || matches!(exit, EXIT_OK | EXIT_PARTIAL) { out.deliver(stdout, 4096, &mut stop) }
        else { out.deliver(stderr, 4096, &mut stop) };
    if delivery.is_err() {
        let _ = stderr.write(b"CLI_OUTPUT_INTERRUPTED: atlas delivery incomplete\n");
        return if delivery_canceled { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}

fn execute(options: &Options, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, Error> {
    if canceled() { return Err(AppError::Canceled.into()); }
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
    let args = &options.workspace;
    let requested = input::absolute(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
    let meta = fs::symlink_metadata(&requested).map_err(|_| AppError::Io)?;
    if meta.file_type().is_symlink() { return Err(AppError::Symlink.into()); }
    if !meta.is_dir() { return Err(AppError::InvalidRange.into()); }
    let root = fs::canonicalize(&requested).map_err(|_| AppError::Io)?;
    let grant = RootGrant::new(RootId::new(owner(), 1).map_err(|_| AppError::InvalidRange)?, RawPath::from_path(&root));
    let limits = WorkspaceLimits { max_files: args.max_files, max_file_bytes: 0, max_source_bytes: 0, ..WorkspaceLimits::default() };
    let id = SearchManifestId::new(owner(), 1).map_err(AppError::from)?;
    let mut catalog = if args.respect_ignores {
        WorkspaceCatalog::open_rule_aware(grant, id, file_id(), limits, RuleLimits::default(), budget, [allocation(30), allocation(39)])?
    } else { WorkspaceCatalog::open(grant, id, file_id(), limits, args.include_excluded, budget, allocation(30))? };
    let cancel = CancelFlag::new();
    while catalog.stage() == WorkspaceStage::Discovering {
        if canceled() { return Err(AppError::Canceled.into()); }
        catalog.step(&cancel)?;
    }
    let size = Size2D::new(options.width, options.height).map_err(|_| AppError::InvalidRange)?;
    let atlas = WorkspaceAtlas::build(&catalog, LayoutRevision::new(owner(), 1).map_err(|_| AppError::InvalidRange)?,
        size, LayoutOptions::modest(), WorkspaceAtlasLimits::default(), budget, allocation(60), &mut *canceled)?;
    let index = atlas.index(budget, allocation(61), &mut *canceled)?;
    let focus = match options.focus.as_deref() {
        Some(path) => lookup(&index, path)?.ok_or(Error::FocusMissing)?, None => index.root_node(),
    };
    let selected = match options.select.as_deref() {
        Some(path) => {
            let node = lookup(&index, path)?.ok_or(Error::SelectionMissing)?;
            atlas.file(node)?; Some(node)
        }
        None => None,
    };
    let display = DisplayMetrics::new(options.scale, size, DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner(), 1).map_err(|_| AppError::InvalidRange)?).map_err(|_| AppError::InvalidRange)?;
    let camera = index.focus_camera(focus, CameraGeneration::new(owner(), 1).map_err(|_| AppError::InvalidRange)?, display, 12.0)?
        .zoom_at(Point2D::new(options.width / 2.0, options.height / 2.0).map_err(|_| AppError::InvalidRange)?, options.zoom)?
        .pan(Point2D::new(options.pan_x, options.pan_y).map_err(|_| AppError::InvalidRange)?)?;
    let mut query = VisibleQuery::new(&index, focus, camera, generation(),
        LodThresholds::new(options.detail_pixels, options.detail_pixels * 2.0 / 3.0)?,
        VisibleLimits { max_items: args.limit, max_visits: options.max_visits }, None, budget, allocation(62))?;
    while query.state() == VisibleState::Pending {
        catalog.validate_active()?;
        query.step(256, generation(), &mut *canceled)?;
    }
    let plan = query.finish()?;
    let stats = plan.stats();
    let detail_limited = stats.budget_aggregates != 0 || stats.precision_aggregates != 0;
    if args.json {
        workspace::common(out, "atlas", &catalog, &root)?;
        out.literal(",\"atlas_schema\":\"fcb.atlas/1\",\"qualification\":\"implemented-unqualified\",\"plan_complete\":true,\"native_presented\":false,\"payload_bytes_read\":\"0\",\"hierarchy_scope\":\"catalogued-files-and-ancestors\",\"metric\":")?;
        out.quoted(atlas.layout().options().metric().name())?;
        out.literal(",\"layout_revision\":")?; out.integer(index.revision().get())?;
        out.literal(",\"hierarchy_nodes\":")?; out.integer(index.len() as u64)?;
        out.literal(",\"detail_limited\":")?; out.boolean(detail_limited)?;
        out.literal(",\"camera\":{\"generation\":")?; out.integer(camera.generation().get())?;
        out.literal(",\"origin_x\":")?; number(out, camera.origin().x())?;
        out.literal(",\"origin_y\":")?; number(out, camera.origin().y())?;
        out.literal(",\"points_per_unit\":")?; number(out, camera.points_per_unit())?;
        out.literal(",\"width_points\":")?; number(out, options.width)?;
        out.literal(",\"height_points\":")?; number(out, options.height)?;
        out.literal(",\"pixels_per_point\":")?; number(out, options.scale)?;
        out.literal("},\"focus\":")?; node_record(out, &index, focus)?;
        out.literal(",\"selection\":")?;
        if let Some(node) = selected {
            out.literal("{\"node\":")?; node_record(out, &index, node)?;
            out.literal(",\"file_id\":")?; out.integer(atlas.file(node)?.get())?;
            out.literal(",\"logical_rect\":")?;
            if let Some(rect) = projected_selection(&index, node, focus, camera)? { rectangle(out, rect)?; }
            else { out.literal("null")?; }
            out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal(",\"parcels\":[")?;
        for (i, parcel) in plan.parcels().iter().enumerate() {
            if canceled() { return Err(AppError::Canceled.into()); }
            if i > 0 { out.literal(",")?; }
            out.literal("{\"node\":")?; node_record(out, &index, parcel.node())?;
            out.literal(",\"detail\":")?; out.quoted(detail(parcel.detail()))?;
            out.literal(",\"reason\":")?; out.quoted(&format!("{:?}", parcel.reason()))?;
            out.literal(",\"represented_leaves\":")?; out.integer(parcel.represented_leaves() as u64)?;
            out.literal(",\"logical_rect\":")?; rectangle(out, parcel.logical_rect())?;
            out.literal(",\"file_id\":")?;
            if parcel.is_source_parcel() { out.integer(atlas.file(parcel.node())?.get())?; } else { out.literal("null")?; }
            out.literal("}")?;
        }
        out.literal("],\"traversal\":{\"visited_nodes\":")?; out.integer(stats.visited_nodes as u64)?;
        out.literal(",\"culled_regions\":")?; out.integer(stats.culled_regions as u64)?;
        out.literal(",\"budget_aggregates\":")?; out.integer(stats.budget_aggregates as u64)?;
        out.literal(",\"precision_aggregates\":")?; out.integer(stats.precision_aggregates as u64)?;
        out.literal(",\"distance_aggregates\":")?; out.integer(stats.distance_aggregates as u64)?;
        out.literal(",\"max_items\":")?; out.integer(args.limit as u64)?;
        out.literal(",\"max_visits\":")?; out.integer(options.max_visits as u64)?;
        out.literal("}}\n")?;
    } else {
        out.literal("Retained repository atlas plan (not a native presented frame)\n")?;
        workspace::rule_output::human(out, &catalog)?;
        for parcel in plan.parcels() {
            if canceled() { return Err(AppError::Canceled.into()); }
            out.literal(detail(parcel.detail()))?; out.literal(" ")?;
            out.path(&RawPath::from_bytes(index.node(parcel.node())?.path()).to_path_buf())?;
            out.literal(" ")?; rectangle(out, parcel.logical_rect())?; out.literal("\n")?;
        }
        if let Some(node) = selected {
            out.literal("Selected file: ")?; out.path(&atlas.entry(node)?.path().raw().to_path_buf())?; out.literal("\n")?;
        }
        out.literal("Source payload bytes read: 0\n")?;
        if !catalog.discovery_complete() || detail_limited { out.literal("PARTIAL discovery or aggregate-limited detail\n")?; }
    }
    atlas.validate_active()?;
    if canceled() { return Err(AppError::Canceled.into()); }
    Ok(if catalog.discovery_complete() && !detail_limited { EXIT_OK } else { EXIT_PARTIAL })
}
fn lookup(index: &AtlasIndex<'_>, path: &Path) -> Result<Option<AtlasNodeId>, Error> {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part { Component::Normal(name) => normalized.push(name), Component::CurDir => {},
            _ => return Err(AppError::InvalidRange.into()) }
    }
    Ok(index.find_path(RawPath::from_path(&normalized).as_bytes()))
}
fn projected_selection(index: &AtlasIndex<'_>, node: AtlasNodeId, focus: AtlasNodeId,
    camera: Camera2D) -> Result<Option<Rect2D>, Error> {
    match index.bounds_in(node, focus) {
        Ok(bounds) => Ok(camera.project_clipped(bounds)?),
        Err(AtlasError::NotDescendant) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn node_record(out: &mut Output, index: &AtlasIndex<'_>, node: AtlasNodeId) -> Result<(), Error> {
    out.literal("{\"ordinal\":")?; out.integer(u64::from(node.ordinal()))?;
    out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(index.node(node)?.path()).to_path_buf())?;
    out.literal("}")?; Ok(())
}
fn number(out: &mut Output, value: f64) -> Result<(), Error> {
    if !value.is_finite() { return Err(AppError::InvalidRange.into()); }
    out.literal(&value.to_string())?; Ok(())
}
fn rectangle(out: &mut Output, rect: Rect2D) -> Result<(), Error> {
    out.literal("{\"x\":")?; number(out, rect.min_x())?;
    out.literal(",\"y\":")?; number(out, rect.min_y())?;
    out.literal(",\"width\":")?; number(out, rect.size().width())?;
    out.literal(",\"height\":")?; number(out, rect.size().height())?;
    out.literal("}")?; Ok(())
}
fn detail(detail: AtlasDetail) -> &'static str {
    match detail { AtlasDetail::File => "file", AtlasDetail::Placeholder => "placeholder",
        AtlasDetail::Directory => "directory", AtlasDetail::SiblingGroup => "sibling-group" }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn argv(args: &[&str]) -> Vec<OsString> { args.iter().map(OsString::from).collect() }
    #[test]
    fn values_and_delimiters_cannot_escalate_discovery_or_change_output_mode() {
        assert!(!json_requested(&argv(&["root", "--focus", "--json"])));
        assert!(!json_requested(&argv(&["--", "--json"])));
        assert!(json_requested(&argv(&["root", "--focus", "--json", "--json"])));
        let parsed = parse(&argv(&["--focus", "--respect-ignores", "--", "--json"])).unwrap();
        assert!(!parsed.workspace.respect_ignores); assert!(!parsed.workspace.json);
        assert_eq!(parsed.workspace.file.unwrap(), PathBuf::from("--json"));
        assert_eq!(parsed.workspace.limit, 1024);
    }
    #[test]
    fn invalid_nonfinite_duplicate_or_unrelated_options_are_refused_before_io() {
        for args in [vec!["root", "--zoom", "NaN"], vec!["root", "--scale", "inf"],
            vec!["root", "--width", "0"], vec!["root", "--limit", "0"],
            vec!["root", "--max-visits", "0"], vec!["root", "--zoom", "2", "--zoom", "3"],
            vec!["root", "--stdin"], vec!["root", "--text", "x"],
            vec!["root", "--respect-ignores", "--include-excluded"]] {
            assert!(parse(&argv(&args)).is_err(), "{args:?}");
        }
    }
}
