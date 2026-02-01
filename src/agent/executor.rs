//! LLM execution trait for structured output support.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::error::Result;

/// Trait for LLM execution with structured output support.
/// Implementations provide both raw text and typed JSON Schema responses.
pub trait LlmExecutor: Send + Sync {
    /// Execute prompt and return raw text response.
    fn execute<'a>(
        &'a self,
        prompt: &'a str,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

    /// Execute prompt and return structured response.
    /// Uses JSON Schema enforcement at the API level.
    fn execute_structured<'a, T>(
        &'a self,
        prompt: &'a str,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static;
}

/// Blanket implementation for Arc-wrapped executors.
impl<E: LlmExecutor> LlmExecutor for Arc<E> {
    fn execute<'a>(
        &'a self,
        prompt: &'a str,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        (**self).execute(prompt, working_dir)
    }

    fn execute_structured<'a, T>(
        &'a self,
        prompt: &'a str,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        (**self).execute_structured(prompt, working_dir)
    }
}
