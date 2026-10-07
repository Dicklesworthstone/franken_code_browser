#![forbid(unsafe_code)]

//! Independently owned readers from the immutable indexed source universe.
//! This is source transfer, not search-hit authority: no query/range is invented.
//! The native host can re-establish an old occurrence in the reader's namespace
//! even after another query has replaced the index's current result buffer.
use super::*;

impl RetainedAtlasSearch {
    /// Copy one explicitly named indexed capture to an independent reader.
    /// Queries/clears of query results do not change this source identity. A
    /// replaced/cleared index, foreign atlas, or mismatching source revision is
    /// refused. The path is only the captured catalog label; no file is opened.
    /// All work, including hashing and source retirement, belongs on a worker.
    pub fn open_index_reader(&mut self, atlas: &AtlasSession, reader_owner: ArenaOwnerId,
        index_generation: u64, file: FileId, revision: SourceRevision,
        mut canceled: impl FnMut() -> bool) -> Result<(ReaderSession, HostResponse), AtlasSearchError> {
        self.validate(atlas)?;
        check(&mut canceled)?;
        if reader_owner == self.manifest.owner() || file.owner() != self.manifest.owner() {
            return Err(AtlasSearchError::WrongAtlas);
        }
        let mut out = self.output(atlas, "index-open-reader")?;
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if index.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        let source = index.engine.capture(file).ok_or(AtlasSearchError::MissingHit)?;
        if source.request().revision() != revision { return Err(AtlasSearchError::StaleIndex); }
        let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
        let label = entry.path().raw().to_path_buf();
        let mut stop = || canceled() || atlas.validate_active().is_err();
        let witness = CaptureWitness::new(source.bytes(), &mut stop)?;
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, source.bytes(), &mut stop)?;
        let info = reader.info(&mut stop)?;
        out.literal(",\"selection_namespace\":\"index-source\",\"index_generation\":")?;
        out.integer(index.generation)?;
        out.literal(",\"capture_manifest\":")?; out.integer(index.engine.id().revision())?;
        out.literal(",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(revision.get())?;
        out.literal(",\"path\":")?; out.path(&label)?;
        witness.encode(&mut out)?;
        out.literal(",\"source_observation\":\"retained-index-capture\",\"source_reopened\":false,\"reader_owner\":")?;
        out.integer(reader_owner.get())?;
        out.literal(",\"reader\":")?; out.literal(info.as_str())?;
        out.literal("}\n")?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        Ok((reader, response))
    }
}

#[cfg(all(test, unix))]
#[path = "index_source_tests.rs"]
mod tests;
