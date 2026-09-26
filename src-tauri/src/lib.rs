// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod doc_fetch;
mod git_mining;
mod mock_provider;
mod provider;
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

/// Person 6 plugs their concrete provider into this state at app startup.
/// Until then, `StubProvider` keeps the app compiling and the IPC surface
/// testable — any call through it returns an explicit "not yet wired" error
/// rather than silently succeeding.
pub struct ProviderState {
    pub provider: Arc<dyn ModelProvider>,
}

struct StubProvider;

#[async_trait::async_trait]
impl ModelProvider for StubProvider {
    async fn call(&self, _req: provider::ModelRequest) -> Result<provider::ModelResponse, String> {
        Err("model provider not yet wired (owned by Person 6)".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let provider: Arc<dyn ModelProvider> =
        if std::env::var("GITLENS_MOCK_PROVIDER").as_deref() == Ok("1") {
            Arc::new(mock_provider::MockProvider)
        } else {
            Arc::new(StubProvider)
        };
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(ProviderState { provider: provider })
        .invoke_handler(tauri::generate_handler![
            greet,
            git_mining::extract_git_history,
            git_mining::discover_repo_subsystems,
            significance_ranking::rank_significant_commits,
            tour_narration::fetch_stop_documents,
            tour_narration::narrate_stop,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
