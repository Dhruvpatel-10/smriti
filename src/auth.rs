/// Bearer-token auth layer for the HTTP daemon (ADR-027).
///
/// Tokens are pre-shared secrets, one per user, loaded from a TOML file at
/// startup. Each token maps to a human-readable identity string used only for
/// debug-level logging — no token value is ever logged.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{KbError, Result};

/// Opaque bearer-token store: maps token → identity.
/// Kept behind an `Arc` so it can be shared across connection tasks without
/// requiring `'static` on the service closure.
#[derive(Clone, Debug)]
pub struct TokenMap(Arc<HashMap<String, String>>);

impl TokenMap {
    /// Return the identity for a given token, or `None` if not present.
    pub fn resolve(&self, token: &str) -> Option<&str> {
        self.0.get(token).map(String::as_str)
    }

    /// `true` when no tokens were configured (auth-disabled mode).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Intermediate deserialization shape for the tokens TOML file.
#[derive(Deserialize)]
struct TokensFile {
    tokens: HashMap<String, String>,
}

/// Load and validate the tokens file.
///
/// Returns `Ok(None)` when the file does not exist (auth-disabled mode).
/// Returns `Ok(Some(map))` on success.
/// Returns `Err` on parse failure or invalid token shape (refuses to start).
pub fn load_tokens(path: &Path) -> Result<Option<TokenMap>> {
    let contents = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %path.display(),
                "auth disabled — tokens file not found; anyone on localhost can read all memories"
            );
            return Ok(None);
        }
        Err(e) => {
            return Err(KbError::Config(format!(
                "cannot read tokens file '{}': {e}",
                path.display()
            )));
        }
        Ok(s) => s,
    };

    if contents.trim().is_empty() {
        tracing::warn!(
            path = %path.display(),
            "auth disabled — tokens file is empty; anyone on localhost can read all memories"
        );
        return Ok(None);
    }

    let parsed: TokensFile = toml::from_str(&contents).map_err(|e| {
        KbError::Config(format!(
            "malformed tokens file '{}': {e}",
            path.display()
        ))
    })?;

    if parsed.tokens.is_empty() {
        tracing::warn!(
            path = %path.display(),
            "auth disabled — [tokens] table is empty; anyone on localhost can read all memories"
        );
        return Ok(None);
    }

    validate_tokens(&parsed.tokens)?;

    tracing::info!(
        path = %path.display(),
        count = parsed.tokens.len(),
        "bearer auth enabled"
    );

    // Invert identity→token to token→identity so lookups by token are O(1).
    let inverted: HashMap<String, String> = parsed
        .tokens
        .into_iter()
        .map(|(identity, token)| (token, identity))
        .collect();

    Ok(Some(TokenMap(Arc::new(inverted))))
}

/// Each token must be ≥ 32 characters and consist only of alphanumeric chars
/// or `-` / `_` (hex tokens produced by `openssl rand -hex 32` satisfy this).
fn validate_tokens(tokens: &HashMap<String, String>) -> Result<()> {
    for (identity, token) in tokens {
        if token.len() < 32 {
            return Err(KbError::Config(format!(
                "token for '{identity}' is too short (minimum 32 characters); \
                 generate with: openssl rand -hex 32"
            )));
        }
        if !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(KbError::Config(format!(
                "token for '{identity}' contains disallowed characters; \
                 allowed: [a-zA-Z0-9-_]"
            )));
        }
    }
    Ok(())
}

/// Check the `Authorization: Bearer <token>` header.
///
/// Returns `Ok(identity)` on success, `Err(401_body)` when auth fails.
/// When the token map is empty (auth-disabled), always succeeds with identity
/// `"anonymous"`.
///
/// Token values are never logged — only the resolved identity name.
pub fn check_bearer(map: &Option<TokenMap>, auth_header: Option<&str>) -> Result<String> {
    let Some(tokens) = map else {
        // Auth-disabled mode — accept all.
        return Ok("anonymous".to_string());
    };

    let header = match auth_header {
        None => {
            return Err(KbError::InvalidInput(
                "missing Authorization header".to_string(),
            ))
        }
        Some(h) => h,
    };

    let token = match header.strip_prefix("Bearer ") {
        None => {
            return Err(KbError::InvalidInput(
                "Authorization header must use Bearer scheme".to_string(),
            ))
        }
        Some(t) => t,
    };

    match tokens.resolve(token) {
        Some(identity) => {
            tracing::debug!(identity, "request authenticated");
            Ok(identity.to_string())
        }
        None => Err(KbError::InvalidInput("invalid bearer token".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn valid_tokens_load_ok() {
        let token_a = "a".repeat(32);
        let token_b = "b1B2c3D4e5".repeat(4); // alphanumeric, 40 chars
        let content = format!(
            "[tokens]\nalice = \"{token_a}\"\nbob = \"{token_b}\"\n"
        );
        let f = write_temp(&content);
        let result = load_tokens(f.path()).unwrap();
        assert!(result.is_some());
        let map = result.unwrap();
        assert_eq!(map.resolve(&token_a), Some("alice"));
        assert_eq!(map.resolve(&token_b), Some("bob"));
    }

    #[test]
    fn token_too_short_is_rejected() {
        let content = "[tokens]\nalice = \"tooshort\"\n";
        let f = write_temp(content);
        let err = load_tokens(f.path()).unwrap_err();
        assert!(err.to_string().contains("too short"));
    }

    #[test]
    fn token_with_bad_chars_is_rejected() {
        // Contains a space — not allowed.
        let content = "[tokens]\nalice = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \"\n";
        let f = write_temp(content);
        let err = load_tokens(f.path()).unwrap_err();
        assert!(err.to_string().contains("disallowed characters"));
    }

    #[test]
    fn missing_file_returns_none() {
        let result = load_tokens(Path::new("/tmp/nonexistent_smriti_tokens_xyz.toml")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn empty_file_returns_none() {
        let f = write_temp("   \n");
        let result = load_tokens(f.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn empty_tokens_table_returns_none() {
        let f = write_temp("[tokens]\n");
        let result = load_tokens(f.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn check_bearer_no_auth_disabled() {
        // auth-disabled: no header → anonymous
        let identity = check_bearer(&None, None).unwrap();
        assert_eq!(identity, "anonymous");
    }

    #[test]
    fn check_bearer_with_valid_token() {
        let token = "x".repeat(32);
        let mut map = HashMap::new();
        map.insert(token.clone(), "alice".to_string());
        let tmap = Some(TokenMap(Arc::new(map)));
        let header = format!("Bearer {token}");
        let identity = check_bearer(&tmap, Some(&header)).unwrap();
        assert_eq!(identity, "alice");
    }

    #[test]
    fn check_bearer_missing_header_returns_err() {
        let token = "x".repeat(32);
        let mut map = HashMap::new();
        map.insert(token, "alice".to_string());
        let tmap = Some(TokenMap(Arc::new(map)));
        let err = check_bearer(&tmap, None).unwrap_err();
        assert!(err.to_string().contains("missing Authorization"));
    }

    #[test]
    fn check_bearer_wrong_scheme_returns_err() {
        let token = "x".repeat(32);
        let mut map = HashMap::new();
        map.insert(token.clone(), "alice".to_string());
        let tmap = Some(TokenMap(Arc::new(map)));
        let err = check_bearer(&tmap, Some(&format!("Basic {token}"))).unwrap_err();
        assert!(err.to_string().contains("Bearer scheme"));
    }

    #[test]
    fn check_bearer_invalid_token_returns_err() {
        let token = "x".repeat(32);
        let mut map = HashMap::new();
        map.insert(token, "alice".to_string());
        let tmap = Some(TokenMap(Arc::new(map)));
        let err = check_bearer(&tmap, Some("Bearer wrongtoken")).unwrap_err();
        assert!(err.to_string().contains("invalid bearer token"));
    }
}
