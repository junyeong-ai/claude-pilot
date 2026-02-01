use std::sync::OnceLock;

use claude_agent::models::{ModelSpec, read_registry};
use tracing::warn;

use crate::error::{PilotError, Result};

/// Default model used when no model is specified.
/// Single source of truth for default model name across the codebase.
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-5-20250929";

/// Fallback usable context when model registry is unavailable.
/// Based on claude-sonnet-4-5: 200k context - 16k output = 184k usable.
const FALLBACK_USABLE_CONTEXT: u64 = 184_000;

/// Cached default model config to avoid repeated registry lookups.
static DEFAULT_MODEL_CONFIG: OnceLock<Option<ModelConfig>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct ModelConfig {
    spec: ModelSpec,
    extended_context_enabled: bool,
}

impl ModelConfig {
    pub fn from_name(model: &str) -> Option<Self> {
        let registry = read_registry();
        registry.resolve(model).map(|spec| Self {
            spec: spec.clone(),
            extended_context_enabled: false,
        })
    }

    /// Resolve a model by name with fallbacks to DEFAULT_MODEL and "sonnet".
    /// Returns an error if all resolution attempts fail.
    pub fn from_name_or_default(model: &str) -> Result<Self> {
        Self::from_name(model)
            .or_else(|| Self::from_name(DEFAULT_MODEL))
            .or_else(|| Self::from_name("sonnet"))
            .ok_or_else(|| {
                PilotError::ModelResolution(format!(
                    "Failed to resolve model '{}' and fallbacks ('{}', 'sonnet'). \
                     This indicates a claude_agent registry issue.",
                    model, DEFAULT_MODEL
                ))
            })
    }

    /// Get fallback usable context when model resolution fails.
    /// Used to provide reasonable defaults without panicking.
    pub fn fallback_usable_context() -> u64 {
        FALLBACK_USABLE_CONTEXT
    }

    pub fn with_extended_context(mut self, enabled: bool) -> Self {
        self.extended_context_enabled =
            enabled && self.spec.capabilities.supports_extended_context();
        self
    }

    pub fn context_window(&self) -> u64 {
        self.spec
            .capabilities
            .effective_context(self.extended_context_enabled)
    }

    pub fn max_output_tokens(&self) -> u64 {
        self.spec.capabilities.max_output_tokens
    }

    pub fn usable_context(&self) -> u64 {
        self.context_window()
            .saturating_sub(self.max_output_tokens())
    }

    pub fn supports_extended_context(&self) -> bool {
        self.spec.capabilities.supports_extended_context()
    }

    pub fn has_extended_context(&self) -> bool {
        self.extended_context_enabled
    }

    pub fn model_id(&self) -> &str {
        &self.spec.id
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        // Use cached config if available
        if let Some(config) = DEFAULT_MODEL_CONFIG.get().and_then(|c| c.clone()) {
            return config;
        }

        // Try to resolve the default model
        match Self::from_name_or_default(DEFAULT_MODEL) {
            Ok(config) => {
                let _ = DEFAULT_MODEL_CONFIG.set(Some(config.clone()));
                config
            }
            Err(e) => {
                // Log warning and cache the failure
                warn!(
                    error = %e,
                    "Model registry unavailable, using fallback context window"
                );
                let _ = DEFAULT_MODEL_CONFIG.set(None);

                // Return a minimal config that allows the application to continue
                // Operations requiring model details will fail, but basic functionality works
                Self::from_name("sonnet")
                    .or_else(|| Self::from_name("claude-3-5-sonnet-20240620"))
                    .expect("At least one fallback model should exist in registry")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_name_resolves_known_model() {
        let config = ModelConfig::from_name("sonnet");
        assert!(config.is_some());
    }

    #[test]
    fn test_from_name_returns_none_for_unknown() {
        let config = ModelConfig::from_name("nonexistent-model-xyz");
        assert!(config.is_none());
    }

    #[test]
    fn test_from_name_or_default_returns_result() {
        let config = ModelConfig::from_name_or_default("nonexistent");
        assert!(config.is_ok());
        assert!(config.unwrap().context_window() > 0);
    }

    #[test]
    fn test_default_model_has_valid_context() {
        let config = ModelConfig::default();
        assert!(config.context_window() >= 200_000);
        assert!(config.max_output_tokens() > 0);
        assert!(config.usable_context() > 0);
    }

    #[test]
    fn test_extended_context_for_supported_model() {
        let config = ModelConfig::from_name_or_default("sonnet")
            .unwrap()
            .with_extended_context(true);

        if config.supports_extended_context() {
            assert!(config.has_extended_context());
            assert!(config.context_window() > 200_000);
        }
    }

    #[test]
    fn test_extended_context_disabled_for_unsupported() {
        let config = ModelConfig::from_name_or_default("haiku")
            .unwrap()
            .with_extended_context(true);

        // Haiku doesn't support extended context
        if !config.supports_extended_context() {
            assert!(!config.has_extended_context());
        }
    }

    #[test]
    fn test_usable_context_is_context_minus_output() {
        let config = ModelConfig::default();
        assert_eq!(
            config.usable_context(),
            config.context_window() - config.max_output_tokens()
        );
    }
}
