//! Person 3 — test-only `ModelProvider`.
//!
//! Delegates every call to `testing_api::dummy_response`, which returns a
//! schema-correct dummy `ModelResponse` for each pipeline stage.
//! NOT for production.

use crate::provider::{ModelProvider, ModelRequest, ModelResponse};
use crate::testing_api;

pub struct MockProvider;

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    async fn call(&self, req: ModelRequest) -> Result<ModelResponse, String> {
        Ok(testing_api::dummy_response(&req))
    }
}
