//! Person 6 — Concrete `ModelProvider` backed by Bob Shell.
//!
//! Shells out to the `bob` CLI in non-interactive mode exactly as the Python
//! `BobProvider` does, keeping both implementations consistent:
//!
//! ```text
//! bob --auth-method api-key --hide-intermediary-output -p "<prompt>"
//! ```
//!
//! Authentication: `BOB_API_KEY` environment variable, per
//! <https://bob.ibm.com/docs/shell/getting-started/install-and-setup#api-key-authentication>
//!
//! The system prompt and structured-output schema from `ModelRequest` are
//! embedded into the user prompt sent to the CLI; Bob Shell has no separate
//! system/user distinction at the command-line level.
//!
//! If the request carries a JSON schema, the response text is parsed as JSON
//! and placed in `ModelResponse::parsed`; on failure the raw text is still
//! returned so callers can fall back gracefully.

use std::env;

use async_trait::async_trait;
use tokio::process::Command;

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};

/// Concrete [`ModelProvider`] that delegates every call to the `bob` CLI.
pub struct BobShellProvider {
    /// Path to the `bob` binary.  Defaults to `"bob"` (PATH lookup).
    bob_path: String,
    /// Value of `BOB_API_KEY` to inject into the child process environment.
    api_key: String,
}

impl BobShellProvider {
    /// Construct from explicit values — mainly useful in tests.
    pub fn new(bob_path: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            bob_path: bob_path.into(),
            api_key: api_key.into(),
        }
    }

    /// Construct from the environment.
    ///
    /// Reads `BOB_API_KEY` (required) and `BOB_PATH` (optional, defaults
    /// to `"bob"`).  Returns `Err` if the API key is absent.
    pub fn from_env() -> Result<Self, String> {
        let api_key = env::var("BOB_API_KEY")
            .map_err(|_| "BOB_API_KEY environment variable is not set".to_string())?;
        if api_key.trim().is_empty() {
            return Err("BOB_API_KEY is set but empty".to_string());
        }
        let bob_path = env::var("BOB_PATH").unwrap_or_else(|_| "bob".to_string());
        Ok(Self { bob_path, api_key })
    }

    /// Build the single prompt string that is passed to `bob -p`.
    ///
    /// Bob Shell's CLI has no separate system/user distinction, so we
    /// concatenate them with a clear separator.  If the caller supplied a JSON
    /// schema, we append an instruction asking Bob to return JSON matching that
    /// schema; this mirrors what the Python pipeline does with delimiter
    /// instructions.
    fn build_prompt(req: &ModelRequest) -> String {
        let mut prompt = String::new();

        if !req.system.is_empty() {
            prompt.push_str("SYSTEM INSTRUCTIONS:\n");
            prompt.push_str(&req.system);
            prompt.push_str("\n\n");
        }

        prompt.push_str(&req.user);

        if let Some(schema) = &req.schema {
            prompt.push_str("\n\nYou MUST respond with valid JSON that matches this schema:\n");
            prompt.push_str(&serde_json::to_string_pretty(schema).unwrap_or_default());
            prompt.push_str("\nReturn only the JSON object — no markdown fences, no explanation.");
        }

        prompt
    }
}

#[async_trait]
impl ModelProvider for BobShellProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        let prompt = Self::build_prompt(&req);
        let model_id = req.model_id.clone();
        let has_schema = req.schema.is_some();

        let mut cmd = Command::new(&self.bob_path);
        cmd.args(["--auth-method", "api-key", "--hide-intermediary-output", "-p", &prompt])
            .env("BOB_API_KEY", &self.api_key)
            // Inherit the rest of the environment so PATH, HOME, etc. are available.
            .kill_on_drop(true);

        let output = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Bob Shell binary not found at '{}'. \
                     Install Bob Shell and ensure it is on PATH. \
                     See https://bob.ibm.com/docs/shell/getting-started/install-and-setup",
                    self.bob_path
                )
            } else {
                format!("Failed to spawn Bob Shell: {e}")
            }
        })?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let snippet: String = stderr.chars().take(300).collect();
            return Err(format!(
                "Bob Shell exited with code {code}. stderr: {snippet}"
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Attempt JSON parse when the request carried a schema.  Failure is
        // non-fatal: the caller can decide what to do with a plain-text response.
        let parsed = if has_schema {
            serde_json::from_str::<serde_json::Value>(&text).ok()
        } else {
            None
        };

        Ok(ModelResponse {
            text,
            parsed,
            model_id,
            input_tokens: None,
            output_tokens: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_errors_without_api_key() {
        // Temporarily remove the env var for this test.
        let saved = env::var("BOB_API_KEY").ok();
        unsafe { env::remove_var("BOB_API_KEY"); }
        let result = BobShellProvider::from_env();
        if let Some(v) = saved {
            unsafe { env::set_var("BOB_API_KEY", v); }
        }
        assert!(result.is_err());
        assert!(result.unwrap_err().to_lowercase().contains("bob_api_key"));
    }

    #[test]
    fn from_env_reads_api_key() {
        unsafe { env::set_var("BOB_API_KEY", "test-key-value"); }
        let p = BobShellProvider::from_env().expect("should succeed");
        unsafe { env::remove_var("BOB_API_KEY"); }
        assert_eq!(p.api_key, "test-key-value");
    }

    #[test]
    fn build_prompt_includes_system_and_user() {
        let req = ModelRequest {
            system: "You are a helpful assistant.".into(),
            user: "What is 2+2?".into(),
            schema: None,
            temperature: 0.0,
            model_id: "m".into(),
            max_tokens: None,
        };
        let p = BobShellProvider::build_prompt(&req);
        assert!(p.contains("SYSTEM INSTRUCTIONS:"));
        assert!(p.contains("You are a helpful assistant."));
        assert!(p.contains("What is 2+2?"));
    }

    #[test]
    fn build_prompt_appends_schema_instruction() {
        let req = ModelRequest {
            system: "sys".into(),
            user: "user".into(),
            schema: Some(serde_json::json!({"type":"object","required":["answer"]})),
            temperature: 0.0,
            model_id: "m".into(),
            max_tokens: None,
        };
        let p = BobShellProvider::build_prompt(&req);
        assert!(p.contains("valid JSON"));
        assert!(p.contains("answer"));
        assert!(p.contains("no markdown fences"));
    }

    #[test]
    fn build_prompt_no_schema_no_json_instruction() {
        let req = ModelRequest {
            system: "".into(),
            user: "hello".into(),
            schema: None,
            temperature: 0.0,
            model_id: "m".into(),
            max_tokens: None,
        };
        let p = BobShellProvider::build_prompt(&req);
        assert!(!p.contains("valid JSON"));
        assert_eq!(p.trim(), "hello");
    }

    #[test]
    fn build_prompt_omits_system_section_when_empty() {
        let req = ModelRequest {
            system: "".into(),
            user: "just the user part".into(),
            schema: None,
            temperature: 0.0,
            model_id: "m".into(),
            max_tokens: None,
        };
        let p = BobShellProvider::build_prompt(&req);
        assert!(!p.contains("SYSTEM INSTRUCTIONS:"));
    }
}
