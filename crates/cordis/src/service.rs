use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;

use crate::context::Context;
use crate::effect::Disposable;

#[derive(Debug, Error)]
pub enum CordisError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("fiber error: {0}")]
    Fiber(String),
}

pub type ServiceInitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Box<dyn Disposable>>, CordisError>> + Send + 'a>>;

pub trait Service: Send + Sync + 'static {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        Box::pin(async move { Ok(None) })
    }

    fn check(&self) -> bool {
        true
    }
}
