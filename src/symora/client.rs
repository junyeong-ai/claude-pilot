use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

use super::types::*;
use crate::error::{PilotError, Result};

pub struct SymoraClient {
    working_dir: std::path::PathBuf,
    timeout_secs: u64,
}

impl SymoraClient {
    pub fn new(working_dir: &Path, timeout_secs: u64) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
            timeout_secs,
        }
    }

    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    async fn run_command(&self, args: &[&str]) -> Result<String> {
        debug!(args = ?args, "Running symora command");

        let future = Command::new("symora")
            .args(args)
            .current_dir(&self.working_dir)
            .output();

        let output = timeout(Duration::from_secs(self.timeout_secs), future)
            .await
            .map_err(|_| PilotError::Timeout(format!("symora {:?}", args)))?
            .map_err(|e| PilotError::AgentExecution(format!("symora execution failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                debug!(stderr = %stderr, "symora stderr");
            }
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn parse_json<T: serde::de::DeserializeOwned>(&self, json: &str) -> Result<T> {
        serde_json::from_str(json)
            .map_err(|e| PilotError::Serialization(format!("Failed to parse symora output: {}", e)))
    }

    pub async fn find_symbols(
        &self,
        file_or_name: &str,
        kind: Option<SymbolKind>,
        language: Option<Language>,
    ) -> Result<SymbolSearchResult> {
        let mut args = vec!["find", "symbol", file_or_name];
        let kind_str;
        let lang_str;

        if let Some(k) = kind {
            kind_str = k.as_str().to_string();
            args.extend(["--kind", &kind_str]);
        }

        if let Some(l) = language {
            lang_str = l.as_str().to_string();
            args.extend(["--lang", &lang_str]);
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(SymbolSearchResult {
                symbols: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn find_symbols_by_name(
        &self,
        name: &str,
        language: Option<Language>,
    ) -> Result<SymbolSearchResult> {
        let mut args = vec!["find", "symbol", "--name", name];
        let lang_str;

        if let Some(l) = language {
            lang_str = l.as_str().to_string();
            args.extend(["--lang", &lang_str]);
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(SymbolSearchResult {
                symbols: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn find_definition(&self, location: &str) -> Result<DefinitionResult> {
        let args = ["find", "def", location];
        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(DefinitionResult { definition: None });
        }
        self.parse_json(&output)
    }

    pub async fn find_references(
        &self,
        location: &str,
        with_snippet: bool,
    ) -> Result<ReferencesResult> {
        let mut args = vec!["find", "refs", location];
        if with_snippet {
            args.push("--with-snippet");
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(ReferencesResult {
                references: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn incoming_calls(&self, location: &str) -> Result<CallsResult> {
        let args = ["calls", "incoming", location];
        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(CallsResult {
                calls: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn outgoing_calls(&self, location: &str) -> Result<CallsResult> {
        let args = ["calls", "outgoing", location];
        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(CallsResult {
                calls: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn hover(&self, location: &str) -> Result<HoverResult> {
        let args = ["hover", location];
        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(HoverResult {
                content: String::new(),
                language: None,
            });
        }
        self.parse_json(&output)
    }

    pub async fn context(
        &self,
        location: &str,
        callers: bool,
        callees: bool,
        types: bool,
        tests: bool,
    ) -> Result<ContextResult> {
        let mut args = vec!["context", location];

        if callers && callees && types && tests {
            args.push("--all");
        } else {
            if callers {
                args.push("--callers");
            }
            if callees {
                args.push("--callees");
            }
            if types {
                args.push("--types");
            }
            if tests {
                args.push("--tests");
            }
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(ContextResult {
                callers: vec![],
                callees: vec![],
                types: vec![],
                tests: vec![],
            });
        }
        self.parse_json(&output)
    }

    pub async fn usage(
        &self,
        pattern: &str,
        language: Language,
        with_metrics: bool,
        filter: Option<&str>,
    ) -> Result<UsageResult> {
        let lang_str = language.as_str();
        let mut args = vec!["usage", pattern, "--lang", lang_str];

        if with_metrics {
            args.push("--with-metrics");
        }

        if let Some(f) = filter {
            args.extend(["--filter", f]);
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(UsageResult {
                results: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn search_ast(
        &self,
        pattern: &str,
        language: Language,
        path: Option<&str>,
    ) -> Result<AstSearchResult> {
        let lang_str = language.as_str();
        let mut args = vec!["search", "ast", pattern, "--lang", lang_str];

        if let Some(p) = path {
            args.extend(["--path", p]);
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(AstSearchResult {
                matches: vec![],
                count: 0,
            });
        }
        self.parse_json(&output)
    }

    pub async fn diagnostics(
        &self,
        file: &str,
        with_context: bool,
        with_suggestions: bool,
    ) -> Result<DiagnosticsResult> {
        let mut args = vec!["diagnostics", file];

        if with_context {
            args.push("--with-context");
        }
        if with_suggestions {
            args.push("--with-suggestions");
        }

        let output = self.run_command(&args).await?;
        if output.trim().is_empty() {
            return Ok(DiagnosticsResult {
                diagnostics: vec![],
            });
        }
        self.parse_json(&output)
    }

    pub async fn is_available(&self) -> bool {
        match Command::new("symora").arg("--version").output().await {
            Ok(output) => output.status.success(),
            Err(_) => {
                warn!("symora is not available");
                false
            }
        }
    }

    pub async fn build_search_index(&self, languages: Option<&[Language]>) -> Result<()> {
        let mut args = vec!["search", "index", "build"];
        let langs_str;

        if let Some(langs) = languages {
            langs_str = langs
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(",");
            args.extend(["--lang", &langs_str]);
        }

        self.run_command(&args).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_symbol_location() {
        let symbol = Symbol {
            name: "test".to_string(),
            kind: SymbolKind::Function,
            file: "src/main.rs".into(),
            line: 10,
            column: 5,
            container: None,
        };
        assert_eq!(symbol.location(), "src/main.rs:10:5");
    }
}
