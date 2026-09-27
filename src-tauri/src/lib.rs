mod bob_provider;
mod doc_fetch;
mod git_mining;
mod mock_provider;
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
/// Provider selection at startup (in priority order):
///   1. `GITLENS_MOCK_PROVIDER=1`  → `MockProvider`  (test / CI)
///   2. `MODEL_PROVIDER=bob`       → `BobShellProvider` (needs `BOB_API_KEY`)
///   3. default                    → `StubProvider`  (returns an error on every call)
pub struct ProviderState {
    pub provider: Arc<dyn ModelProvider>,
}

struct StubProvider;

#[async_trait::async_trait]
impl ModelProvider for StubProvider {
    async fn call(&self, _req: provider::ModelRequest) -> Result<provider::ModelResponse, String> {
        Err(
            "No model provider is configured. \
             Set MODEL_PROVIDER=bob and BOB_API_KEY, \
             or set GITLENS_MOCK_PROVIDER=1 for testing."
                .into(),
        )
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let provider: Arc<dyn ModelProvider> =
        if std::env::var("GITLENS_MOCK_PROVIDER").as_deref() == Ok("1") {
            Arc::new(mock_provider::MockProvider)
        } else if std::env::var("MODEL_PROVIDER").as_deref() == Ok("bob") {
            match bob_provider::BobShellProvider::from_env() {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    eprintln!("[gitlens] BobShellProvider init failed: {e}");
                    eprintln!("[gitlens] Falling back to StubProvider.");
                    Arc::new(StubProvider)
                }
            }
        } else {
            Arc::new(StubProvider)
        };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ProviderState { provider })
        .invoke_handler(tauri::generate_handler![
            greet,
            read_repo_file,
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