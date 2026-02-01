use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Common trait for types that have a file location.
pub trait HasLocation {
    fn file(&self) -> &PathBuf;
    fn line(&self) -> u32;
    fn column(&self) -> u32;

    fn location(&self) -> String {
        format!(
            "{}:{}:{}",
            self.file().display(),
            self.line(),
            self.column()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    #[serde(default)]
    pub container: Option<String>,
}

impl HasLocation for Symbol {
    fn file(&self) -> &PathBuf {
        &self.file
    }
    fn line(&self) -> u32 {
        self.line
    }
    fn column(&self) -> u32 {
        self.column
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Constant,
    Variable,
    Field,
    Module,
    Property,
    #[serde(other)]
    Other,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Constant => "constant",
            Self::Variable => "variable",
            Self::Field => "field",
            Self::Module => "module",
            Self::Property => "property",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolSearchResult {
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    #[serde(default)]
    pub snippet: Option<String>,
}

impl HasLocation for Reference {
    fn file(&self) -> &PathBuf {
        &self.file
    }
    fn line(&self) -> u32 {
        self.line
    }
    fn column(&self) -> u32 {
        self.column
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencesResult {
    #[serde(default)]
    pub references: Vec<Reference>,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyItem {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    #[serde(default)]
    pub kind: Option<String>,
}

impl HasLocation for CallHierarchyItem {
    fn file(&self) -> &PathBuf {
        &self.file
    }
    fn line(&self) -> u32 {
        self.line
    }
    fn column(&self) -> u32 {
        self.column
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallsResult {
    #[serde(default)]
    pub calls: Vec<CallHierarchyItem>,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoverResult {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Definition {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

impl HasLocation for Definition {
    fn file(&self) -> &PathBuf {
        &self.file
    }
    fn line(&self) -> u32 {
        self.line
    }
    fn column(&self) -> u32 {
        self.column
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionResult {
    pub definition: Option<Definition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResult {
    #[serde(default)]
    pub callers: Vec<CallHierarchyItem>,
    #[serde(default)]
    pub callees: Vec<CallHierarchyItem>,
    #[serde(default)]
    pub types: Vec<Symbol>,
    #[serde(default)]
    pub tests: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageItem {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    #[serde(default)]
    pub metrics: Option<UsageMetrics>,
    #[serde(default)]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageMetrics {
    #[serde(default)]
    pub reference_count: usize,
    #[serde(default)]
    pub has_tests: bool,
    #[serde(default)]
    pub has_docs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageResult {
    #[serde(default)]
    pub results: Vec<UsageItem>,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstMatch {
    pub file: PathBuf,
    pub line: u32,
    pub text: String,
    #[serde(default)]
    pub node_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstSearchResult {
    #[serde(default)]
    pub matches: Vec<AstMatch>,
    #[serde(default)]
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsResult {
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Go,
    Java,
    TypeScript,
    JavaScript,
    Kotlin,
    Python,
    Php,
    Cpp,
    CSharp,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Kotlin => "kotlin",
            Self::Python => "python",
            Self::Php => "php",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" => Some(Self::JavaScript),
            "kt" | "kts" => Some(Self::Kotlin),
            "py" => Some(Self::Python),
            "php" => Some(Self::Php),
            "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" => Some(Self::Cpp),
            "cs" => Some(Self::CSharp),
            _ => None,
        }
    }

    /// Detect the primary language of a project from its root directory.
    pub fn detect_from_project(root: &std::path::Path) -> Option<Self> {
        if root.join("Cargo.toml").exists() {
            return Some(Self::Rust);
        }
        if root.join("go.mod").exists() {
            return Some(Self::Go);
        }
        if root.join("tsconfig.json").exists() {
            return Some(Self::TypeScript);
        }
        if root.join("package.json").exists() {
            return Some(Self::JavaScript);
        }
        if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
            return Some(Self::Python);
        }
        if root.join("build.gradle.kts").exists() {
            return Some(Self::Kotlin);
        }
        if root.join("pom.xml").exists() || root.join("build.gradle").exists() {
            return Some(Self::Java);
        }
        None
    }
}
