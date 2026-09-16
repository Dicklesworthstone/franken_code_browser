#![forbid(unsafe_code)]

use franken_markdown::ast::{alert_body, Block};

/// Named features in the document dialect capability matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DialectFeature {
    /// Document footnotes (`[^1]` and `[^1]: note`).
    Footnotes,
    /// GitHub Flavored Markdown alerts / callouts (`> [!NOTE]`, etc.).
    Callouts,
    /// Reference-style links (`[text][ref]` and `[ref]: url`).
    ReferenceLinks,
    /// Headings with deterministic anchor slug identities.
    HeadingIds,
    /// Interactive/read-only task list check items (`- [ ]`, `- [x]`).
    TaskLists,
    /// Strikethrough text (`~~strikethrough~~`).
    Strikethrough,
    /// Raw inline and block HTML treatment.
    RawHtml,
}

impl DialectFeature {
    /// Short stable identifier for the feature.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Footnotes => "footnotes",
            Self::Callouts => "callouts",
            Self::ReferenceLinks => "reference_links",
            Self::HeadingIds => "heading_ids",
            Self::TaskLists => "task_lists",
            Self::Strikethrough => "strikethrough",
            Self::RawHtml => "raw_html",
        }
    }
}

/// Level of dialect support and conformance guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DialectSupportLevel {
    /// Full standard representation in AST and display layout.
    FullySupported,
    /// Explicitly qualified subset with strict behavioral boundaries.
    /// Does not claim 100% CommonMark/GFM edge-case compliance.
    QualifiedSubset,
    /// Unsupported or disabled constructs remain visible as literal/escaped text.
    PreservedVisible,
}

/// An explicit capability row in the document dialect matrix.
///
/// Plan §12.2: "Footnotes, callouts, reference links, HTML treatment, and
/// extension compatibility are explicit capability rows with fixtures.
/// The parser's actual conformance determines the advertised dialect; do not
/// claim full CommonMark/GFM conformance merely because the common cases look correct."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialectCapabilityRow {
    /// Target dialect feature.
    pub feature: DialectFeature,
    /// Conformance level achieved by the integrated parser.
    pub support_level: DialectSupportLevel,
    /// Advertised conformance scope and limitations.
    pub advertised_conformance: &'static str,
    /// Exact behavior when an unsupported or malformed construct is encountered.
    pub unsupported_behavior: &'static str,
    /// Qualified upstream parser rule or invariant.
    pub parser_rule: &'static str,
}

/// Document dialect configuration toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialectConfig {
    /// Active parsing profile (e.g. standard CommonMark/GFM vs GfmPlus).
    pub profile: DocumentProfile,
    /// When false (default), raw HTML is escaped and rendered as visible text.
    /// When true, controlled HTML blocks/inlines are admitted.
    pub allow_raw_html: bool,
    /// Whether footnote parsing and resolution are enabled.
    pub enable_footnotes: bool,
    /// Whether callout/alert transformation is enabled.
    pub enable_callouts: bool,
}

impl Default for DialectConfig {
    fn default() -> Self {
        Self {
            profile: DocumentProfile::GfmPlus,
            allow_raw_html: false,
            enable_footnotes: true,
            enable_callouts: true,
        }
    }
}

/// Document profile selection matching upstream FMD options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DocumentProfile {
    /// CommonMark + GFM (tables, task lists, strikethrough, autolinks).
    CommonMarkGfm,
    /// GFM-Plus: CommonMark + GFM + Footnotes + GitHub Alerts + Definition Lists.
    #[default]
    GfmPlus,
}

/// Truthful dialect matrix enumerating all supported and bounded dialect behaviors.
#[derive(Clone, Debug)]
pub struct DocumentDialectMatrix {
    rows: Vec<DialectCapabilityRow>,
    config: DialectConfig,
}

impl Default for DocumentDialectMatrix {
    fn default() -> Self {
        Self::standard()
    }
}

impl DocumentDialectMatrix {
    /// Builds the standard dialect matrix populated with all 7 explicit capability rows.
    pub fn standard() -> Self {
        Self::with_config(DialectConfig::default())
    }

    /// Builds the dialect matrix with explicit configuration toggles.
    pub fn with_config(config: DialectConfig) -> Self {
        let footnote_level = if config.enable_footnotes && config.profile == DocumentProfile::GfmPlus {
            DialectSupportLevel::QualifiedSubset
        } else {
            DialectSupportLevel::PreservedVisible
        };

        let callout_level = if config.enable_callouts && config.profile == DocumentProfile::GfmPlus {
            DialectSupportLevel::QualifiedSubset
        } else {
            DialectSupportLevel::PreservedVisible
        };

        let html_level = if config.allow_raw_html {
            DialectSupportLevel::QualifiedSubset
        } else {
            DialectSupportLevel::PreservedVisible
        };

        let rows = vec![
            DialectCapabilityRow {
                feature: DialectFeature::Footnotes,
                support_level: footnote_level,
                advertised_conformance: "Qualified GFM-Plus subset: numbered sequentially by appearance in body. Duplicate definitions keep the first definition. Undefined references remain literal visible text.",
                unsupported_behavior: "Undefined footnote references render as literal visible '[^id]', never misleading '[0]'. Disabled footnotes remain visible text.",
                parser_rule: "footnotes.rs: collect definitions; number by first reference; rewrite AST; leave undefined references intact.",
            },
            DialectCapabilityRow {
                feature: DialectFeature::Callouts,
                support_level: callout_level,
                advertised_conformance: "Qualified GFM Alert subset: [!NOTE], [!TIP], [!IMPORTANT], [!WARNING], [!CAUTION]. Tag must be on initial line without same-line prose.",
                unsupported_behavior: "Unknown alert tags (e.g. > [!INFO]) and same-line prose (> [!NOTE] text) stay plain blockquotes so text is never swallowed.",
                parser_rule: "ast.rs:alert_body parses alert header; trailing prose on same line is preserved as standard blockquote.",
            },
            DialectCapabilityRow {
                feature: DialectFeature::ReferenceLinks,
                support_level: DialectSupportLevel::QualifiedSubset,
                advertised_conformance: "Qualified CommonMark reference resolution: case-insensitive reference matching; distant definition updates re-resolve all matching call sites.",
                unsupported_behavior: "Malformed definitions (e.g. [ref]: with no URL) and undefined references remain visible text and produce recoverable diagnostics.",
                parser_rule: "parse/mod.rs: collect_link_references removes valid definition lines; malformed lines remain visible text.",
            },
            DialectCapabilityRow {
                feature: DialectFeature::HeadingIds,
                support_level: DialectSupportLevel::FullySupported,
                advertised_conformance: "Deterministic slug generation with collision disambiguation: lowercase ASCII alphanumerics, collapsed separators, 'section' default for empty titles, and '-2', '-3' collision suffixes.",
                unsupported_behavior: "Empty headings receive 'section'; colliding headings receive numeric suffixes; navigation links bind by slug identity, not pixel position.",
                parser_rule: "source_map.rs: slug_inlines + heading_slug_counts map each heading to a distinct deterministic slug anchor.",
            },
            DialectCapabilityRow {
                feature: DialectFeature::TaskLists,
                support_level: DialectSupportLevel::FullySupported,
                advertised_conformance: "GFM task list items with immutable read-only state: '- [ ]' for unchecked, '- [x]' / '- [X]' for checked.",
                unsupported_behavior: "Invalid checkbox formatting (e.g. '- [?]' or '- []') remains standard bullet list items without task state.",
                parser_rule: "ast.rs: ListItem.task contains Some(true) / Some(false) for valid task items; None for standard list items.",
            },
            DialectCapabilityRow {
                feature: DialectFeature::Strikethrough,
                support_level: DialectSupportLevel::FullySupported,
                advertised_conformance: "GFM strikethrough: '~~text~~' parsed to Inline::Strikethrough with exact source span provenance.",
                unsupported_behavior: "Unpaired or single tildes (~text~) remain literal visible text.",
                parser_rule: "ast.rs: Inline::Strikethrough wraps formatted inner inlines.",
            },
            DialectCapabilityRow {
                feature: DialectFeature::RawHtml,
                support_level: html_level,
                advertised_conformance: "Safe escaping by default: raw HTML tags escaped and rendered as visible text. allow_raw_html toggle admits controlled blocks/inlines without DOM execution.",
                unsupported_behavior: "When raw HTML is disabled, all tags (<script>, <div>, etc.) are escaped and rendered as harmless visible text.",
                parser_rule: "html.rs & ast.rs: HtmlBlock and Html inline nodes emitted only when allow_raw_html is enabled.",
            },
        ];

        Self { rows, config }
    }

    /// Access active configuration.
    pub fn config(&self) -> DialectConfig {
        self.config
    }

    /// Retrieve all capability rows.
    pub fn all_rows(&self) -> &[DialectCapabilityRow] {
        &self.rows
    }

    /// Retrieve the capability row for a specific feature.
    pub fn get_row(&self, feature: DialectFeature) -> Option<&DialectCapabilityRow> {
        self.rows.iter().find(|r| r.feature == feature)
    }

    /// Check if a feature is fully supported without restrictions.
    pub fn is_fully_supported(&self, feature: DialectFeature) -> bool {
        self.get_row(feature)
            .map(|r| r.support_level == DialectSupportLevel::FullySupported)
            .unwrap_or(false)
    }

    /// Check if a feature is a qualified subset.
    pub fn is_qualified_subset(&self, feature: DialectFeature) -> bool {
        self.get_row(feature)
            .map(|r| r.support_level == DialectSupportLevel::QualifiedSubset)
            .unwrap_or(false)
    }

    /// Evaluates whether a blockquote corresponds to a recognized callout/alert or standard blockquote.
    ///
    /// Truthful rule: Same-line prose (`> [!NOTE] text`) stays a normal blockquote so text is not swallowed.
    pub fn evaluate_callout(&self, blocks: &[Block]) -> CalloutEvaluation {
        if !self.config.enable_callouts || self.config.profile != DocumentProfile::GfmPlus {
            return CalloutEvaluation::StandardBlockQuote {
                reason: "callouts disabled by dialect configuration or profile",
            };
        }

        let inner = if let Some(Block::BlockQuote(inner)) = blocks.first() {
            inner.as_slice()
        } else {
            blocks
        };

        if let Some((tag, label, body_blocks)) = alert_body(inner) {
            CalloutEvaluation::Alert {
                tag,
                label,
                body_block_count: body_blocks.len(),
            }
        } else {
            CalloutEvaluation::StandardBlockQuote {
                reason: "not a recognized alert or has same-line prose preserving text",
            }
        }
    }

    /// Deterministically computes a heading slug given the heading title and prior slug collision counts.
    ///
    /// Matches upstream FMD `slug_inlines` and collision disambiguation (`-2`, `-3`, `section` for empty).
    pub fn compute_heading_slug(title: &str, existing_counts: &mut std::collections::HashMap<String, usize>) -> String {
        let mut base = String::new();
        let mut pending_dash = false;

        for c in title.chars() {
            if c.is_ascii_alphanumeric() {
                if pending_dash && !base.is_empty() {
                    base.push('-');
                    pending_dash = false;
                }
                base.push(c.to_ascii_lowercase());
            } else if c.is_whitespace() || c == '-' || c == '_' {
                pending_dash = true;
            }
        }

        if base.is_empty() {
            base = "section".to_string();
        }

        let count = existing_counts.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            base
        } else {
            format!("{}-{}", base, count)
        }
    }

    /// Evaluates task state for list items.
    pub fn evaluate_task_state(task: Option<bool>) -> TaskItemState {
        match task {
            Some(true) => TaskItemState::Checked,
            Some(false) => TaskItemState::Unchecked,
            None => TaskItemState::NonTask,
        }
    }

    /// Evaluates treatment of a raw HTML snippet under the active dialect config.
    pub fn evaluate_html_treatment(&self, raw_html: &str) -> HtmlTreatment {
        if self.config.allow_raw_html {
            HtmlTreatment::PassThrough {
                raw: raw_html.to_string(),
            }
        } else {
            HtmlTreatment::EscapedVisible {
                escaped: escape_html_for_display(raw_html),
            }
        }
    }
}

/// Evaluation outcome for blockquotes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalloutEvaluation {
    /// Qualified GitHub alert / callout.
    Alert {
        tag: &'static str,
        label: &'static str,
        body_block_count: usize,
    },
    /// Standard blockquote preserved without transformation.
    StandardBlockQuote {
        reason: &'static str,
    },
}

/// Task state for list items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskItemState {
    /// Checked task item (`- [x]` or `- [X]`).
    Checked,
    /// Unchecked task item (`- [ ]`).
    Unchecked,
    /// Ordinary bullet or numbered list item.
    NonTask,
}

/// Treatment applied to raw HTML snippets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HtmlTreatment {
    /// Rendered as safe visible text with characters escaped.
    EscapedVisible { escaped: String },
    /// Passed through as raw HTML when explicitly authorized.
    PassThrough { raw: String },
}

/// Escapes HTML special characters into safe display entities.
fn escape_html_for_display(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for c in src.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
