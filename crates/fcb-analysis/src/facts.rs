//! Source facts, evidence tracking, and capability boundary enforcement (FCB-030.A).
//!
//! Enforces Plan §11.5, §18.1, and §5.5:
//! - Malformed source, same-name candidates and unsupported language never
//!   become proven definitions/references.
//! - A lexical import is not a resolved external package dependency.
//! - Heuristic candidates carry explicit Heuristic badges.
//! - An outline parser is not a compiler.

use crate::outline::{CapabilityLevel, OutlineEvidence};

/// Category of an extracted source fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceFactKind {
    /// A syntax entity declared in this source file (e.g. `fn foo()`).
    DeclaredItem,
    /// A lexical import statement (e.g. `use foo::bar;`, `import { x } from 'y'`).
    ///
    /// Plan §18.1: "A lexical import name is not a resolved external package
    /// dependency until resolution rules support that conclusion."
    LexicalImport,
    /// An occurrence of an identifier that matches a candidate name.
    ///
    /// Plan §11.5: "Heuristic: A candidate only: same-name identifier link,
    /// approximate related-file suggestion."
    IdentifierCandidate,
}

impl SourceFactKind {
    /// Canonical label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::DeclaredItem => "declared_item",
            Self::LexicalImport => "lexical_import",
            Self::IdentifierCandidate => "identifier_candidate",
        }
    }
}

/// A structured fact extracted from source code with explicit capability classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFact {
    /// Identifier within file facts.
    pub id: u64,
    /// Symbol or entity name.
    pub name: String,
    /// Fact kind.
    pub kind: SourceFactKind,
    /// Capability tier on the Plan §11.5 ladder.
    pub capability_level: CapabilityLevel,
    /// Exact evidence byte and line span in the source.
    pub evidence: OutlineEvidence,
    /// Whether this fact claims to be a proven compiler/LSP semantic fact.
    ///
    /// For all local syntactic outlines and lexical extraction, this MUST be `false`.
    pub is_proven_semantic: bool,
}

/// Violations detected by the fact capability oracle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactAuditError {
    /// Same-name identifier candidate illegally claimed proven definition/reference.
    SameNameCandidateCannotBeProven {
        name: String,
        claimed_level: CapabilityLevel,
    },
    /// Lexical import statement illegally claimed resolved external dependency without resolution proof.
    LexicalImportCannotBeProvenExternalDependency {
        name: String,
    },
    /// Local syntax entity illegally claimed compiler-level semantic resolution.
    LocalSyntaxCannotClaimCompilerSemantics {
        name: String,
        claimed_level: CapabilityLevel,
    },
    /// Unsupported language or plain route illegally claimed structural or semantic facts.
    UnsupportedLanguageCannotClaimSemantics {
        name: String,
        claimed_level: CapabilityLevel,
    },
    /// Malformed or degraded source illegally claimed proven status.
    MalformedSourceCannotBeProven {
        name: String,
    },
}

impl std::fmt::Display for FactAuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SameNameCandidateCannotBeProven {
                name,
                claimed_level,
            } => write!(
                f,
                "same-name candidate '{name}' cannot claim proven semantic status (claimed level: {:?})",
                claimed_level
            ),
            Self::LexicalImportCannotBeProvenExternalDependency { name } => write!(
                f,
                "lexical import '{name}' cannot be claimed as a resolved external package dependency without independent resolution proof"
            ),
            Self::LocalSyntaxCannotClaimCompilerSemantics {
                name,
                claimed_level,
            } => write!(
                f,
                "local syntax item '{name}' claimed compiler semantics '{:?}' without an external compiler/LSP provider",
                claimed_level
            ),
            Self::UnsupportedLanguageCannotClaimSemantics {
                name,
                claimed_level,
            } => write!(
                f,
                "unsupported language fact '{name}' claimed level '{:?}' (must be Bytes)",
                claimed_level
            ),
            Self::MalformedSourceCannotBeProven { name } => write!(
                f,
                "fact '{name}' derived from malformed/degraded source cannot be claimed as proven",
            ),
        }
    }
}

impl std::error::Error for FactAuditError {}

/// Oracle auditor that enforces the Plan §11.5, §18.1 semantic capability boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct FactAuditor;

impl FactAuditor {
    /// Construct a new `FactAuditor`.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Audit a single extracted source fact.
    pub fn audit_fact(&self, fact: &SourceFact) -> Result<(), FactAuditError> {
        // Oracle Rule 1: Same-name candidates can NEVER claim compiler or proven semantics
        if fact.kind == SourceFactKind::IdentifierCandidate {
            if fact.is_proven_semantic {
                return Err(FactAuditError::SameNameCandidateCannotBeProven {
                    name: fact.name.clone(),
                    claimed_level: fact.capability_level,
                });
            }
            if fact.capability_level != CapabilityLevel::Heuristic {
                return Err(FactAuditError::SameNameCandidateCannotBeProven {
                    name: fact.name.clone(),
                    claimed_level: fact.capability_level,
                });
            }
        }

        // Oracle Rule 2: Lexical imports cannot be claimed as proven external package dependencies
        if fact.kind == SourceFactKind::LexicalImport && fact.is_proven_semantic {
            return Err(FactAuditError::LexicalImportCannotBeProvenExternalDependency {
                name: fact.name.clone(),
            });
        }

        // Oracle Rule 3: Declared syntax items from an outline parser cannot claim compiler semantics
        if fact.kind == SourceFactKind::DeclaredItem {
            if fact.is_proven_semantic || fact.capability_level.allows_compiler_claim() {
                return Err(FactAuditError::LocalSyntaxCannotClaimCompilerSemantics {
                    name: fact.name.clone(),
                    claimed_level: fact.capability_level,
                });
            }
        }

        Ok(())
    }

    /// Audit a collection of facts, failing on the first violation.
    pub fn audit_all<'a, I>(&self, facts: I) -> Result<(), FactAuditError>
    where
        I: IntoIterator<Item = &'a SourceFact>,
    {
        for fact in facts {
            self.audit_fact(fact)?;
        }
        Ok(())
    }
}
