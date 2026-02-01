//! Unified agent identity system with workspace namespacing.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleType {
    Research,
    Planning,
    Coder,
    Verifier,
    Reviewer,
    Architect,
    Module(String),
    GroupCoordinator(String),
    DomainCoordinator(String),
    WorkspaceCoordinator,
    CrossWorkspaceCoordinator,
}

impl RoleType {
    pub fn key(&self) -> String {
        match self {
            Self::Research => "research".into(),
            Self::Planning => "planning".into(),
            Self::Coder => "coder".into(),
            Self::Verifier => "verifier".into(),
            Self::Reviewer => "reviewer".into(),
            Self::Architect => "architect".into(),
            Self::Module(name) => format!("module:{}", name),
            Self::GroupCoordinator(id) => format!("group:{}", id),
            Self::DomainCoordinator(id) => format!("domain:{}", id),
            Self::WorkspaceCoordinator => "ws-coordinator".into(),
            Self::CrossWorkspaceCoordinator => "cross-ws-coordinator".into(),
        }
    }

    pub fn is_planning_participant(&self) -> bool {
        matches!(
            self,
            Self::Research | Self::Planning | Self::Architect | Self::Module(_)
        )
    }

    pub fn is_executor(&self) -> bool {
        matches!(self, Self::Coder | Self::Verifier | Self::Reviewer)
    }

    pub fn is_coordinator(&self) -> bool {
        matches!(
            self,
            Self::GroupCoordinator(_)
                | Self::DomainCoordinator(_)
                | Self::WorkspaceCoordinator
                | Self::CrossWorkspaceCoordinator
        )
    }

    pub fn is_module(&self) -> bool {
        matches!(self, Self::Module(_))
    }

    pub fn module_name(&self) -> Option<&str> {
        match self {
            Self::Module(name) => Some(name),
            _ => None,
        }
    }
}

impl fmt::Display for RoleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentIdentifier {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub role: RoleType,
    pub instance: u32,
}

impl AgentIdentifier {
    pub fn new(role: RoleType, instance: u32) -> Self {
        Self {
            workspace: None,
            module: None,
            role,
            instance,
        }
    }

    pub fn in_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn in_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    pub fn qualified_id(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref ws) = self.workspace {
            parts.push(ws.clone());
        }
        if let Some(ref m) = self.module {
            parts.push(m.clone());
        }
        parts.push(format!("{}-{}", self.role.key(), self.instance));
        parts.join(":")
    }

    pub fn pool_key(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref ws) = self.workspace {
            parts.push(ws.clone());
        }
        if let Some(ref m) = self.module {
            parts.push(m.clone());
        }
        parts.push(self.role.key());
        parts.join(":")
    }

    pub fn research(instance: u32) -> Self {
        Self::new(RoleType::Research, instance)
    }

    pub fn planning(instance: u32) -> Self {
        Self::new(RoleType::Planning, instance)
    }

    pub fn coder(instance: u32) -> Self {
        Self::new(RoleType::Coder, instance)
    }

    pub fn verifier(instance: u32) -> Self {
        Self::new(RoleType::Verifier, instance)
    }

    pub fn reviewer(instance: u32) -> Self {
        Self::new(RoleType::Reviewer, instance)
    }

    pub fn architect(instance: u32) -> Self {
        Self::new(RoleType::Architect, instance)
    }

    pub fn module_agent(module: impl Into<String>, instance: u32) -> Self {
        Self::new(RoleType::Module(module.into()), instance)
    }

    pub fn module_planning(workspace: &str, module: &str, instance: u32) -> Self {
        Self::planning(instance)
            .in_workspace(workspace)
            .in_module(module)
    }

    pub fn module_coder(workspace: &str, module: &str, instance: u32) -> Self {
        Self::coder(instance)
            .in_workspace(workspace)
            .in_module(module)
    }

    pub fn group_coordinator(group_id: impl Into<String>, instance: u32) -> Self {
        Self::new(RoleType::GroupCoordinator(group_id.into()), instance)
    }

    pub fn domain_coordinator(domain_id: impl Into<String>, instance: u32) -> Self {
        Self::new(RoleType::DomainCoordinator(domain_id.into()), instance)
    }

    pub fn workspace_coordinator(workspace: &str, instance: u32) -> Self {
        Self::new(RoleType::WorkspaceCoordinator, instance).in_workspace(workspace)
    }

    pub fn cross_workspace_coordinator(instance: u32) -> Self {
        Self::new(RoleType::CrossWorkspaceCoordinator, instance)
    }

    pub fn matches_role(&self, role: &RoleType) -> bool {
        &self.role == role
    }

    pub fn is_in_workspace(&self, ws: &str) -> bool {
        self.workspace.as_deref() == Some(ws)
    }

    pub fn is_in_module(&self, module: &str) -> bool {
        self.module.as_deref() == Some(module)
    }
}

impl Default for AgentIdentifier {
    fn default() -> Self {
        Self::coder(0)
    }
}

impl fmt::Display for AgentIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.qualified_id())
    }
}

impl From<&str> for AgentIdentifier {
    fn from(s: &str) -> Self {
        let parts: Vec<&str> = s.split(':').collect();
        match parts.as_slice() {
            [role_instance] => Self::parse_role_instance(role_instance),
            [module, role_instance] => Self::parse_role_instance(role_instance).in_module(*module),
            [ws, module, role_instance] => Self::parse_role_instance(role_instance)
                .in_workspace(*ws)
                .in_module(*module),
            _ => Self::default(),
        }
    }
}

impl AgentIdentifier {
    fn parse_role_instance(s: &str) -> Self {
        if let Some(pos) = s.rfind('-') {
            let role_str = &s[..pos];
            let instance = s[pos + 1..].parse().unwrap_or(0);
            let role = match role_str {
                "research" => RoleType::Research,
                "planning" => RoleType::Planning,
                "coder" => RoleType::Coder,
                "verifier" => RoleType::Verifier,
                "reviewer" => RoleType::Reviewer,
                "architect" => RoleType::Architect,
                "ws-coordinator" => RoleType::WorkspaceCoordinator,
                "cross-ws-coordinator" => RoleType::CrossWorkspaceCoordinator,
                s if s.starts_with("module:") => RoleType::Module(s[7..].to_string()),
                s if s.starts_with("group:") => RoleType::GroupCoordinator(s[6..].to_string()),
                s if s.starts_with("domain:") => RoleType::DomainCoordinator(s[7..].to_string()),
                _ => RoleType::Coder,
            };
            Self::new(role, instance)
        } else {
            Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qualified_id() {
        assert_eq!(AgentIdentifier::planning(0).qualified_id(), "planning-0");
        assert_eq!(
            AgentIdentifier::planning(1)
                .in_workspace("project-a")
                .qualified_id(),
            "project-a:planning-1"
        );
        assert_eq!(
            AgentIdentifier::module_planning("ws-a", "auth", 0).qualified_id(),
            "ws-a:auth:planning-0"
        );
    }

    #[test]
    fn test_pool_key() {
        assert_eq!(AgentIdentifier::planning(0).pool_key(), "planning");
        assert_eq!(
            AgentIdentifier::planning(1)
                .in_workspace("project-a")
                .pool_key(),
            "project-a:planning"
        );
        assert_eq!(
            AgentIdentifier::module_planning("ws-a", "auth", 0).pool_key(),
            "ws-a:auth:planning"
        );
    }

    #[test]
    fn test_role_type_key() {
        assert_eq!(RoleType::Planning.key(), "planning");
        assert_eq!(RoleType::Module("auth".into()).key(), "module:auth");
        assert_eq!(
            RoleType::GroupCoordinator("core".into()).key(),
            "group:core"
        );
    }

    #[test]
    fn test_from_str() {
        let id: AgentIdentifier = "planning-0".into();
        assert_eq!(id.role, RoleType::Planning);
        assert_eq!(id.instance, 0);

        let id: AgentIdentifier = "ws-a:auth:coder-2".into();
        assert_eq!(id.workspace, Some("ws-a".into()));
        assert_eq!(id.module, Some("auth".into()));
        assert_eq!(id.role, RoleType::Coder);
        assert_eq!(id.instance, 2);
    }

    #[test]
    fn test_role_predicates() {
        assert!(RoleType::Planning.is_planning_participant());
        assert!(RoleType::Research.is_planning_participant());
        assert!(!RoleType::Coder.is_planning_participant());

        assert!(RoleType::Coder.is_executor());
        assert!(!RoleType::Planning.is_executor());

        assert!(RoleType::GroupCoordinator("x".into()).is_coordinator());
        assert!(!RoleType::Coder.is_coordinator());
    }
}
