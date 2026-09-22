#![forbid(unsafe_code)]

//! One-file capture AND segment construction for a replacement repository index.
//! Source policy and trigram construction stay in the existing production
//! engines. The last accepted index remains queryable between preparation steps.

use std::path::PathBuf;
use fcb::search::index::export::OwnedIndexBuilder;
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasIndexBuildProgress {
    pub generation: u64,
    pub in_progress: bool,
    pub stop_reason: Option<AtlasSearchStop>,
    pub captured_files: usize,
    pub unavailable_files: usize,
    pub examined_files: usize,
    pub pending_files: usize,
    pub indexed_files: usize,
    pub uncovered_files: usize,
    pub retained_source_bytes: u64,
    pub source_bytes_read: u64,
    pub read_calls: u64,
    pub steps: u64,
    pub last_step_files: usize,
    pub last_step_source_bytes: u64,
    pub last_step_read_calls: u64,
}

pub(crate) struct IndexBuildWork {
    generation: u64,
    builder: OwnedIndexBuilder,
    options: AtlasIndexOptions,
    root: PathBuf,
    diagnostics: Vec<Unavailable>,
    examined: usize,
    io: workspace::IoCounts,
    scan_id: ResourceAllocationId,
    steps: u64,
    last_files: usize,
    last_bytes: u64,
    last_calls: u64,
    _lease: ResourceLease,
}
impl RetainedAtlasSearch {
    pub fn pending_index_generation(&self) -> Option<u64> { self.building.as_ref().map(|b| b.generation) }

    /// Explicit worker-side retirement. Queries/results and the previously
    /// accepted index remain intact. Atlas epoch cancellation calls this too.
    pub fn cancel_index_build(&mut self) -> bool { self.building.take().is_some() }

    /// Admit one private replacement without opening a source path or building
    /// a segment. Queries may use the old accepted index between build steps.
    /// Preparation generations share the non-reusing search-attempt sequence.
    pub fn begin_index(&mut self, atlas: &AtlasSession, generation: u64,
        options: AtlasIndexOptions, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let work = self.prepare_index_build(atlas, generation, options, &mut canceled)?;
        let progress = work.progress(atlas);
        let response = self.encode_build_progress(atlas, progress, "index-begin", &mut canceled)?;
        self.building = Some(work);
        Ok(response)
    }

    /// Capture and build at most one catalog member, then yield control. The
    /// final call moves prepared storage; it never scans/rebuilds the universe.
    /// Nonterminal lost replies reconcile through index_build_info. Repeating a
    /// completed step is read-only; stale calls never remove a newer build.
    pub fn step_index(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        if self.pending_index_generation() != Some(generation) {
            if self.index_generation() == Some(generation) {
                return self.index_build_info(atlas, generation, canceled);
            }
            return Err(AtlasSearchError::StaleIndex);
        }
        // An error or panic discards only this private candidate. The old index
        // and completed query rows remain admitted and usable.
        let mut work = self.building.take().ok_or(AtlasSearchError::MissingIndex)?;
        if let Some(stop) = self.advance_index_build(atlas, &mut work, &mut canceled)? {
            self.publish_index_build(atlas, work, stop, "index-step", &mut canceled)
        } else {
            let response = self.encode_build_progress(atlas, work.progress(atlas), "index-step", &mut canceled)?;
            self.building = Some(work);
            Ok(response)
        }
    }
    pub fn index_build_progress(&self, atlas: &AtlasSession, generation: u64)
        -> Result<AtlasIndexBuildProgress, AtlasSearchError> {
        self.validate(atlas)?;
        if let Some(work) = &self.building {
            if work.generation == generation { return Ok(work.progress(atlas)); }
        }
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if index.generation != generation { return Err(AtlasSearchError::StaleIndex); }
        let stats = index.engine.statistics();
        Ok(AtlasIndexBuildProgress { generation, in_progress: false, stop_reason: Some(index.stop),
            captured_files: index.engine.captured_files(), unavailable_files: index.engine.unavailable_files().len(),
            examined_files: index.examined, pending_files: index.pending,
            indexed_files: stats.indexed_files, uncovered_files: stats.uncovered_files,
            retained_source_bytes: index.engine.source_bytes(), source_bytes_read: index.read_bytes,
            read_calls: index.read_calls, steps: index.build_steps, last_step_files: index.last_build_files,
            last_step_source_bytes: index.last_build_bytes, last_step_read_calls: index.last_build_calls })
    }
    pub fn index_build_info(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let progress = self.index_build_progress(atlas, generation)?;
        self.encode_build_progress(atlas, progress, "index-progress", &mut canceled)
    }

    pub(super) fn prepare_index_build(&mut self, atlas: &AtlasSession, generation: u64,
        options: AtlasIndexOptions, canceled: &mut impl FnMut() -> bool)
        -> Result<IndexBuildWork, AtlasSearchError> {
        self.validate(atlas)?; self.attempt(generation)?;
        self.building = None; // New valid attempt supersedes the older builder, even if admission fails.
        options.validate()?; check(canceled)?;
        let manifest_revision = generation.checked_add(self.manifest.revision())
            .ok_or(AtlasSearchError::IdentityExhausted)?;
        let manifest = SearchManifestId::new(self.manifest.owner(), manifest_revision)?;
        let [work_id, source_id, index_id, scan_id] =
            [self.next_id()?, self.next_id()?, self.next_id()?, self.next_id()?];
        let root_bytes = atlas.atlas().catalog().grant().root_path().len();
        let bytes = 2 * options.max_file_bytes + 256 * 1024 + 2 * root_bytes
            + MAX_DIAGNOSTICS * size_of::<Unavailable>() + size_of::<IndexBuildWork>() + size_of::<RetainedIndex>();
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), work_id, ByteLength::new(bytes as u64))
            .map_err(|_| AppError::Admission)?;
        let capacity = options.max_files.min(atlas.atlas().file_count());
        let builder = OwnedIndexBuilder::new(manifest, SourceRetentionLimits {
            max_files: capacity, max_source_bytes: options.max_source_bytes as u64, max_path_bytes: 0,
        }, IndexLimits { max_source_bytes_per_file: options.max_file_bytes,
            max_source_bytes_total: options.max_source_bytes as u64, max_total_grams: options.max_index_grams,
            max_scratch_bytes: 4 * 1024 * 1024, ..Default::default() },
            &self.budget, source_id, index_id, &mut *canceled)?;
        let work = IndexBuildWork { generation, builder, options,
            root: atlas.atlas().catalog().grant().root_path().to_path_buf(),
            diagnostics: reserve(MAX_DIAGNOSTICS)?, examined: 0, io: workspace::IoCounts::default(), scan_id,
            steps: 0, last_files: 0, last_bytes: 0, last_calls: 0, _lease: lease };
        self.validate(atlas)?; check(canceled)?;
        Ok(work)
    }
    pub(super) fn advance_index_build(&self, atlas: &AtlasSession, work: &mut IndexBuildWork,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<AtlasSearchStop>, AtlasSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let before_files = work.examined;
        let before_bytes = work.io.bytes;
        let before_calls = work.io.calls;
        if work.stop(atlas).is_none() {
            let catalog = atlas.atlas().catalog();
            let ordinal = work.examined;
            let entry = catalog.entries().get(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
            let file = catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
            let revision = SourceRevision::new(self.manifest.owner(), work.generation)
                .map_err(|_| AtlasSearchError::IdentityExhausted)?;
            let captured = if entry.observed_bytes() > work.options.max_file_bytes as u64 {
                Err(fcb::source::SourceError::PayloadTooLarge)
            } else {
                let request = CaptureRequest::new(file, revision).map_err(|_| AtlasSearchError::WrongAtlas)?;
                workspace::read_capture(&work.root, request, entry.path(), work.options.max_file_bytes,
                    work.options.max_source_bytes as u64, &mut work.io, &self.budget,
                    &mut || canceled() || atlas.validate_active().is_err())
            };
            self.validate(atlas)?; check(canceled)?;
            match captured {
                Ok(capture) => work.builder.push_capture(&capture, "", &self.budget, work.scan_id,
                    || canceled() || atlas.validate_active().is_err())?,
                Err(fcb::source::SourceError::Canceled) => return Err(AtlasSearchError::Canceled),
                Err(error) => {
                    work.builder.push_unavailable(file, &mut *canceled)?;
                    if work.diagnostics.len() < MAX_DIAGNOSTICS {
                        work.diagnostics.push(Unavailable { file, reason: error.code() });
                    }
                }
            }
            work.examined += 1;
        }
        self.validate(atlas)?; check(canceled)?;
        work.steps += 1; // At most max_files, or one empty finalization step.
        work.last_files = work.examined - before_files;
        work.last_bytes = work.io.bytes - before_bytes;
        work.last_calls = work.io.calls - before_calls;
        Ok(work.stop(atlas))
    }
    pub(super) fn publish_index_build(&mut self, atlas: &AtlasSession, work: IndexBuildWork,
        stop: AtlasSearchStop, command: &str, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSearchError> {
        let IndexBuildWork { generation, builder, diagnostics, examined, io, root,
            steps, last_files, last_bytes, last_calls, _lease: lease, .. } = work;
        let pending = atlas.atlas().file_count().checked_sub(examined).ok_or(AtlasSearchError::WrongAtlas)?;
        let membership = if pending == 0 && atlas.atlas().discovery_complete() {
            MembershipState::Closed
        } else { MembershipState::Discovering };
        let engine = builder.finish(membership, &mut *canceled)?;
        drop(root);
        lease.reconcile(ByteLength::new((size_of::<RetainedIndex>()
            + diagnostics.capacity() * size_of::<Unavailable>()) as u64)).map_err(|_| AppError::Admission)?;
        let candidate = RetainedIndex { generation, engine, diagnostics, examined, pending,
            read_bytes: io.bytes, read_calls: io.calls, stop, build_steps: steps,
            last_build_files: last_files, last_build_bytes: last_bytes, last_build_calls: last_calls, _lease: lease };
        let mut out = self.output(atlas, command)?;
        encode_index(&mut out, atlas, &candidate)?;
        let partial = !capture_complete(atlas, &candidate) || candidate.engine.statistics().uncovered_files != 0;
        let response = self.finish(atlas, out, partial, canceled)?;
        // A query started on the old index while this builder was paused cannot
        // resume on its replacement. Completed rows/reader pins remain intact;
        // a pending live query is independent and can keep running.
        if self.pending.as_ref().is_some_and(|work| matches!(&work.execution, SearchExecution::Indexed(_))) {
            self.pending = None;
        }
        self.index = Some(candidate);
        Ok(response)
    }
    fn encode_build_progress(&mut self, atlas: &AtlasSession, p: AtlasIndexBuildProgress,
        command: &str, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        check(canceled)?;
        let mut out = self.output(atlas, command)?;
        out.literal(",\"index_generation\":")?; out.integer(p.generation)?;
        out.literal(",\"accepted_index_generation\":")?;
        if let Some(generation) = self.index_generation() { out.integer(generation)?; } else { out.literal("null")?; }
        out.literal(",\"index_build_in_progress\":")?; out.boolean(p.in_progress)?;
        out.literal(",\"capture_complete\":")?;
        out.boolean(!p.in_progress && p.pending_files == 0 && p.unavailable_files == 0 && atlas.atlas().discovery_complete())?;
        out.literal(",\"stop_reason\":")?;
        if let Some(reason) = p.stop_reason { out.quoted(reason.code())?; } else { out.literal("null")?; }
        for (key, value) in [("captured_files", p.captured_files as u64), ("unavailable_files", p.unavailable_files as u64),
            ("examined_files", p.examined_files as u64), ("pending_files", p.pending_files as u64),
            ("indexed_files", p.indexed_files as u64), ("uncovered_files", p.uncovered_files as u64),
            ("indexed_source_bytes", p.retained_source_bytes), ("initial_source_bytes_read", p.source_bytes_read),
            ("initial_read_calls", p.read_calls), ("build_steps", p.steps), ("last_step_files", p.last_step_files as u64),
            ("last_step_source_bytes", p.last_step_source_bytes), ("last_step_read_calls", p.last_step_read_calls)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal("}\n")?;
        let partial = p.in_progress || p.pending_files != 0 || p.unavailable_files != 0 || p.uncovered_files != 0
            || !atlas.atlas().discovery_complete();
        self.finish(atlas, out, partial, canceled)
    }
}
impl IndexBuildWork {
    fn stop(&self, atlas: &AtlasSession) -> Option<AtlasSearchStop> {
        if self.examined == atlas.atlas().file_count() { Some(AtlasSearchStop::AllFilesExamined) }
        else if self.examined == self.options.max_files { Some(AtlasSearchStop::FileLimit) }
        else if self.io.bytes >= self.options.max_source_bytes as u64 { Some(AtlasSearchStop::SourceByteLimit) }
        else { None }
    }
    fn progress(&self, atlas: &AtlasSession) -> AtlasIndexBuildProgress {
        let stats = self.builder.statistics();
        AtlasIndexBuildProgress { generation: self.generation, in_progress: true, stop_reason: None,
            captured_files: self.builder.captured_files(), unavailable_files: self.builder.unavailable_files(),
            examined_files: self.examined, pending_files: atlas.atlas().file_count() - self.examined,
            indexed_files: stats.indexed_files, uncovered_files: stats.uncovered_files,
            retained_source_bytes: self.builder.source_bytes(), source_bytes_read: self.io.bytes,
            read_calls: self.io.calls, steps: self.steps, last_step_files: self.last_files,
            last_step_source_bytes: self.last_bytes, last_step_read_calls: self.last_calls }
    }
}
