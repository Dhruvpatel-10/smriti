//! Path safety utilities for export/import operations.
//!
//! Ensures filenames cannot escape the configured export directory.

use crate::error::{KbError, Result};

/// Validate that a user-provided name is a safe filename (no path traversal).
///
/// Rejects names containing `/`, `\`, `..`, null bytes, empty strings,
/// or names starting with `.`.
pub fn safe_filename(name: &str) -> Result<String> {
    if name.is_empty() {
        return Err(KbError::InvalidInput("filename must not be empty".into()));
    }
    if name.starts_with('.') {
        return Err(KbError::InvalidInput(
            "filename must not start with '.'".into(),
        ));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(KbError::InvalidInput(
            "filename must not contain path separators".into(),
        ));
    }
    if name.contains("..") {
        return Err(KbError::InvalidInput(
            "filename must not contain '..'".into(),
        ));
    }
    if name.contains('\0') {
        return Err(KbError::InvalidInput(
            "filename must not contain null bytes".into(),
        ));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty() {
        assert!(safe_filename("").is_err());
    }

    #[test]
    fn rejects_dot_prefix() {
        assert!(safe_filename(".hidden").is_err());
    }

    #[test]
    fn rejects_path_separators() {
        assert!(safe_filename("../etc/passwd").is_err());
        assert!(safe_filename("foo/bar").is_err());
        assert!(safe_filename("foo\\bar").is_err());
    }

    #[test]
    fn rejects_double_dots() {
        assert!(safe_filename("foo..bar").is_err());
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(safe_filename("foo\0bar").is_err());
    }

    #[test]
    fn accepts_valid_filenames() {
        assert_eq!(safe_filename("backup.jsonl").unwrap(), "backup.jsonl");
        assert_eq!(
            safe_filename("kb_export_2026-04-10.jsonl").unwrap(),
            "kb_export_2026-04-10.jsonl"
        );
    }
}
