#![forbid(unsafe_code)]

//! Scope disclosure and root confinement contracts (FCB-053.B).
//!
//! Opening a repository or file grants a specific read scope, not unbounded
//! filesystem execution. Per plan §22.6 and §23.4:
//! - Raw paths with spaces, Unicode, CJK, and emoji must be preserved faithfully.
//! - When a `--root` is granted, opening a file inside that root resolves to
//!   an allowed relative path under that root.
//! - An `open FILE` outside the granted root must NEVER silently widen the root.
//!   It requires explicit disclosure and consent, or refusal.
//! - Root escape attempts (traversal, upward lexical climb, or escaping symlink
//!   destinations) are strictly refused.

use std::path::{Component, Path};

/// A granted root directory representing an authorized read scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedRoot {
    canonical_path: String,
    label: String,
}

impl GrantedRoot {
    pub fn new(path: &str, label: &str) -> Result<Self, ScopeRefusal> {
        let normalized = normalize_lexical_path(path)?;
        Ok(Self {
            canonical_path: normalized,
            label: label.to_string(),
        })
    }

    pub fn canonical_path(&self) -> &str {
        &self.canonical_path
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Outcome of evaluating an open request against granted scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeEvaluation {
    /// The target file is within an existing granted root.
    WithinGrant {
        root: String,
        relative_path: String,
        full_path: String,
    },
    /// The target file is outside all granted roots; requires explicit user scope disclosure.
    RequiresDisclosure(ScopeDisclosure),
}

/// Structured disclosure explaining scope widening requirement to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDisclosure {
    /// The exact target file path requested.
    pub requested_path: String,
    /// Suggested new root to grant.
    pub suggested_root: String,
    /// List of currently granted roots.
    pub existing_roots: Vec<String>,
    /// Human-readable explanation.
    pub explanation: String,
}

/// Refusal reasons when a path violates scope confinement or format rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeRefusal {
    EmptyPath,
    NotAbsolute,
    TraversalEscape,
    ControlCharacters,
    SymlinkEscape { link: String, target: String },
}

/// Normalizes a path string lexically without filesystem I/O.
/// Rejects empty paths, relative paths, control characters, and root escapes.
pub fn normalize_lexical_path(raw: &str) -> Result<String, ScopeRefusal> {
    if raw.is_empty() {
        return Err(ScopeRefusal::EmptyPath);
    }

    if raw.chars().any(|c| (c as u32) < 0x20 || (c as u32) == 0x7f) {
        return Err(ScopeRefusal::ControlCharacters);
    }

    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(ScopeRefusal::NotAbsolute);
    }

    let mut parts: Vec<&str> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(ScopeRefusal::TraversalEscape);
                }
            }
            Component::Normal(os) => {
                let s = os.to_str().ok_or(ScopeRefusal::ControlCharacters)?;
                parts.push(s);
            }
            Component::Prefix(_) => {
                return Err(ScopeRefusal::NotAbsolute);
            }
        }
    }

    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    Ok(out)
}

/// Evaluates an open file request against granted roots.
///
/// If `requested_path` is inside any `granted_roots`, returns `WithinGrant`.
/// If `requested_path` is outside all granted roots, returns `RequiresDisclosure`.
/// NEVER silently widens the granted roots.
pub fn evaluate_open_file(
    requested_path: &str,
    granted_roots: &[GrantedRoot],
) -> Result<ScopeEvaluation, ScopeRefusal> {
    let normalized_target = normalize_lexical_path(requested_path)?;

    for root in granted_roots {
        let root_str = root.canonical_path();
        if normalized_target == root_str {
            return Ok(ScopeEvaluation::WithinGrant {
                root: root_str.to_string(),
                relative_path: String::new(),
                full_path: normalized_target,
            });
        }

        let prefix = if root_str == "/" {
            "/".to_string()
        } else {
            format!("{root_str}/")
        };

        if let Some(rel) = normalized_target.strip_prefix(&prefix) {
            return Ok(ScopeEvaluation::WithinGrant {
                root: root_str.to_string(),
                relative_path: rel.to_string(),
                full_path: normalized_target,
            });
        }
    }

    // Path is outside all granted roots. Construct a safe scope disclosure.
    let suggested_root = derive_suggested_root(&normalized_target);
    let existing: Vec<String> = granted_roots
        .iter()
        .map(|r| r.canonical_path().to_string())
        .collect();

    Ok(ScopeEvaluation::RequiresDisclosure(ScopeDisclosure {
        requested_path: normalized_target.clone(),
        suggested_root,
        existing_roots: existing,
        explanation: format!(
            "Target path '{normalized_target}' is outside granted roots. Access requires explicit grant."
        ),
    }))
}

/// Helper to derive a sensible suggested parent root for an outside-scope target.
fn derive_suggested_root(path_str: &str) -> String {
    let p = Path::new(path_str);
    if let Some(parent) = p.parent() {
        let s = parent.to_str().unwrap_or("/");
        if s.is_empty() {
            "/".to_string()
        } else {
            s.to_string()
        }
    } else {
        "/".to_string()
    }
}

/// Validates that a symlink target stays strictly confined within the granted root.
///
/// Both absolute and relative symlink destinations are resolved lexically relative
/// to the symlink's containing directory. If the resolved destination escapes
/// `root_path`, returns `Err(ScopeRefusal::SymlinkEscape)`.
pub fn validate_symlink_confinement(
    symlink_path: &str,
    symlink_dest: &str,
    root_path: &str,
) -> Result<String, ScopeRefusal> {
    let norm_root = normalize_lexical_path(root_path)?;
    let norm_link = normalize_lexical_path(symlink_path)?;

    // Ensure the link itself is within the root.
    let root_prefix = if norm_root == "/" {
        "/".to_string()
    } else {
        format!("{norm_root}/")
    };
    if norm_link != norm_root && !norm_link.starts_with(&root_prefix) {
        return Err(ScopeRefusal::SymlinkEscape {
            link: symlink_path.to_string(),
            target: symlink_dest.to_string(),
        });
    }

    // Resolve target path.
    let target_norm = if symlink_dest.starts_with('/') {
        normalize_lexical_path(symlink_dest)?
    } else {
        let link_parent = Path::new(&norm_link).parent().unwrap_or(Path::new("/"));
        let joined = link_parent.join(symlink_dest);
        let joined_str = joined.to_str().ok_or(ScopeRefusal::ControlCharacters)?;
        normalize_lexical_path(joined_str)?
    };

    // Verify resolved destination stays within root.
    if target_norm == norm_root || target_norm.starts_with(&root_prefix) {
        Ok(target_norm)
    } else {
        Err(ScopeRefusal::SymlinkEscape {
            link: symlink_path.to_string(),
            target: target_norm,
        })
    }
}
