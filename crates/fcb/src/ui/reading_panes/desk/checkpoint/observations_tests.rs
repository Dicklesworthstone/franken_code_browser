use super::*;
use crate::store::envelope::{Sha256, HEADER_LEN};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn alloc(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn range(a: u64, b: u64) -> ByteRange { ByteRange::new(ByteOffset::new(a), ByteOffset::new(b)).unwrap() }
fn desk(n: u64) -> ReadingDesk {
    let budget = ResourceBudget::new(owner(n), ByteLength::new(256 * 1024 * 1024)).unwrap();
    ReadingDesk::new(owner(n), DeskLimits::default(), &budget, alloc(1)).unwrap()
}
fn observed(base: u64, requested_end: u64, observed_length: u64, final_length: Option<u64>,
    consistency: ExtentConsistency, bytes: &[u8]) -> ObservedExtent {
    let origin = owner(u64::MAX - 10);
    let request = CaptureRequest::new(FileId::new(origin, u64::MAX - 20).unwrap(),
        SourceRevision::new(origin, u64::MAX - 30).unwrap()).unwrap()
        .with_range(range(base, requested_end)).unwrap();
    let budget = ResourceBudget::new(origin, ByteLength::new(4 * 1024 * 1024)).unwrap();
    let lease = budget.try_reserve_managed(origin, alloc(1), ByteLength::new(bytes.len() as u64 + 4096)).unwrap();
    ObservedExtent::restore_recorded(request, range(base, base + bytes.len() as u64), ByteLength::new(observed_length),
        final_length.map(ByteLength::new), consistency, bytes, lease).unwrap()
}
fn import(desk: &mut ReadingDesk, extent: &ObservedExtent, encoding: DetectedEncoding, selection: ByteRange)
    -> DeskObservationChange {
    let attempt = desk.last_attempt() + 1;
    desk.import_observation(desk.revision(), attempt, extent, b"original/\xff\0.rs", encoding, selection,
        100, alloc(1000 + attempt), || false).unwrap()
}

#[test]
fn checkpoint_roundtrip_preserves_utf8_bom_utf16_endianness_scalars_and_personal_text() {
    let text = "a😀\r\nz\u{feff}é";
    for encoding in [DetectedEncoding::Utf8 { has_bom: true }, DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
        let bytes = match encoding {
            DetectedEncoding::Utf8 { .. } => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
            DetectedEncoding::Utf16Le => std::iter::once(0xfeffu16).chain(text.encode_utf16())
                .flat_map(u16::to_le_bytes).collect(),
            DetectedEncoding::Utf16Be => std::iter::once(0xfeffu16).chain(text.encode_utf16())
                .flat_map(u16::to_be_bytes).collect(),
            _ => unreachable!(),
        };
        let length = bytes.len() as u64;
        let extent = observed(0, length, length, Some(length), ExtentConsistency::HostSupplied, &bytes);
        let selection = range(encoding.bom_bytes_len(), length);
        let mut original = desk(700);
        let imported = import(&mut original, &extent, encoding, selection);
        original.edit_observed_passage(1, 2, imported.passage, 1, "Question\0", "Keep\r\nmy words 😀", || false).unwrap();
        let saved = original.checkpoint(2, alloc(2000), || false).unwrap();
        assert_eq!(&saved.bytes()[10..12], &2u16.to_le_bytes());
        assert_eq!(saved.source_bytes(), length);
        let mut restored = desk(701);
        let receipt = restored.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || false).unwrap();
        assert!(restored.panes().is_empty()); assert!(restored.bookmarks().is_empty());
        let passage = &restored.observed_passages()[0];
        assert_eq!(passage.selection(), selection); assert_eq!(passage.update_sequence(), 2);
        assert_eq!(passage.label(), "Question\0"); assert_eq!(passage.body(), "Keep\r\nmy words 😀");
        let observation = restored.observation(1, passage.observation()).unwrap();
        assert_eq!(observation.path_bytes(), b"original/\xff\0.rs");
        assert_eq!(observation.encoding(), encoding);
        assert_eq!(observation.extent().bytes(), bytes);
        assert_eq!(observation.extent_sha256(), Sha256::digest(&bytes));
        assert_eq!(observation.origin_owner().get(), u64::MAX - 10);
        assert_eq!(observation.origin_file(), u64::MAX - 20);
        assert_eq!(observation.origin_observation(), u64::MAX - 30);
        assert_eq!(observation.extent().request().file().owner(), owner(701));
        assert!(receipt.next_source_identity > observation.extent().request().revision().get());
        let decoded = restored.read_observed_passage(1, passage.id(), alloc(3), || false).unwrap();
        assert_eq!(decoded.text(), text); assert_eq!(decoded.range(), selection);
        assert_eq!(decoded.extent().range_bytes(selection).unwrap(), &bytes[encoding.bom_bytes_len() as usize..]);
        assert!(!decoded.has_replacements());
        integrity::verify_checkpoint_integrity(&mut std::io::Cursor::new(saved.bytes()), saved.bytes().len() as u64,
            saved.digest(), || false).unwrap();
    }
}

#[test]
fn partial_changed_and_short_observations_keep_full_width_offsets_and_do_not_gain_eof_context() {
    let base = (1u64 << 40) + 8;
    for consistency in [ExtentConsistency::ChangedDuringRead, ExtentConsistency::ShortRead, ExtentConsistency::MetadataUnavailable] {
        let bytes = b"prefix__A\xffB__suffix__end";
        let length = bytes.len() as u64;
        let requested_end = base + length + if consistency == ExtentConsistency::ShortRead { 8 } else { 0 };
        let initial_length = if consistency == ExtentConsistency::ShortRead { requested_end + 40 } else { base + length };
        let final_length = match consistency {
            ExtentConsistency::MetadataUnavailable => None,
            ExtentConsistency::ChangedDuringRead => Some(base + length + 40),
            _ => Some(base + length),
        };
        let extent = observed(base, requested_end, initial_length, final_length, consistency, bytes);
        let mut original = desk(1);
        import(&mut original, &extent, DetectedEncoding::Utf8 { has_bom: false }, range(base + 8, base + 11));
        let saved = original.checkpoint(1, alloc(2000), || false).unwrap();
        let mut restored = desk(2);
        restored.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || false).unwrap();
        let passage = &restored.observed_passages()[0];
        let observation = restored.observation(1, passage.observation()).unwrap();
        assert_eq!(observation.extent().request().range(), extent.request().range());
        assert_eq!(observation.extent().range(), extent.range());
        assert_eq!(observation.extent().observed_length(), extent.observed_length());
        assert_eq!(observation.extent().final_length(), extent.final_length());
        assert_eq!(observation.extent().consistency(), consistency);
        assert_eq!(observation.extent().request_filled(), consistency != ExtentConsistency::ShortRead);
        let read = restored.read_observed_passage(1, passage.id(), alloc(3), || false).unwrap();
        assert_eq!(read.text(), "A\u{fffd}B"); assert!(read.has_replacements());
        assert_eq!(read.range(), range(base + 8, base + 11));
        let view = BrowserSession::new(restored.owner()).open_extent(observation.extent().clone()).unwrap();
        assert!(matches!(view.decode(range(base + length - 4, base + length), observation.encoding(),
            fcb_core::QueryGeneration::new(restored.owner(), 1).unwrap(), &restored.budget, alloc(4), || false),
            Err(crate::search::extents::ExtentViewError::ContextUnavailable)));
    }
}

#[test]
fn same_owner_restore_rebases_passages_and_observations_without_losing_shared_bytes_or_old_views() {
    let bytes = b"prefix__needle__suffix";
    let extent = observed(0, bytes.len() as u64, bytes.len() as u64, Some(bytes.len() as u64),
        ExtentConsistency::HostSupplied, bytes);
    let mut original = desk(1);
    let first = import(&mut original, &extent, DetectedEncoding::Utf8 { has_bom: false }, range(8, 14));
    import(&mut original, &extent, DetectedEncoding::Utf8 { has_bom: false }, range(8, 11));
    let old_view = original.read_observed_passage(2, first.passage, alloc(3000), || false).unwrap();
    let old_actual = old_view.extent().request().revision();
    let saved = original.checkpoint(2, alloc(2000), || false).unwrap();
    original.restore_checkpoint(2, 3, saved.bytes(), alloc(2001), || false).unwrap();
    assert_eq!(original.retained_source_count(), 1); assert_eq!(original.observed_passages().len(), 2);
    let a = &original.observed_passages()[0]; let b = &original.observed_passages()[1];
    assert_eq!(a.observation(), b.observation()); assert_ne!(a.id(), first.passage);
    assert_ne!(a.observation(), first.observation);
    assert!(original.observation(3, a.observation()).unwrap().extent().request().revision().get() > old_actual.get());
    assert_eq!(old_view.text(), "needle"); assert_eq!(old_view.extent().request().revision(), old_actual);
    assert!(original.observed_passage(3, first.passage).is_err());
}

#[test]
fn restored_origin_ordinals_are_only_provenance_when_a_later_process_reuses_them() {
    let old = b"prefix__needle__suffix";
    let old_extent = observed(0, old.len() as u64, old.len() as u64, Some(old.len() as u64),
        ExtentConsistency::HostSupplied, old);
    let mut original = desk(1);
    import(&mut original, &old_extent, DetectedEncoding::Utf8 { has_bom: false }, range(8, 14));
    let checkpoint = original.checkpoint(1, alloc(2000), || false).unwrap();
    let mut restored = desk(2);
    restored.restore_checkpoint(0, 1, checkpoint.bytes(), alloc(2), || false).unwrap();
    let old_observation = restored.observed_passages()[0].observation();
    // The helper deliberately reuses every origin ordinal while representing
    // another host lifetime, different source bytes and a different raw path.
    let changed = b"prefix__REVIEW__suffix";
    let current = observed(0, changed.len() as u64, changed.len() as u64, Some(changed.len() as u64),
        ExtentConsistency::HostSupplied, changed);
    let added = restored.import_observation(1, 2, &current, b"different.rs", DetectedEncoding::Utf8 { has_bom: false },
        range(8, 14), 100, alloc(3), || false).unwrap();
    assert_ne!(old_observation, added.observation);
    let repeated = restored.import_observation(2, 3, &current, b"different.rs", DetectedEncoding::Utf8 { has_bom: false },
        range(8, 11), added.next_source_identity, alloc(4), || false).unwrap();
    assert_eq!(repeated.observation, added.observation);
    assert_eq!(restored.retained_source_count(), 2);
    assert_eq!(restored.observed_passages().len(), 3);
    let second = restored.checkpoint(3, alloc(5), || false).unwrap();
    let mut reopened = desk(3);
    reopened.restore_checkpoint(0, 1, second.bytes(), alloc(2), || false).unwrap();
    assert_eq!(reopened.retained_source_count(), 2);
    let passages = reopened.observed_passages();
    assert_eq!(reopened.read_observed_passage(1, passages[0].id(), alloc(3), || false).unwrap().text(), "needle");
    assert_eq!(reopened.read_observed_passage(1, passages[1].id(), alloc(4), || false).unwrap().text(), "REVIEW");
}

// Independent positional fixture: one observation and one personal passage,
// no complete sources, panes or history. The envelope checksum is valid even
// when semantic metadata below is intentionally hostile.
fn wire(selection: ByteRange, consistency: u8, filled: u8, digest: &[u8], index: u64, sequence: u64) -> Vec<u8> {
    let mut out = EnvelopeWriter::new(OBSERVATION_SCHEMA);
    for n in [0, 0, 0, 0, NONE, 0, 1, 1] { out.put_u64(n); }
    for n in [17, 18, 19] { out.put_u64(n); }
    out.put_bytes(b"path\xff"); out.put_u8(0);
    for n in [0, 4, 0, 4, 4] { out.put_u64(n); }
    out.put_u8(1); out.put_u64(4); out.put_u8(consistency); out.put_u8(filled);
    out.put_bytes(digest); out.put_bytes(b"abcd");
    out.put_str("Keep"); out.put_str("My note"); out.put_u64(sequence);
    out.put_u64(index); out.put_u64(selection.start().get()); out.put_u64(selection.end().get());
    out.finish()
}

#[test]
fn independent_wire_reads_and_semantic_corruption_never_replace_existing_personal_state() {
    let digest = Sha256::digest(b"abcd");
    let valid = wire(range(0, 4), 0, 1, digest.as_bytes(), 0, 1);
    let mut restored = desk(1);
    restored.restore_checkpoint(0, 1, &valid, alloc(2), || false).unwrap();
    let id = restored.observed_passages()[0].id();
    assert_eq!(restored.read_observed_passage(1, id, alloc(3), || false).unwrap().text(), "abcd");
    let variants = [
        wire(range(0, 5), 0, 1, digest.as_bytes(), 0, 1),
        wire(range(0, 0), 0, 1, digest.as_bytes(), 0, 1),
        wire(range(0, 4), 3, 1, digest.as_bytes(), 0, 1),
        wire(range(0, 4), 0, 0, digest.as_bytes(), 0, 1),
        wire(range(0, 4), 0, 1, &[0; 32], 0, 1),
        wire(range(0, 4), 0, 1, digest.as_bytes(), 1, 1),
        wire(range(0, 4), 0, 1, digest.as_bytes(), 0, 0),
    ];
    for (i, bytes) in variants.iter().enumerate() {
        assert!(restored.restore_checkpoint(1, i as u64 + 2, bytes, alloc(100 + i as u64), || false).is_err());
        assert_eq!(restored.revision(), 1); assert_eq!(restored.observed_passages()[0].id(), id);
        assert_eq!(restored.observed_passages()[0].body(), "My note");
    }
    let mut unknown = valid.clone(); unknown[10] = 3;
    let end = unknown.len() - 32;
    let hash = Sha256::digest(&unknown[..end]); unknown[end..].copy_from_slice(hash.as_bytes());
    assert_eq!(restored.restore_checkpoint(1, 20, &unknown, alloc(200), || false), Err(CheckpointError::UnsupportedVersion));
    let mut excessive = valid;
    excessive[HEADER_LEN + 48..HEADER_LEN + 56].copy_from_slice(&u64::MAX.to_le_bytes());
    let end = excessive.len() - 32;
    let hash = Sha256::digest(&excessive[..end]); excessive[end..].copy_from_slice(hash.as_bytes());
    assert_eq!(restored.restore_checkpoint(1, 21, &excessive, alloc(201), || false), Err(CheckpointError::Limit));
    assert_eq!(restored.observed_passages()[0].body(), "My note");
}

#[test]
fn canceled_restore_and_legacy_replacement_preserve_or_deliberately_replace_passages() {
    let digest = Sha256::digest(b"abcd");
    let valid = wire(range(0, 4), 0, 1, digest.as_bytes(), 0, 1);
    let mut probe = desk(1); let mut checks = 0;
    probe.restore_checkpoint(0, 1, &valid, alloc(2), || { checks += 1; false }).unwrap();
    let mut restored = desk(2);
    restored.restore_checkpoint(0, 1, &valid, alloc(2), || false).unwrap();
    let id = restored.observed_passages()[0].id();
    for stop in [1, 3, checks] {
        let mut count = 0;
        let attempt = restored.last_attempt() + 1;
        assert_eq!(restored.restore_checkpoint(1, attempt, &valid, alloc(100 + attempt),
            || { count += 1; count == stop }), Err(CheckpointError::Desk(DeskError::Canceled)));
        assert_eq!(restored.revision(), 1); assert_eq!(restored.observed_passages()[0].id(), id);
        assert_eq!(restored.observed_passages()[0].body(), "My note");
    }
    // Ordinary 1.1 remains the emitted format when no observed personal state
    // exists; explicit restore is an intentional whole-desk replacement.
    let empty = desk(3);
    let legacy = empty.checkpoint(0, alloc(2), || false).unwrap();
    assert_eq!(&legacy.bytes()[10..12], &1u16.to_le_bytes());
    restored.restore_checkpoint(1, restored.last_attempt() + 1, legacy.bytes(), alloc(300), || false).unwrap();
    assert!(restored.observed_passages().is_empty()); assert_eq!(restored.retained_source_count(), 0);
    let saved = restored.checkpoint(restored.revision(), alloc(301), || false).unwrap();
    assert_eq!(saved.bytes(), legacy.bytes());
}


#[test]
fn checkpoint_preserves_distinct_extents_from_one_origin_and_each_personal_note() {
    let source: Vec<_> = (0..512).map(|i| b'a' + (i % 26) as u8).collect();
    let first = observed(0, 240, 512, Some(512), ExtentConsistency::HostSupplied, &source[..240]);
    let second = observed(160, 400, 512, Some(512), ExtentConsistency::HostSupplied, &source[160..400]);
    let mut original = desk(900);
    let a = import(&mut original, &first, DetectedEncoding::Utf8 { has_bom: false }, range(16, 32));
    let b = import(&mut original, &second, DetectedEncoding::Utf8 { has_bom: false }, range(200, 232));
    assert_ne!(a.observation, b.observation);
    original.edit_observed_passage(2, 3, a.passage, 1, "First", "first\0note", || false).unwrap();
    original.edit_observed_passage(3, 4, b.passage, 1, "Second", "second\r\nnote", || false).unwrap();
    let saved = original.checkpoint(4, alloc(3000), || false).unwrap();
    assert_eq!(saved.source_bytes(), 480);
    let mut restored = desk(901);
    restored.restore_checkpoint(0, 1, saved.bytes(), alloc(2), || false).unwrap();
    assert_eq!(restored.retained_source_count(), 2);
    assert_eq!(restored.observed_passages().len(), 2);
    for (index, selected, captured, label, body) in [
        (0, range(16, 32), range(0, 240), "First", "first\0note"),
        (1, range(200, 232), range(160, 400), "Second", "second\r\nnote"),
    ] {
        let passage = &restored.observed_passages()[index];
        assert_eq!(passage.selection(), selected);
        assert_eq!(passage.label(), label); assert_eq!(passage.body(), body);
        assert_eq!(passage.update_sequence(), 2);
        let observation = restored.observation(1, passage.observation()).unwrap();
        assert_eq!(observation.extent().range(), captured);
        assert_eq!(observation.origin_owner().get(), u64::MAX - 10);
        assert_eq!(observation.origin_file(), u64::MAX - 20);
        assert_eq!(observation.origin_observation(), u64::MAX - 30);
        let text = restored.read_observed_passage(1, passage.id(), alloc(4000 + index as u64), || false).unwrap();
        assert_eq!(text.extent().range_bytes(selected).unwrap(),
            &source[selected.start().get() as usize..selected.end().get() as usize]);
    }
    let first_note = restored.observed_passages()[0].id();
    let second_note = restored.observed_passages()[1].id();
    restored.forget_observed_passage(1, 2, first_note, 2, || false).unwrap();
    assert_eq!(restored.retained_source_count(), 1);
    assert_eq!(restored.observed_passage(2, second_note).unwrap().body(), "second\r\nnote");
    let surviving = restored.checkpoint(2, alloc(5000), || false).unwrap();
    assert_eq!(surviving.source_bytes(), 240);
}
