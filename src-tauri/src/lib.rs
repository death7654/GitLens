mod anthropic_provider;
mod bob_provider;
mod doc_fetch;
mod gemini_provider;
mod git_mining;
mod mock_provider;
mod openai_provider;
mod rate_limiter;
mod provider;
mod testing_api;
mod significance_ranking;
mod significance_ranking_stages;
mod significance_ranking_types;
mod tour_narration;
mod tour_types;

use provider::ModelProvider;
use std::sync::Arc;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Read a UTF-8 text file at `relative_path` inside the repository at `root`.
///
/// Returns the file contents as a string, or an error message if the path
/// escapes `root`, the file cannot be read, or the bytes are not valid UTF-8.
/// Binary files (images, compiled artefacts, etc.) are rejected with a clear
/// error rather than returning garbage.
#[tauri::command]
fn read_repo_file(root: String, relative_path: String) -> Result<String, String> {
    use std::path::Path;

    let base = Path::new(&root).canonicalize()
        .map_err(|e| format!("Cannot resolve repo root '{root}': {e}"))?;

    // Reject any path that contains ".." components before canonicalization
    // so we never accidentally follow symlinks out of the repo root.
    let joined = base.join(&relative_path);
    let resolved = joined.canonicalize()
        .map_err(|e| format!("Cannot resolve path '{relative_path}': {e}"))?;

    if !resolved.starts_with(&base) {
        return Err(format!("Path '{relative_path}' escapes the repository root."));
    }

    let bytes = std::fs::read(&resolved)
        .map_err(|e| format!("Cannot read '{relative_path}': {e}"))?;

    String::from_utf8(bytes)
        .map_err(|_| format!("'{relative_path}' is not a UTF-8 text file."))
}

/// Holds the active `ModelProvider` for the lifetime of the app.
///
/// Unlike the rest of this app's config, the API key is never read from the
/// environment or a `.env` file — it's entered by the user on the Settings
/// page and applied at runtime via the `set_api_key` command below, which
/// swaps out `provider` in place. That's why this is a `Mutex<Arc<...>>`
/// rather than a bare `Arc<...>`: every Tauri command that needs the
/// provider (`rank_significant_commits`, `narrate_stop`, …) takes a fresh
/// clone of the `Arc` from behind the lock at the start of each call, so a
/// key entered mid-session takes effect on the very next request with no
/// restart needed.
///
/// `set_api_key` also resolves and returns a model ID appropriate to the
/// chosen provider (see `default_model_id_for_provider`) — the frontend
/// captures that return value and uses it to build `RankingConfig`/
/// `TourConfig` for subsequent requests, since a model ID that's right for
/// one provider (e.g. `"gemini-flash-latest"`) is meaningless to another.
///
/// Provider selection at startup (in priority order):
///   1. `GITLENS_MOCK_PROVIDER=1`  → `MockProvider`  (test / CI — no key needed)
///   2. default                    → `StubProvider`  (returns a clear error
///      on every call until the user saves a key from Settings)
pub struct ProviderState {
    pub provider: std::sync::Mutex<Arc<dyn ModelProvider>>,
}

struct StubProvider;

#[async_trait::async_trait]
impl ModelProvider for StubProvider {
    async fn call(&self, _req: provider::ModelRequest) -> Result<provider::ModelResponse, String> {
        Err(
            "No model provider is configured. \
             Open Settings and save an API key for Gemini, OpenAI, or Anthropic, \
             or set GITLENS_MOCK_PROVIDER=1 for testing."
                .into(),
        )
    }
}

/// A reasonable default model for each provider, used when the Settings
/// page's optional Model field is left blank. Kept as "-latest"-style
/// aliases where the vendor offers one, for the same reason
/// `RankingConfig`/`TourConfig` default to `"gemini-flash-latest"` rather
/// than a pinned version: pinned model names get retired (see the comment
/// history on those defaults) and an alias tracks the vendor's current
/// recommendation instead.
fn default_model_id_for_provider(provider_name: &str) -> &'static str {
    match provider_name {
        "gemini" => "gemini-flash-latest",
        "openai" => "gpt-4o-mini",
        "anthropic" => "claude-3-5-haiku-latest",
        _ => "gemini-flash-latest",
    }
}

/// Backs the Settings page's "Save & apply" button
/// (`invoke('set_api_key', { providerName, apiKey, baseUrl, modelId })` in
/// `main.js`).
///
/// Swaps the app's active `ModelProvider` in place — every subsequent
/// ranking/summary/narration call picks up the new provider immediately, no
/// restart required. Returns the *resolved* model ID (the caller's
/// `model_id` if non-empty, else `default_model_id_for_provider`'s fallback)
/// so the frontend can reflect it back into the Model field and use it when
/// building `RankingConfig`/`TourConfig` for later requests.
///
/// `base_url` is used by the `"openai"` and `"anthropic"` providers (for a
/// local OpenAI-compatible server or a proxy, respectively); Gemini still
/// allows an override only via the `GEMINI_BASE_URL` env var, since the
/// Settings UI doesn't show that field for it.
#[tauri::command]
fn set_api_key(
    provider_name: String,
    api_key: String,
    base_url: Option<String>,
    model_id: Option<String>,
    provider_state: tauri::State<'_, ProviderState>,
) -> Result<String, String> {
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err("API key cannot be empty.".into());
    }

    let resolved_model_id = model_id
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default_model_id_for_provider(&provider_name).to_string());

    let new_provider: Arc<dyn ModelProvider> = match provider_name.as_str() {
        "gemini" => Arc::new(gemini_provider::GeminiProvider::new(api_key)),
        "openai" => Arc::new(openai_provider::OpenAiProvider::with_base_url(api_key, base_url)),
        "anthropic" => Arc::new(anthropic_provider::AnthropicProvider::with_base_url(api_key, base_url)),
        other => {
            return Err(format!(
                "Unknown provider '{other}'. Expected 'gemini', 'openai', or 'anthropic'."
            ))
        }
    };

    let mut guard = provider_state
        .provider
        .lock()
        .map_err(|e| format!("provider lock was poisoned: {e}"))?;
    *guard = new_provider;
    Ok(resolved_model_id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Load variables from `.env` in this crate's directory (src-tauri/) into
    // the process environment, before anything below reads
    // GITLENS_MOCK_PROVIDER, GEMINI_MAX_*, OPENAI_MAX_*, or GITLENS_CACHE_ROOT.
    // Note: API keys are NOT among these — they're entered on the Settings
    // page at runtime (see `set_api_key` below), not read from `.env`.
    //
    // We resolve the path via CARGO_MANIFEST_DIR (baked in at compile time as
    // the absolute path to src-tauri/ on the machine that built this binary)
    // rather than calling plain `dotenvy::dotenv()`, which instead searches
    // upward from the process's *current working directory*. That cwd is
    // src-tauri/ under `cargo tauri dev` — but for an installed/double-clicked
    // app it can be anywhere (home directory, an app bundle path, etc.), so
    // the cwd-based search silently finds nothing there. This only works for
    // binaries run on the same machine (and same source checkout) they were
    // built on, which matches this app's local-first, locally-built usage.
    //
    // A missing .env file is not an error — real environment variables (set
    // by a shell, CI, or the OS) always take precedence over .env values and
    // are still honoured with no .env file present at all.
    let dotenv_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
    if let Err(e) = dotenvy::from_path(&dotenv_path) {
        if !matches!(e, dotenvy::Error::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::NotFound) {
            eprintln!("[gitlens] Failed to load .env file at {}: {e}", dotenv_path.display());
        }
    }

    // No API key is ever read from the environment at startup — the user
    // supplies one from the Settings page once the app is running (see
    // `set_api_key`). Until then, every model call fails with a clear
    // "open Settings" message from StubProvider rather than a silent no-op.
    let provider: Arc<dyn ModelProvider> =
        if std::env::var("GITLENS_MOCK_PROVIDER").as_deref() == Ok("1") {
            Arc::new(mock_provider::MockProvider)
        } else {
            Arc::new(StubProvider)
        };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ProviderState { provider: std::sync::Mutex::new(provider) })
        .invoke_handler(tauri::generate_handler![
            greet,
            read_repo_file,
            set_api_key,
            git_mining::extract_git_history,
            git_mining::discover_repo_subsystems,
            git_mining::get_repo_status,
            significance_ranking::rank_significant_commits,
            tour_narration::fetch_stop_documents,
            tour_narration::narrate_stop,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}