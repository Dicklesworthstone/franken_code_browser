#![forbid(unsafe_code)]

//! Conservative admission for exact heading destinations. The pinned upstream
//! spanned parser pairs independently collected spans with AST blocks; valid
//! bounds alone do not establish correspondence. All syntax and AST semantics
//! below remain upstream-owned. This audit neither repairs nor guesses spans.

use franken_markdown::{Block, SpannedDocument};
use super::{ByteLength, DocumentReadError, DocumentReader, ResourceAllocationId, ResourceBudget, SourceSpan};

impl DocumentReader<'_> {
    /// Qualify the complete heading source catalog before exact navigation.
    /// Ordinary preview rendering remains available when this extra audit is
    /// unsupported. Callers must not turn an unqualified catalog into a claim
    /// that a requested heading is missing.
    ///
    /// Every top-level block must be the same upstream AST when parsed from its
    /// claimed original slice. Uncovered gaps must parse to no blocks. Checking
    /// only a heading's text/slice would accept identical text hidden in math,
    /// a lazy quote, or another block after an earlier span shifted the zip.
    pub fn qualify_heading_source_ranges(&self, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<(), DocumentReadError> {
        self.qualified_source_spans(budget, allocation, &mut canceled).map(|_| ())
    }

    /// The same conservative provenance audit also qualifies native display
    /// items. Their spans must name one of these exact verified top-level blocks;
    /// an arbitrary in-bounds slice is insufficient evidence for navigation.
    pub(crate) fn qualified_source_spans(&self, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<Vec<SourceSpan>, DocumentReadError> {
        check(&mut canceled)?;
        let source = self.session.source_text();
        let workspace = crate::links::parse_workspace(source.as_bytes()).map_err(|_| DocumentReadError::EngineBudget)?;
        // The retained reader coexists with a fresh full AST and one reparsed
        // slice. Disjoint slices/gaps cover at most source.len() bytes in total;
        // their maximum transient parser workspace is bounded by the full one.
        let charge = workspace.checked_mul(2).and_then(|n| n.checked_add(256 * 1024))
            .ok_or(DocumentReadError::EngineBudget)?;
        let _lease = budget.try_reserve_managed(self.capture.request().file().owner(), allocation,
            ByteLength::new(charge as u64)).map_err(|_| DocumentReadError::ResourceDenied)?;
        let parsed = parse(source, &mut canceled)?;
        if parsed.blocks.len() > self.options.max_blocks { return Err(DocumentReadError::EngineBudget); }
        if parsed.blocks.len() != self.output.consumed_blocks { return Err(DocumentReadError::InvalidOutput); }
        let mut previous_end = 0;
        let mut headings = 0;
        let mut nodes = 0;
        for (index, block) in parsed.blocks.iter().enumerate() {
            check(&mut canceled)?;
            let span = block.span;
            if span.start < previous_end || span.start >= span.end { return Err(DocumentReadError::InvalidOutput); }
            let gap = source.get(previous_end..span.start).ok_or(DocumentReadError::InvalidOutput)?;
            empty_gap(gap, &mut canceled)?;
            let slice = source.get(span.start..span.end).ok_or(DocumentReadError::InvalidOutput)?;
            // The current upstream catalog omits nested headings entirely.
            // Refuse incomplete catalogs rather than report those slugs missing.
            // Bound structural depth before invoking recursive AST equality.
            if nested_heading(&block.node, 0, &mut nodes)? { return Err(DocumentReadError::InvalidOutput); }
            let isolated = parse(slice, &mut canceled)?;
            if isolated.blocks.len() != 1 { return Err(DocumentReadError::InvalidOutput); }
            let mut slice_nodes = 0;
            if nested_heading(&isolated.blocks[0].node, 0, &mut slice_nodes)?
                || isolated.blocks[0].node != block.node { return Err(DocumentReadError::InvalidOutput); }
            if let Block::Heading { level, .. } = &block.node {
                let heading = self.headings().get(headings).ok_or(DocumentReadError::InvalidOutput)?;
                if heading.block_index != index || heading.level != *level || heading.source_span != span
                    || !heading.origin.is_primary() { return Err(DocumentReadError::InvalidOutput); }
                headings += 1;
            }
            previous_end = span.end;
        }
        empty_gap(source.get(previous_end..).ok_or(DocumentReadError::InvalidOutput)?, &mut canceled)?;
        if headings != self.headings().len() { return Err(DocumentReadError::InvalidOutput); }
        // Enclosing-block consumers must never fall back to an element outside
        // the verified block catalog. This does not certify glyph provenance.
        let elements = self.output.source_map.elements();
        let mut rendered_end = 0;
        for element in elements {
            check(&mut canceled)?;
            let block = parsed.blocks.get(element.block_index).ok_or(DocumentReadError::InvalidOutput)?;
            if !element.source_ranges.origin().is_primary() || element.rendered_range.start < rendered_end
                || self.rendered_text().get(element.rendered_range.start..element.rendered_range.end).is_none() {
                return Err(DocumentReadError::InvalidOutput);
            }
            rendered_end = element.rendered_range.end;
            for span in element.source_ranges.spans() {
                if span.start < block.span.start || span.end > block.span.end
                    || source.get(span.start..span.end).is_none() { return Err(DocumentReadError::InvalidOutput); }
            }
        }
        // Qualify the existing upstream flow association without claiming
        // inline/glyph accuracy: each row must name exactly its overlapping
        // rendered elements and their original enclosing range. This is a
        // monotonic metadata audit, not another layout or Markdown parser.
        let mut first = 0;
        for line in &self.output.lines {
            check(&mut canceled)?;
            while elements.get(first).is_some_and(|element| element.rendered_range.is_empty()
                || element.rendered_range.end <= line.rendered_range.start) { first += 1; }
            let mut seen = 0;
            let mut start = usize::MAX;
            let mut end = 0;
            for (index, element) in elements.iter().enumerate().skip(first) {
                if element.rendered_range.start >= line.rendered_range.end { break; }
                if !element.rendered_range.is_empty() && element.rendered_range.overlaps(line.rendered_range) {
                    if line.element_indices.get(seen) != Some(&index) { return Err(DocumentReadError::InvalidOutput); }
                    seen += 1;
                    for span in element.source_ranges.spans() { start = start.min(span.start); end = end.max(span.end); }
                }
            }
            let expected = if start <= end && end > 0 { SourceSpan { start, end } } else { SourceSpan::default() };
            if seen != line.element_indices.len() || line.source_span != expected { return Err(DocumentReadError::InvalidOutput); }
        }
        check(&mut canceled)?;
        Ok(parsed.blocks.iter().map(|block| block.span).collect())
    }
}

fn parse(source: &str, canceled: &mut impl FnMut() -> bool) -> Result<SpannedDocument, DocumentReadError> {
    check(canceled)?;
    // Upstream strips frontmatter from its AST but not its span collector.
    // This also refuses a gap/slice whose standalone parse would reinterpret
    // ordinary in-document source as a newly leading metadata block.
    let frontmatter = franken_markdown::parse::split_frontmatter(source).0.is_some();
    check(canceled)?;
    if frontmatter { return Err(DocumentReadError::InvalidOutput); }
    let parsed = franken_markdown::parse::parse_document_spanned(source);
    check(canceled)?;
    if parsed.source_len != source.len() || !parsed.diagnostics.is_empty() { return Err(DocumentReadError::InvalidOutput); }
    Ok(parsed)
}
fn empty_gap(source: &str, canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentReadError> {
    if !source.is_empty() && !parse(source, canceled)?.blocks.is_empty() { return Err(DocumentReadError::InvalidOutput); }
    Ok(())
}
fn nested_heading(block: &Block, depth: usize, nodes: &mut usize) -> Result<bool, DocumentReadError> {
    if depth > 128 || *nodes >= 131_072 { return Err(DocumentReadError::EngineBudget); }
    *nodes += 1;
    match block {
        Block::Heading { .. } => Ok(depth > 0),
        Block::BlockQuote(children) | Block::FootnoteDefinition { blocks: children, .. } => {
            for child in children { if nested_heading(child, depth + 1, nodes)? { return Ok(true); } }
            Ok(false)
        }
        Block::List(list) => {
            for item in &list.items {
                for child in &item.blocks { if nested_heading(child, depth + 1, nodes)? { return Ok(true); } }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), DocumentReadError> {
    if canceled() { Err(DocumentReadError::Canceled) } else { Ok(()) }
}
