use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers;

use crate::error::{KbError, Result};

/// LLM client for background drain tasks.
///
/// Primary: Google Gemini 2.0 Flash (free tier, 1500 req/day)
/// Fallback: Groq Llama 3.1 70B (burst overflow)
///
/// Both are optional — if no API keys are configured, LLM features
/// degrade gracefully (pure-code path still works).
pub struct LlmClient {
    gemini: Option<providers::gemini::Client>,
    groq: Option<providers::groq::Client>,
}

const GEMINI_MODEL: &str = "gemini-2.0-flash";
const GROQ_MODEL: &str = "llama-3.1-70b-versatile";

impl LlmClient {
    /// Create an LLM client from environment variables.
    ///
    /// Reads:
    /// - `GEMINI_API_KEY` for Google Gemini
    /// - `GROQ_API_KEY` for Groq
    ///
    /// If neither is set, returns a client with no providers (LLM features disabled).
    pub fn from_env() -> Self {
        let gemini = std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .and_then(|key| {
                match providers::gemini::Client::new(&key) {
                    Ok(client) => {
                        tracing::info!("Gemini LLM client initialized");
                        Some(client)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to initialize Gemini client: {e}");
                        None
                    }
                }
            });

        let groq = std::env::var("GROQ_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .and_then(|key| {
                match providers::groq::Client::new(&key) {
                    Ok(client) => {
                        tracing::info!("Groq LLM client initialized");
                        Some(client)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to initialize Groq client: {e}");
                        None
                    }
                }
            });

        if gemini.is_none() && groq.is_none() {
            tracing::info!("No LLM API keys configured — background intelligence disabled");
        }

        Self { gemini, groq }
    }

    /// Whether any LLM provider is available.
    pub fn is_available(&self) -> bool {
        self.gemini.is_some() || self.groq.is_some()
    }

    /// Send a prompt to the LLM (Gemini primary, Groq fallback).
    /// Returns the response text, or None if no provider is available.
    pub async fn prompt(&self, system_prompt: &str, user_prompt: &str) -> Result<Option<String>> {
        if !self.is_available() {
            return Ok(None);
        }

        // Try Gemini first
        if let Some(ref client) = self.gemini {
            let agent = client
                .agent(GEMINI_MODEL)
                .preamble(system_prompt)
                .build();

            match agent.prompt(user_prompt).await {
                Ok(response) => return Ok(Some(response)),
                Err(e) => {
                    tracing::debug!("Gemini prompt failed: {e}, trying Groq fallback");
                }
            }
        }

        // Groq fallback
        if let Some(ref client) = self.groq {
            let agent = client
                .agent(GROQ_MODEL)
                .preamble(system_prompt)
                .build();

            match agent.prompt(user_prompt).await {
                Ok(response) => return Ok(Some(response)),
                Err(e) => {
                    tracing::warn!("Groq prompt failed: {e}");
                    return Err(KbError::Llm(format!("All LLM providers failed: {e}")));
                }
            }
        }

        Ok(None)
    }
}
