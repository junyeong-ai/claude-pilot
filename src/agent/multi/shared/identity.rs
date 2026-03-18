use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn research(instance: u32) -> Self {
        Self(format!("research-{instance}"))
    }

    pub fn planning(instance: u32) -> Self {
        Self(format!("planning-{instance}"))
    }

    pub fn coder(instance: u32) -> Self {
        Self(format!("coder-{instance}"))
    }

    pub fn verifier(instance: u32) -> Self {
        Self(format!("verifier-{instance}"))
    }

    pub fn reviewer(instance: u32) -> Self {
        Self(format!("reviewer-{instance}"))
    }

    pub fn architect(instance: u32) -> Self {
        Self(format!("architect-{instance}"))
    }

    pub fn module(module_id: &str) -> Self {
        Self(format!("module-{module_id}"))
    }

    pub fn module_with_instance(name: &str, instance: u32) -> Self {
        Self(format!("module-{}-{}", name.to_lowercase(), instance))
    }

    pub fn core(role: &str, instance: u32) -> Self {
        Self(format!("{role}-{instance}"))
    }

    pub fn group_coordinator(group_id: &str) -> Self {
        Self(format!("group-{group_id}"))
    }

    pub fn domain_coordinator(domain_id: &str) -> Self {
        Self(format!("domain-{domain_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleCategory {
    Core,
    Module,
    Reviewer,
    Architecture,
    Advisor,
}

impl fmt::Display for RoleCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Module => write!(f, "module"),
            Self::Reviewer => write!(f, "reviewer"),
            Self::Architecture => write!(f, "architecture"),
            Self::Advisor => write!(f, "advisor"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_formats() {
        assert_eq!(AgentId::module("auth").as_str(), "module-auth");
        assert_eq!(
            AgentId::module_with_instance("Auth", 0).as_str(),
            "module-auth-0"
        );
        assert_eq!(
            AgentId::module_with_instance("user-service", 1).as_str(),
            "module-user-service-1"
        );
        assert_eq!(AgentId::group_coordinator("core").as_str(), "group-core");
        assert_eq!(
            AgentId::domain_coordinator("security").as_str(),
            "domain-security"
        );
        assert_eq!(AgentId::research(0).as_str(), "research-0");
        assert_eq!(AgentId::reviewer(0).as_str(), "reviewer-0");
        assert_eq!(AgentId::architect(0).as_str(), "architect-0");
        assert_eq!(AgentId::core("planning", 0).as_str(), "planning-0");
        let id: String = AgentId::coder(1).into();
        assert_eq!(id, "coder-1");
    }
}
