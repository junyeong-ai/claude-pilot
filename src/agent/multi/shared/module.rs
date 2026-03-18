use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    #[default]
    ReadOnly,
    FileModify,
    VerifyExecute,
}

impl PermissionProfile {
    pub fn can_modify_files(&self) -> bool {
        matches!(self, Self::FileModify | Self::VerifyExecute)
    }

    pub fn can_execute(&self) -> bool {
        matches!(self, Self::VerifyExecute)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualifiedModule {
    pub workspace: String,
    pub domain: Option<String>,
    pub module: String,
}

impl QualifiedModule {
    pub fn new(workspace: impl Into<String>, module: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            domain: None,
            module: module.into(),
        }
    }

    pub fn with_domain(
        workspace: impl Into<String>,
        domain: impl Into<String>,
        module: impl Into<String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            domain: Some(domain.into()),
            module: module.into(),
        }
    }

    pub fn to_qualified_string(&self) -> String {
        match &self.domain {
            Some(d) => format!("{}::{}::{}", self.workspace, d, self.module),
            None => format!("{}::{}", self.workspace, self.module),
        }
    }
}

impl FromStr for QualifiedModule {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split("::").collect();
        match parts.len() {
            2 => Ok(Self::new(parts[0], parts[1])),
            3 => Ok(Self::with_domain(parts[0], parts[1], parts[2])),
            _ => Err(format!("invalid qualified module format: {}", s)),
        }
    }
}

impl fmt::Display for QualifiedModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_qualified_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qualified_module() {
        let qm = QualifiedModule::new("project-a", "auth");
        assert_eq!(qm.to_qualified_string(), "project-a::auth");

        let qm2 = QualifiedModule::with_domain("project-a", "security", "auth");
        assert_eq!(qm2.to_qualified_string(), "project-a::security::auth");

        let parsed: QualifiedModule = "ws::mod".parse().unwrap();
        assert_eq!(parsed.workspace, "ws");
        assert_eq!(parsed.module, "mod");
        assert!(parsed.domain.is_none());

        let parsed2: QualifiedModule = "ws::dom::mod".parse().unwrap();
        assert_eq!(parsed2.domain.as_deref(), Some("dom"));
    }
}
