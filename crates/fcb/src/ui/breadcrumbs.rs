#![forbid(unsafe_code)]

//! Hierarchical scope breadcrumbs (Root › crates › module › file).
//!
//! Provides precision path orientation and keyboard/pointer navigation
//! through the current repository depth without losing context.

/// A single clickable and focusable segment in the breadcrumb trail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeSegment {
    pub id: String,
    pub label: String,
    pub depth: usize,
    pub is_current: bool,
}

/// The complete breadcrumb trail with keyboard navigation focus.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScopeBreadcrumbs {
    pub segments: Vec<ScopeSegment>,
    pub focused_index: Option<usize>,
}

impl ScopeBreadcrumbs {
    pub fn new() -> Self {
        Self {
            segments: vec![ScopeSegment {
                id: "root".to_string(),
                label: "Root".to_string(),
                depth: 0,
                is_current: true,
            }],
            focused_index: Some(0),
        }
    }

    /// Set breadcrumb trail from a relative or absolute file path.
    pub fn set_from_path(&mut self, path: &str) {
        self.segments.clear();
        self.segments.push(ScopeSegment {
            id: "root".to_string(),
            label: "Root".to_string(),
            depth: 0,
            is_current: path.is_empty(),
        });

        if path.is_empty() {
            self.focused_index = Some(0);
            return;
        }

        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let total = parts.len();
        let mut accumulated = String::new();

        for (i, part) in parts.into_iter().enumerate() {
            if !accumulated.is_empty() {
                accumulated.push('/');
            }
            accumulated.push_str(part);

            self.segments.push(ScopeSegment {
                id: accumulated.clone(),
                label: part.to_string(),
                depth: i + 1,
                is_current: i + 1 == total,
            });
        }

        self.focused_index = Some(self.segments.len().saturating_sub(1));
    }

    /// Truncate breadcrumb trail back to the segment at `index`.
    pub fn truncate_to(&mut self, index: usize) -> Option<&ScopeSegment> {
        if index < self.segments.len() {
            self.segments.truncate(index + 1);
            for (i, seg) in self.segments.iter_mut().enumerate() {
                seg.is_current = i == index;
            }
            self.focused_index = Some(index);
            self.segments.get(index)
        } else {
            None
        }
    }

    pub fn focused_segment(&self) -> Option<&ScopeSegment> {
        self.focused_index.and_then(|idx| self.segments.get(idx))
    }

    pub fn navigate_left(&mut self) -> Option<&ScopeSegment> {
        if self.segments.is_empty() {
            return None;
        }
        let next_idx = match self.focused_index {
            Some(idx) => idx.saturating_sub(1),
            None => self.segments.len().saturating_sub(1),
        };
        self.focused_index = Some(next_idx);
        self.segments.get(next_idx)
    }

    pub fn navigate_right(&mut self) -> Option<&ScopeSegment> {
        if self.segments.is_empty() {
            return None;
        }
        let next_idx = match self.focused_index {
            Some(idx) => (idx + 1).min(self.segments.len().saturating_sub(1)),
            None => 0,
        };
        self.focused_index = Some(next_idx);
        self.segments.get(next_idx)
    }

    /// Format as standard text breadcrumb string (e.g. "Root › crates › fcb").
    pub fn as_display_string(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.label.as_str())
            .collect::<Vec<_>>()
            .join(" › ")
    }
}
