//! Bounded wire publication for retained saved-capture comparisons. Row IDs
//! remain positions in the full sorted merge, regardless of page/filter.
use super::*;
use fcb::search::paged_snapshot::{PagedMember, SnapshotDirectory};

impl SnapshotComparisonSession {
    pub(super) fn output(&mut self, command: &str, publication: Option<&RetainedComparison>)
        -> Result<Output, SnapshotComparisonError> {
        let [id] = self.allocations()?;
        let mut out = Output::new(self.owner(), MAX_SNAPSHOT_COMPARISON_RESPONSE, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.snapshot-comparison/1\",\"status\":\"ok\",\"command\":")?;
        out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner().get())?;
        out.literal(",\"before_archive\":")?; archive(&mut out, &self.before, &self.before_path)?;
        out.literal(",\"after_archive\":")?; archive(&mut out, &self.after, &self.after_path)?;
        out.literal(",\"live_source_reopened\":false,\"live_source_bytes_read\":\"0\",\"history_inferred\":false,\"renames_inferred\":false")?;
        out.literal(",\"alignment\":\"native-path-pair-not-logical-file-continuity\",\"native_presented\":false")?;
        out.literal(",\"last_comparison_attempt\":")?; out.integer(self.last_attempt)?;
        let accepted = publication.filter(|s| s.cursor.finished()).map(|s| s.generation.get())
            .or_else(|| self.accepted_generation());
        let pending = match publication {
            Some(state) => (!state.cursor.finished()).then_some(state.generation.get()),
            None => self.pending_generation(),
        };
        out.literal(",\"accepted_comparison_generation\":")?; optional(&mut out, accepted)?;
        out.literal(",\"pending_comparison_generation\":")?; optional(&mut out, pending)?;
        for (key, value) in [("max_members_per_archive", self.limits.max_files),
            ("max_path_bytes", self.limits.max_path_bytes), ("max_source_file_bytes", self.limits.max_file_bytes),
            ("max_archive_bytes", self.limits.max_document_bytes),
            ("max_retained_entries", MAX_SNAPSHOT_COMPARISON_ENTRIES),
            ("max_response_bytes", MAX_SNAPSHOT_COMPARISON_RESPONSE)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
        }
        Ok(out)
    }

    pub(super) fn encode_page(&self, out: &mut Output, state: &RetainedComparison,
        filter: SnapshotChangeFilter, start: usize, limit: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<(), SnapshotComparisonError> {
        let stats = state.cursor.stats();
        let total = match filter {
            SnapshotChangeFilter::All => stats.compared_paths,
            SnapshotChangeFilter::Differences => stats.compared_paths - stats.unchanged,
            SnapshotChangeFilter::Unchanged => stats.unchanged, SnapshotChangeFilter::Changed => stats.changed,
            SnapshotChangeFilter::Added => stats.added, SnapshotChangeFilter::Removed => stats.removed,
            SnapshotChangeFilter::OnlyBefore => stats.only_before, SnapshotChangeFilter::OnlyAfter => stats.only_after,
            SnapshotChangeFilter::Unavailable => stats.unavailable,
        };
        if !(1..=MAX_SNAPSHOT_COMPARISON_PAGE).contains(&limit) || start > total {
            return Err(SnapshotComparisonError::InvalidLimits);
        }
        out.literal(",\"comparison_generation\":")?; out.integer(state.generation.get())?;
        out.literal(",\"comparison_in_progress\":")?; out.boolean(!state.cursor.finished())?;
        out.literal(",\"scan_complete\":")?; out.boolean(state.cursor.finished())?;
        out.literal(",\"comparison_complete\":")?; out.boolean(state.complete())?;
        out.literal(",\"policies_match\":")?; out.boolean(state.cursor.policies_match())?;
        out.literal(",\"membership_comparable\":")?; out.boolean(state.cursor.membership_comparable())?;
        for (key, value) in [
            ("paths_upper_bound", state.capacity), ("compared_paths", stats.compared_paths),
            ("unchanged", stats.unchanged), ("changed", stats.changed), ("added", stats.added),
            ("removed", stats.removed), ("only_before", stats.only_before), ("only_after", stats.only_after),
            ("unavailable", stats.unavailable), ("known_differences", stats.known_differences()),
            ("uncertain_paths", stats.uncertain_paths()), ("retained_entries", state.entries.len()),
            ("last_step_entries", state.last_step_entries), ("max_step_entries", MAX_SNAPSHOT_COMPARISON_STEP),
        ] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
        }
        out.literal(",\"steps\":")?; out.integer(state.steps)?;
        out.literal(",\"filter\":")?; out.quoted(filter.name())?;
        out.literal(",\"offset\":")?; out.integer(start as u64)?;
        out.literal(",\"limit\":")?; out.integer(limit as u64)?;
        out.literal(",\"matching_entries\":")?; out.integer(total as u64)?;
        out.literal(",\"entries\":[")?;
        let desired = limit.min(total - start);
        let (mut skipped, mut emitted) = (0usize, 0usize);
        for (index, change) in state.entries.iter().enumerate() {
            if emitted == desired { break; }
            check(canceled)?;
            if !filter.includes(change.kind()) { continue; }
            if skipped < start { skipped += 1; continue; }
            let checkpoint = out.checkpoint();
            let result = (|| -> Result<(), SnapshotComparisonError> {
                if emitted != 0 { out.literal(",")?; }
                row(out, index as u64 + 1, *change, self.before.directory(), self.after.directory(), state.generation)
            })();
            match result {
                Ok(()) if out.remaining_bytes() >= 256 => emitted += 1,
                Ok(()) | Err(SnapshotComparisonError::App(AppError::Output(OutputError::Limit))) => {
                    out.restore(checkpoint)?;
                    // Every admitted single path/member descriptor fits with
                    // the bounded header. Never return a nonadvancing page.
                    if emitted == 0 { return Err(OutputError::Limit.into()); }
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        let end = start + emitted;
        out.literal("],\"next_offset\":")?;
        optional(out, (end < total).then_some(end as u64))?;
        out.literal(",\"page_limited\":")?; out.boolean(emitted < desired)?;
        Ok(())
    }
}

fn archive(out: &mut Output, archive: &PagedSnapshot<File>, path: &Path) -> Result<(), OutputError> {
    let directory = archive.directory();
    out.literal("{\"digest\":")?; out.quoted(&directory.digest().to_hex())?;
    out.literal(",\"path\":")?; out.path(path)?;
    out.literal(",\"policy\":")?; out.quoted(directory.policy())?;
    out.literal(",\"discovery_complete\":")?; out.boolean(directory.discovery_complete())?;
    for (key, value) in [
        ("members_total", directory.len() as u64), ("captured_members", directory.captured_files() as u64),
        ("source_bytes", directory.source_bytes()), ("archive_bytes", directory.archive_bytes()),
        ("initial_archive_bytes_read", directory.validation_stats().bytes_read),
        ("initial_archive_read_calls", directory.validation_stats().read_calls),
        ("member_bytes_read", archive.load_stats().bytes_read), ("member_read_calls", archive.load_stats().read_calls),
    ] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal("}")
}
fn row(out: &mut Output, id: u64, change: SnapshotChange, before: &SnapshotDirectory,
    after: &SnapshotDirectory, generation: QueryGeneration) -> Result<(), SnapshotComparisonError> {
    change.validate(before, after, generation)?;
    out.literal("{\"entry_id\":")?; out.integer(id)?;
    out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(change.path(before, after)?).to_path_buf())?;
    out.literal(",\"kind\":")?; out.quoted(change.kind().name())?;
    out.literal(",\"before\":")?;
    member(out, change.before_ordinal().and_then(|ordinal| before.member(ordinal)), SnapshotComparisonSide::Before)?;
    out.literal(",\"after\":")?;
    member(out, change.after_ordinal().and_then(|ordinal| after.member(ordinal)), SnapshotComparisonSide::After)?;
    out.literal("}")?;
    Ok(())
}
fn member(out: &mut Output, value: Option<PagedMember<'_>>, side: SnapshotComparisonSide)
    -> Result<(), SnapshotComparisonError> {
    let Some(value) = value else { out.literal("null")?; return Ok(()); };
    let identity = member_identity(side, value.ordinal)?;
    out.literal("{\"member\":")?; out.integer(value.ordinal as u64)?;
    out.literal(",\"file_id\":")?; out.integer(identity)?;
    out.literal(",\"source_revision\":")?; out.integer(identity)?;
    out.literal(",\"observed_bytes\":")?; out.integer(value.observed_bytes)?;
    match value.data {
        PagedMemberData::Captured { byte_length, digest, .. } => {
            out.literal(",\"captured\":true,\"captured_bytes\":")?; out.integer(byte_length as u64)?;
            out.literal(",\"source_digest\":")?; out.quoted(&digest.to_hex())?;
            out.literal(",\"unavailable_reason\":null")?;
        }
        PagedMemberData::Unavailable(reason) => {
            out.literal(",\"captured\":false,\"captured_bytes\":null,\"source_digest\":null,\"unavailable_reason\":")?;
            out.quoted(reason)?;
        }
    }
    out.literal("}")?;
    Ok(())
}
fn optional(out: &mut Output, value: Option<u64>) -> Result<(), OutputError> {
    match value { Some(value) => out.integer(value), None => out.literal("null") }
}
