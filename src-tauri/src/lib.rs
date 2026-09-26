// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod bob_provider;
mod git_mining;
mod mock_provider;
mod provider;
mod testing_api;
mod significance_ranking;
mod significance_ranking_stages;
mod significance_ranking_types;

use provider::ModelProvider;
use std::sync::Arc;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
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
        .plugin(tauri_plugin_opener::init())
        .manage(ProviderState { provider })
        .invoke_handler(tauri::generate_handler![
            greet,
            git_mining::extract_git_history,
            git_mining::discover_repo_subsystems,
            significance_ranking::rank_significant_commits,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
