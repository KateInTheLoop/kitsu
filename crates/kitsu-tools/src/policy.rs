//! Filesystem access policy (design §7.1). Deny-by-default path allowlist:
//! a read is permitted only if its canonicalized path lies under one of the
//! allowed roots. Enforced centrally before any built-in fs tool touches disk.

use std::path::{Path, PathBuf};

use crate::error::ToolError;

/// Default per-tool output cap (bytes). Tool results larger than this are
/// truncated with a marker so a huge file can't blow the context budget.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// A read-only filesystem policy: a set of allowed roots plus an output cap.
#[derive(Debug, Clone)]
pub struct Policy {
    allowed_roots: Vec<PathBuf>,
    max_bytes: usize,
}

impl Policy {
    /// Build a policy from allow-path roots. Each root is canonicalized now so
    /// later `starts_with` checks compare real paths (defeats `..`/symlinks).
    /// A root that cannot be canonicalized (missing/invalid) is rejected.
    pub fn new(roots: &[String], max_bytes: usize) -> Result<Self, ToolError> {
        let mut allowed_roots = Vec::with_capacity(roots.len());
        for r in roots {
            let canon = std::fs::canonicalize(r).map_err(|e| {
                ToolError::PolicyDenied(format!("allow-path '{r}' is not accessible: {e}"))
            })?;
            allowed_roots.push(canon);
        }
        Ok(Self {
            allowed_roots,
            max_bytes,
        })
    }

    /// Convenience: a policy with the default byte cap.
    pub fn from_roots(roots: &[String]) -> Result<Self, ToolError> {
        Self::new(roots, DEFAULT_MAX_BYTES)
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub fn is_empty(&self) -> bool {
        self.allowed_roots.is_empty()
    }

    /// Check that `requested` resolves to a path under an allowed root. Returns
    /// the canonicalized path on success. On denial, the error names only the
    /// requested path, never the allowed roots (design §9: don't leak paths).
    pub fn check_read(&self, requested: &Path) -> Result<PathBuf, ToolError> {
        if self.allowed_roots.is_empty() {
            return Err(ToolError::PolicyDenied(format!(
                "path '{}' is not allowed (no allowed paths configured)",
                requested.display()
            )));
        }
        let canon = std::fs::canonicalize(requested).map_err(|_| {
            ToolError::PolicyDenied(format!(
                "path '{}' is not accessible or does not exist",
                requested.display()
            ))
        })?;
        if self.allowed_roots.iter().any(|root| canon.starts_with(root)) {
            Ok(canon)
        } else {
            Err(ToolError::PolicyDenied(format!(
                "path '{}' is outside the allowed paths",
                requested.display()
            )))
        }
    }

    /// Truncate `content` to the byte cap, appending a marker if it was cut.
    /// Respects UTF-8 char boundaries so the result is always valid text.
    pub fn cap_output(&self, mut content: String) -> String {
        if content.len() <= self.max_bytes {
            return content;
        }
        let mut end = self.max_bytes;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        content.push_str("\n...[truncated: output exceeded byte cap]");
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create temp file");
        f.write_all(body.as_bytes()).expect("write temp file");
        p
    }

    #[test]
    fn read_inside_root_is_allowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().to_string();
        let policy = Policy::from_roots(&[root]).expect("policy");
        let f = write_file(dir.path(), "ok.txt", "hi");
        assert!(policy.check_read(&f).is_ok());
    }

    #[test]
    fn read_outside_root_is_denied() {
        let allowed = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir2");
        let policy =
            Policy::from_roots(&[allowed.path().to_string_lossy().to_string()]).expect("policy");
        let f = write_file(other.path(), "secret.txt", "nope");
        let err = policy.check_read(&f).expect_err("should be denied");
        assert!(matches!(err, ToolError::PolicyDenied(_)));
    }

    #[test]
    fn dotdot_escape_is_denied() {
        // A sibling dir reached via `..` from the allowed root must be denied,
        // because canonicalize() resolves the `..` before the starts_with check.
        let base = tempfile::tempdir().expect("tempdir");
        let allowed = base.path().join("allowed");
        let sibling = base.path().join("sibling");
        std::fs::create_dir_all(&allowed).expect("mk allowed");
        std::fs::create_dir_all(&sibling).expect("mk sibling");
        let secret = write_file(&sibling, "secret.txt", "nope");
        let policy =
            Policy::from_roots(&[allowed.to_string_lossy().to_string()]).expect("policy");
        // Requested via the allowed root + traversal.
        let escape = allowed.join("..").join("sibling").join("secret.txt");
        let _ = secret;
        let err = policy.check_read(&escape).expect_err("traversal should be denied");
        assert!(matches!(err, ToolError::PolicyDenied(_)));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = Policy::from_roots(&[]).expect("policy");
        assert!(policy.is_empty());
        let f = write_file(dir.path(), "x.txt", "x");
        assert!(policy.check_read(&f).is_err());
    }

    #[test]
    fn cap_output_truncates_and_marks() {
        let policy = Policy::new(&[], 10).expect("policy");
        let capped = policy.cap_output("0123456789ABCDEF".to_string());
        assert!(capped.starts_with("0123456789"));
        assert!(capped.contains("truncated"));
        // Short content is untouched.
        assert_eq!(policy.cap_output("short".to_string()), "short");
    }
}
