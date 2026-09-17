#![forbid(unsafe_code)]

//! Collapsible and resizable sidebar containing Inspector, Results, History,
//! and Outline panels.
//!
//! Heuristic facts have explicit certainty badges and explanations. Unknown
//! facts are unknown, not zero. Search results retain their query generation
//! so stale batches never overwrite current results.

use fcb_core::{FileId, QueryGeneration, SourceRevision};

/// Available panels in the collapsible sidebar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SidebarPanel {
    /// File metadata, indexing status, relationships, and source facts.
    Inspector,
    /// Search query results and match navigation.
    Results,
    /// Visited location stack with back/forward navigation.
    History,
    /// Structural symbols, functions, types, and document headings.
    Outline,
}

impl SidebarPanel {
    pub const ALL: [Self; 4] = [
        Self::Inspector,
        Self::Results,
        Self::History,
        Self::Outline,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Inspector => "Inspector",
            Self::Results => "Search Results",
            Self::History => "History",
            Self::Outline => "Outline",
        }
    }
}

/// Certainty of a source fact presented in the Inspector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FactCertainty {
    /// Fact is compiler- or source-verified (exact).
    Authoritative,
    /// Fact is inferred from heuristic or partial lexical analysis.
    Heuristic { reason: String },
    /// Fact is not known (distinguished from zero or empty).
    Unknown,
}

/// A structured property row in the Inspector panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectorFact {
    pub key: String,
    pub label: String,
    pub value: Option<String>,
    pub certainty: FactCertainty,
}

/// State for the Inspector panel.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InspectorState {
    pub file_path: Option<String>,
    pub file_id: Option<FileId>,
    pub language: Option<String>,
    pub byte_count: Option<u64>,
    pub line_count: Option<usize>,
    pub revision: Option<SourceRevision>,
    pub indexing_status: String,
    pub facts: Vec<InspectorFact>,
    pub inbound_relations: Vec<String>,
    pub outbound_relations: Vec<String>,
}

impl InspectorState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// A single search result entry displayed in the Results panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchResultEntry {
    pub id: u64,
    pub file_id: FileId,
    pub path: String,
    pub line_number: usize,
    pub byte_range: (u64, u64),
    pub excerpt: String,
}

/// State for the Results panel.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultsState {
    pub query: String,
    pub query_generation: Option<QueryGeneration>,
    pub results: Vec<SearchResultEntry>,
    pub selected_index: Option<usize>,
    pub is_searching: bool,
    pub has_more: bool,
}

impl ResultsState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            query_generation: None,
            results: Vec::new(),
            selected_index: None,
            is_searching: false,
            has_more: false,
        }
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.query_generation = None;
        self.results.clear();
        self.selected_index = None;
        self.is_searching = false;
        self.has_more = false;
    }

    pub fn selected_result(&self) -> Option<&SearchResultEntry> {
        self.selected_index.and_then(|idx| self.results.get(idx))
    }

    pub fn select_next(&mut self) -> Option<&SearchResultEntry> {
        if self.results.is_empty() {
            return None;
        }
        let next_idx = match self.selected_index {
            Some(idx) => (idx + 1).min(self.results.len() - 1),
            None => 0,
        };
        self.selected_index = Some(next_idx);
        self.results.get(next_idx)
    }

    pub fn select_prev(&mut self) -> Option<&SearchResultEntry> {
        if self.results.is_empty() {
            return None;
        }
        let prev_idx = match self.selected_index {
            Some(idx) => idx.saturating_sub(1),
            None => 0,
        };
        self.selected_index = Some(prev_idx);
        self.results.get(prev_idx)
    }
}

impl Default for ResultsState {
    fn default() -> Self {
        Self::new()
    }
}

/// A recorded navigation point in the History panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryItem {
    pub id: u64,
    pub path: String,
    pub description: String,
    pub timestamp_nanos: u64,
}

/// State for the History panel with back/forward support.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryState {
    pub entries: Vec<HistoryItem>,
    pub current_index: Option<usize>,
}

impl HistoryState {
    pub const MAX_ENTRIES: usize = 128;

    pub fn can_go_back(&self) -> bool {
        self.current_index.is_some_and(|idx| idx > 0)
    }

    pub fn can_go_forward(&self) -> bool {
        self.current_index
            .is_some_and(|idx| idx + 1 < self.entries.len())
    }

    pub fn push(&mut self, item: HistoryItem) {
        if let Some(idx) = self.current_index {
            // Truncate any forward history when branching
            self.entries.truncate(idx + 1);
        }
        if self.entries.len() >= Self::MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push(item);
        self.current_index = Some(self.entries.len() - 1);
    }

    pub fn go_back(&mut self) -> Option<&HistoryItem> {
        if self.can_go_back() {
            let idx = self.current_index.unwrap() - 1;
            self.current_index = Some(idx);
            self.entries.get(idx)
        } else {
            None
        }
    }

    pub fn go_forward(&mut self) -> Option<&HistoryItem> {
        if self.can_go_forward() {
            let idx = self.current_index.unwrap() + 1;
            self.current_index = Some(idx);
            self.entries.get(idx)
        } else {
            None
        }
    }
}

/// A structural element in the Outline panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineSymbol {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub line_number: usize,
    pub depth: usize,
}

/// State for the Outline panel.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OutlineState {
    pub file_path: Option<String>,
    pub symbols: Vec<OutlineSymbol>,
    pub selected_symbol_index: Option<usize>,
}

impl OutlineState {
    pub fn clear(&mut self) {
        self.file_path = None;
        self.symbols.clear();
        self.selected_symbol_index = None;
    }

    pub fn selected_symbol(&self) -> Option<&OutlineSymbol> {
        self.selected_symbol_index
            .and_then(|idx| self.symbols.get(idx))
    }
}

/// Complete sidebar state.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarState {
    pub active_panel: SidebarPanel,
    pub is_collapsed: bool,
    pub width: f32,
    pub min_width: f32,
    pub max_width: f32,
    pub inspector: InspectorState,
    pub results: ResultsState,
    pub history: HistoryState,
    pub outline: OutlineState,
}

impl SidebarState {
    pub const DEFAULT_WIDTH: f32 = 300.0;
    pub const MIN_WIDTH: f32 = 200.0;
    pub const MAX_WIDTH: f32 = 800.0;

    pub fn new() -> Self {
        Self {
            active_panel: SidebarPanel::Inspector,
            is_collapsed: false,
            width: Self::DEFAULT_WIDTH,
            min_width: Self::MIN_WIDTH,
            max_width: Self::MAX_WIDTH,
            inspector: InspectorState::default(),
            results: ResultsState::new(),
            history: HistoryState::default(),
            outline: OutlineState::default(),
        }
    }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    pub fn set_width(&mut self, width: f32) {
        self.width = width.clamp(self.min_width, self.max_width);
    }
}

impl Default for SidebarState {
    fn default() -> Self {
        Self::new()
    }
}
