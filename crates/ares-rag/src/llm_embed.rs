//! Remote embeddings via the Cordis [`ares_llm::Llm`] service.
//!
//! Always compiled (not gated on `local-embeddings`). Local ONNX models stay
//! in [`crate::embeddings`]; this module is the remote path that does not pull
//! fastembed.
//!
//! genai is a private HTTP adapter inside `ares-llm`. This crate never imports
//! `genai` or `GenaiClient`. Callers look up the `Llm` service and go through
//! [`ares_llm::Llm::embed`], which owns client resolution, intercepts
//! (`ModelOverride`, `TenantModelPolicy`), and the `llm.embed` waterfall.

use std::sync::Arc;

use ares_types::{AppError, Result};
use cordis::Context;

/// Embed `inputs` through [`ares_llm::Llm`] when that service is on `ctx`.
///
/// Looks up `Llm` with `ctx.get::<Llm>()`, then `llm.embed(&ctx, inputs)`.
/// `ctx` is `&Arc<Context>` because [`ares_llm::Llm::embed`] takes the same
/// handle used by `complete` / `get_client` (intercepts and EventsService).
///
/// # Errors
///
/// Returns [`AppError::Configuration`] if `Llm` is not provided on `ctx`.
/// Other errors come from `Llm::embed` (client resolution / provider embed).
pub async fn embed_with_llm(
    ctx: &Arc<Context>,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>> {
    let Some(llm) = ctx.get::<ares_llm::Llm>() else {
        return Err(AppError::Configuration(
            "Llm service is not provided for remote embeddings".into(),
        ));
    };
    llm.embed(ctx, inputs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::AppError;

    #[tokio::test]
    async fn embed_with_llm_errors_when_llm_missing() {
        let ctx = Context::new_root();
        let err = embed_with_llm(&ctx, &[String::from("hello")])
            .await
            .expect_err("missing Llm must fail closed");
        match err {
            AppError::Configuration(msg) => {
                assert_eq!(msg, "Llm service is not provided for remote embeddings");
            }
            other => panic!("expected Configuration, got {other:?}"),
        }
    }
}
