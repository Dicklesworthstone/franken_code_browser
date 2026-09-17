#![forbid(unsafe_code)]

//! Work-accounted matching over the existing ignore compiler's AST. Wildcards
//! backtrack through one saved position instead of recursively enumerating all
//! partitions. The compatibility matcher and rule-aware discovery share this
//! implementation; there is no second grammar or dependency.

use super::{CompiledPattern, ExclusionCause, IgnoreDecision, IgnoreLayerKind,
    IgnoreMatcher, NormalizedPath, SegAtom, SegPat};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreWorkError { PathLimit, WorkLimit }

pub(crate) fn spend(remaining: &mut u64, amount: usize) -> Result<(), IgnoreWorkError> {
    let amount = u64::try_from(amount).map_err(|_| IgnoreWorkError::WorkLimit)?;
    if *remaining < amount { *remaining = 0; return Err(IgnoreWorkError::WorkLimit); }
    *remaining -= amount;
    Ok(())
}

impl IgnoreMatcher {
    /// Decrease the caller's remaining work allowance. Exhaustion is UNKNOWN,
    /// never an Include/Exclude answer. The caller must preserve incomplete
    /// policy evidence rather than treating the last evaluated rule as final.
    /// No recursion; path scratch is bounded to 256 borrowed segment pointers.
    pub fn decide_bounded(&mut self, path: &NormalizedPath, is_dir: bool,
        remaining: &mut u64) -> Result<IgnoreDecision, IgnoreWorkError> {
        if path.as_bytes().len() > 4096 || path.segments().len() > 256 {
            return Err(IgnoreWorkError::PathLimit);
        }
        spend(remaining, path.segments().len() + 1)?;
        let segments: Vec<&[u8]> = path.segments().iter().map(|segment| segment.as_bytes()).collect();
        let mut overridden = false;
        for prefix in &self.overrides {
            spend(remaining, 1)?;
            if prefix.len() > segments.len() { continue; }
            let mut equal = true;
            for (left, right) in segments.iter().zip(prefix) {
                spend(remaining, left.len().min(right.len()) + 1)?;
                if *left != right.as_slice() { equal = false; break; }
            }
            if equal { overridden = true; break; }
        }
        let mut last = None;
        for pattern in &self.patterns {
            spend(remaining, 1)?;
            if overridden && matches!(pattern.layer, IgnoreLayerKind::DefaultPolicy | IgnoreLayerKind::Scope) { continue; }
            if pattern_matches(pattern, &segments, is_dir, remaining)? {
                last = Some((pattern.negated, pattern.layer));
            }
        }
        Ok(match last {
            Some((false, layer)) => {
                self.excluded = self.excluded.saturating_add(1);
                IgnoreDecision::Exclude { cause: match layer {
                    IgnoreLayerKind::DefaultPolicy => ExclusionCause::DefaultPolicy,
                    IgnoreLayerKind::RuleFile => ExclusionCause::RuleFile,
                    IgnoreLayerKind::Scope => ExclusionCause::Scope,
                } }
            }
            _ => IgnoreDecision::Include,
        })
    }
}

pub(crate) fn pattern_matches(pattern: &CompiledPattern, path: &[&[u8]], is_dir: bool,
    remaining: &mut u64) -> Result<bool, IgnoreWorkError> {
    spend(remaining, 1)?;
    if pattern.directory_only && !is_dir { return Ok(false); }
    if path.len() < pattern.layer_prefix.len() { return Ok(false); }
    for (left, right) in path.iter().zip(&pattern.layer_prefix) {
        spend(remaining, left.len().min(right.len()) + 1)?;
        if *left != right.as_slice() { return Ok(false); }
    }
    let relative = &path[pattern.layer_prefix.len()..];
    if relative.is_empty() { return Ok(false); }
    if pattern.basename_only {
        return match pattern.segs.as_slice() {
            [SegPat::Atoms(atoms)] => match_atoms(atoms, relative[relative.len() - 1], remaining),
            [SegPat::GlobStar] => Ok(true),
            _ => Ok(false),
        };
    }
    match_segments(&pattern.segs, relative, remaining)
}

fn match_segments(pattern: &[SegPat], path: &[&[u8]], remaining: &mut u64)
    -> Result<bool, IgnoreWorkError> {
    let (mut p, mut s) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        spend(remaining, 1)?;
        if p == pattern.len() && s == path.len() { return Ok(true); }
        if matches!(pattern.get(p), Some(SegPat::GlobStar)) {
            star = Some((p + 1, s)); p += 1; continue;
        }
        if let (Some(SegPat::Atoms(atoms)), Some(segment)) = (pattern.get(p), path.get(s)) {
            if match_atoms(atoms, segment, remaining)? { p += 1; s += 1; continue; }
        }
        match star.as_mut() {
            Some((resume, consumed)) if *consumed < path.len() => {
                *consumed += 1; p = *resume; s = *consumed;
            }
            _ => return Ok(false),
        }
    }
}

fn match_atoms(atoms: &[SegAtom], bytes: &[u8], remaining: &mut u64)
    -> Result<bool, IgnoreWorkError> {
    let (mut a, mut s) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        spend(remaining, 1)?;
        if a == atoms.len() && s == bytes.len() { return Ok(true); }
        let matched = match atoms.get(a) {
            Some(SegAtom::Star) => { star = Some((a + 1, s)); a += 1; continue; }
            Some(SegAtom::Ques) if s < bytes.len() => Some(1),
            Some(SegAtom::Lit(literal)) => {
                spend(remaining, literal.len().min(bytes.len().saturating_sub(s)) + 1)?;
                bytes.get(s..).filter(|tail| tail.starts_with(literal)).map(|_| literal.len())
            }
            Some(SegAtom::Class { negated, bytes: class }) if s < bytes.len() => {
                spend(remaining, class.len() + 1)?;
                (class.contains(&bytes[s]) != *negated).then_some(1)
            }
            _ => None,
        };
        if let Some(width) = matched { a += 1; s += width; continue; }
        match star.as_mut() {
            Some((resume, consumed)) if *consumed < bytes.len() => {
                *consumed += 1; a = *resume; s = *consumed;
            }
            _ => return Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::compile_pattern;

    // Independent dynamic-programming wildcard oracle for small native bytes.
    fn reference(pattern: &[u8], text: &[u8]) -> bool {
        let mut row = vec![false; text.len() + 1]; row[0] = true;
        for &symbol in pattern {
            let mut next = vec![false; text.len() + 1];
            if symbol == b'*' { next[0] = row[0]; }
            for i in 1..=text.len() {
                next[i] = if symbol == b'*' { row[i] || next[i - 1] }
                    else { row[i - 1] && (symbol == b'?' || symbol == text[i - 1]) };
            }
            row = next;
        }
        row[text.len()]
    }
    #[test]
    fn greedy_atoms_equal_independent_dp_including_multiple_stars() {
        let patterns = ["a*b*c", "*a*b*", "a?*b", "*?*?*b", "ab*ab", "*", "?", "a*b*c*d*missing"];
        for pattern in patterns {
            let compiled = compile_pattern(IgnoreLayerKind::RuleFile, &[], pattern).unwrap();
            for length in 1..=8 {
                for mask in 0..1usize << length {
                    let bytes: Vec<u8> = (0..length).map(|bit| if mask & (1 << bit) == 0 { b'a' } else { b'b' }).collect();
                    assert_eq!(pattern_matches(&compiled, &[&bytes], false, &mut 1_000_000).unwrap(),
                        reference(pattern.as_bytes(), &bytes), "{pattern} {bytes:?}");
                }
            }
        }
    }
    #[test]
    fn global_work_refusal_is_not_the_last_partial_rule_decision() {
        let mut matcher = IgnoreMatcher::include_all();
        matcher.add_rule_file(None, "*.rs\n!keep.rs\n");
        let path = NormalizedPath::from_dirent_name(b"keep.rs").unwrap();
        for mut remaining in [0, 1, 4] {
            assert_eq!(matcher.decide_bounded(&path, false, &mut remaining), Err(IgnoreWorkError::WorkLimit));
        }
        assert_eq!(matcher.decide_bounded(&path, false, &mut 1000).unwrap(), IgnoreDecision::Include);
    }
    #[test]
    fn repeated_stars_and_globstars_use_no_recursive_call_stack() {
        let raw = (0..100).map(|_| "**/").collect::<String>() + "a*b*c*missing";
        let pattern = compile_pattern(IgnoreLayerKind::RuleFile, &[], &raw).unwrap();
        let path = vec![b"aaaaaaaaaaaaaaaaaaaa".as_slice(); 100];
        assert_eq!(pattern_matches(&pattern, &path, false, &mut 8), Err(IgnoreWorkError::WorkLimit));
        assert!(!pattern_matches(&pattern, &path, false, &mut 1_000_000).unwrap());
    }
}
