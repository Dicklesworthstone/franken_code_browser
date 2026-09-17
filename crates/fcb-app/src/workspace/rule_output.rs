#![forbid(unsafe_code)]

//! Shared live-workspace/snapshot-save rule evidence. Configuration I/O is not
//! source-payload I/O. Receipt paths are reversible metadata; rule contents are
//! not copied into diagnostics, shell commands or terminal control sequences.

use fcb::search::{RawPath, workspace::WorkspaceCatalog};
use crate::output::{Output, OutputError};

pub(crate) fn fields(out: &mut Output, catalog: &WorkspaceCatalog) -> Result<(), OutputError> {
    out.literal(",\"rule_files_enabled\":")?; out.boolean(catalog.reads_rule_files())?;
    out.literal(",\"rule_policy\":")?;
    let Some(policy) = catalog.rule_policy() else { return out.literal("null"); };
    let stats = policy.stats();
    out.literal("{\"observed_rules_complete\":")?; out.boolean(stats.is_complete())?;
    out.literal(",\"scope_complete\":")?; out.boolean(stats.is_complete() && catalog.discovery_complete())?;
    for (key, value) in [
        ("configuration_bytes_read", stats.bytes_read), ("configuration_read_calls", stats.read_calls),
        ("configuration_checks", stats.checks), ("files_read", stats.files_read as u64),
        ("files_loaded", stats.files_loaded as u64), ("rules_loaded", stats.rules_loaded as u64),
        ("compiled_admission_bytes", stats.compiled_admission_bytes as u64), ("match_steps", stats.match_steps),
        ("failed_files", stats.failed_files as u64), ("unresolved_paths", stats.unresolved_paths as u64),
        ("diagnostics_omitted", stats.diagnostics_omitted as u64),
    ] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    let limits = policy.limits();
    out.literal(",\"limits\":{")?;
    for (position, (key, value)) in [
        ("max_file_bytes", limits.max_file_bytes as u64), ("max_total_bytes", limits.max_total_bytes as u64),
        ("max_files", limits.max_files as u64), ("max_checks", limits.max_checks),
        ("max_read_calls", limits.max_read_calls), ("max_rules", limits.max_rules as u64),
        ("max_pattern_bytes", limits.max_pattern_bytes as u64), ("max_compiled_bytes", limits.max_compiled_bytes as u64),
        ("max_match_steps", limits.max_match_steps), ("max_total_match_steps", limits.max_total_match_steps),
    ].into_iter().enumerate() {
        if position > 0 { out.literal(",")?; }
        out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal("},\"diagnostics\":[")?;
    for (position, diagnostic) in policy.diagnostics().iter().enumerate() {
        if position > 0 { out.literal(",")?; }
        out.literal("{\"path\":")?;
        out.path(&RawPath::from_bytes(diagnostic.path()).to_path_buf())?;
        out.literal(",\"code\":")?; out.quoted(diagnostic.error().code())?; out.literal("}")?;
    }
    out.literal("]}")
}

pub(crate) fn human(out: &mut Output, catalog: &WorkspaceCatalog) -> Result<(), OutputError> {
    let Some(policy) = catalog.rule_policy() else { return Ok(()); };
    let stats = policy.stats();
    out.literal("Repository rules: ")?; out.literal(&stats.files_loaded.to_string())?;
    out.literal(" files; configuration bytes read: ")?; out.literal(&stats.bytes_read.to_string())?;
    out.literal(" (separate from source payload).\n")?;
    if !stats.is_complete() {
        out.literal("PARTIAL rule scope: affected directories/paths were withheld, not treated as included or deleted.\n")?;
    }
    for diagnostic in policy.diagnostics() {
        out.path(&RawPath::from_bytes(diagnostic.path()).to_path_buf())?;
        out.literal(": ")?; out.literal(diagnostic.error().code())?; out.literal("\n")?;
    }
    if stats.diagnostics_omitted > 0 {
        out.literal("Additional rule diagnostics omitted: ")?; out.literal(&stats.diagnostics_omitted.to_string())?; out.literal("\n")?;
    }
    Ok(())
}
