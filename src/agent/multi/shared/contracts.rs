use super::module::QualifiedModule;
use crate::domain::Severity;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiChange {
    pub api_name: String,
    pub change_type: ApiChangeType,
    pub old_signature: Option<String>,
    pub new_signature: Option<String>,
    pub module: Option<QualifiedModule>,
}

impl ApiChange {
    pub fn add(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            api_name: name.into(),
            change_type: ApiChangeType::Add,
            old_signature: None,
            new_signature: Some(signature.into()),
            module: None,
        }
    }

    pub fn remove(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            api_name: name.into(),
            change_type: ApiChangeType::Remove,
            old_signature: Some(signature.into()),
            new_signature: None,
            module: None,
        }
    }

    pub fn rename(old_name: impl Into<String>, new_name: impl Into<String>) -> Self {
        Self {
            api_name: new_name.into(),
            change_type: ApiChangeType::Rename {
                old_name: old_name.into(),
            },
            old_signature: None,
            new_signature: None,
            module: None,
        }
    }

    pub fn modify(
        name: impl Into<String>,
        old_sig: impl Into<String>,
        new_sig: impl Into<String>,
    ) -> Self {
        Self {
            api_name: name.into(),
            change_type: ApiChangeType::ModifySignature,
            old_signature: Some(old_sig.into()),
            new_signature: Some(new_sig.into()),
            module: None,
        }
    }

    pub fn with_module(mut self, module: QualifiedModule) -> Self {
        self.module = Some(module);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApiChangeType {
    Add,
    Remove,
    Rename { old_name: String },
    ModifySignature,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeChange {
    pub type_name: String,
    pub change_type: TypeChangeType,
    pub details: String,
    pub module: Option<QualifiedModule>,
}

impl TypeChange {
    pub fn add_field(type_name: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            change_type: TypeChangeType::AddField,
            details: field.into(),
            module: None,
        }
    }

    pub fn remove_field(type_name: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            change_type: TypeChangeType::RemoveField,
            details: field.into(),
            module: None,
        }
    }

    pub fn change_field_type(type_name: impl Into<String>, change: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            change_type: TypeChangeType::ChangeFieldType,
            details: change.into(),
            module: None,
        }
    }

    pub fn with_module(mut self, module: QualifiedModule) -> Self {
        self.module = Some(module);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TypeChangeType {
    AddField,
    RemoveField,
    ChangeFieldType,
    RenameField { old_name: String },
    AddVariant,
    RemoveVariant,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConstraints {
    pub tech_decisions: Vec<TechDecision>,
    pub api_contracts: Vec<ApiContract>,
    pub workspace_refinements: HashMap<String, Vec<String>>,
    pub runtime_constraints: Vec<RuntimeConstraint>,
}

impl GlobalConstraints {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_tech_decision(&mut self, decision: TechDecision) {
        self.tech_decisions.push(decision);
    }

    pub fn add_api_contract(&mut self, contract: ApiContract) {
        self.api_contracts.push(contract);
    }

    pub fn add_workspace_refinement(
        &mut self,
        workspace: impl Into<String>,
        refinement: impl Into<String>,
    ) {
        self.workspace_refinements
            .entry(workspace.into())
            .or_default()
            .push(refinement.into());
    }

    #[cfg(test)]
    pub fn refinements(&self, workspace: &str) -> Vec<&str> {
        self.workspace_refinements
            .get(workspace)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechDecision {
    pub topic: String,
    pub decision: String,
    pub rationale: String,
    pub affected_modules: Vec<QualifiedModule>,
}

impl TechDecision {
    #[cfg(test)]
    pub fn new(topic: impl Into<String>, decision: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            decision: decision.into(),
            rationale: String::new(),
            affected_modules: Vec::new(),
        }
    }

}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiContract {
    pub name: String,
    pub provider: QualifiedModule,
    pub consumers: Vec<QualifiedModule>,
    pub specification: String,
}

impl ApiContract {
    pub fn new(name: impl Into<String>, provider: QualifiedModule) -> Self {
        Self {
            name: name.into(),
            provider,
            consumers: Vec::new(),
            specification: String::new(),
        }
    }

    pub fn with_consumers(mut self, consumers: Vec<QualifiedModule>) -> Self {
        self.consumers = consumers;
        self
    }

    pub fn with_specification(mut self, spec: impl Into<String>) -> Self {
        self.specification = spec.into();
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConstraint {
    pub source: String,
    pub constraint_type: ConstraintType,
    pub description: String,
    pub affected_modules: Vec<QualifiedModule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintType {
    ApiNaming,
    TypeDefinition,
    DependencyOrder,
    TechnologyChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConflict {
    pub conflict_type: SemanticConflictType,
    pub agents_involved: Vec<String>,
    pub description: String,
    pub severity: Severity,
}

impl SemanticConflict {
    pub fn api_rename(
        old_name: impl Into<String>,
        new_names: Vec<(String, String)>,
    ) -> Self {
        let agents: Vec<String> = new_names.iter().map(|(a, _)| a.clone()).collect();
        let names: Vec<&str> = new_names.iter().map(|(_, n)| n.as_str()).collect();
        Self {
            conflict_type: SemanticConflictType::ApiRename {
                old_name: old_name.into(),
                proposed_names: names.iter().map(|s| s.to_string()).collect(),
            },
            agents_involved: agents,
            description: format!("Conflicting API rename proposals: {:?}", names),
            severity: Severity::Error,
        }
    }

    pub fn type_change(type_name: impl Into<String>, changes: Vec<(String, String)>) -> Self {
        let agents: Vec<String> = changes.iter().map(|(a, _)| a.clone()).collect();
        Self {
            conflict_type: SemanticConflictType::TypeChange {
                type_name: type_name.into(),
                changes: changes.into_iter().map(|(_, c)| c).collect(),
            },
            agents_involved: agents,
            description: "Conflicting type modifications".to_string(),
            severity: Severity::Error,
        }
    }

    pub fn dependency_mismatch(provider: impl Into<String>, consumer: impl Into<String>) -> Self {
        Self {
            conflict_type: SemanticConflictType::DependencyMismatch {
                provider: provider.into(),
                consumer: consumer.into(),
            },
            agents_involved: Vec::new(),
            description: "Provider/consumer dependency mismatch".to_string(),
            severity: Severity::Critical,
        }
    }

    pub fn is_blocking(&self) -> bool {
        self.severity == Severity::Critical
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticConflictType {
    ApiRename {
        old_name: String,
        proposed_names: Vec<String>,
    },
    TypeChange {
        type_name: String,
        changes: Vec<String>,
    },
    DependencyMismatch {
        provider: String,
        consumer: String,
    },
    TechDecisionConflict {
        topic: String,
        decisions: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub enum ConvergenceCheckResult {
    Converged {
        score: f64,
        minor_conflicts: Vec<SemanticConflict>,
    },
    Blocked {
        conflicts: Vec<SemanticConflict>,
    },
    NeedsMoreRounds {
        current_score: f64,
        conflicts: Vec<SemanticConflict>,
    },
}

impl ConvergenceCheckResult {
    pub fn is_converged(&self) -> bool {
        matches!(self, Self::Converged { .. })
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    pub fn conflicts(&self) -> &[SemanticConflict] {
        match self {
            Self::Converged {
                minor_conflicts, ..
            } => minor_conflicts,
            Self::Blocked { conflicts } | Self::NeedsMoreRounds { conflicts, .. } => conflicts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_change() {
        let add = ApiChange::add("validate", "fn validate(token: &str) -> bool");
        assert_eq!(add.change_type, ApiChangeType::Add);
        assert!(add.new_signature.is_some());

        let rename = ApiChange::rename("old_name", "new_name");
        assert!(matches!(rename.change_type, ApiChangeType::Rename { .. }));
    }

    #[test]
    fn test_global_constraints() {
        let mut gc = GlobalConstraints::new();
        gc.add_tech_decision(TechDecision::new("auth", "Use JWT"));
        gc.add_workspace_refinement("ws-a", "Frontend uses cookies");

        assert_eq!(gc.tech_decisions.len(), 1);
        assert_eq!(gc.refinements("ws-a").len(), 1);
        assert!(gc.refinements("ws-b").is_empty());
    }

    #[test]
    fn test_semantic_conflict() {
        let conflict = SemanticConflict::api_rename(
            "validate",
            vec![
                ("agent-1".to_string(), "verify".to_string()),
                ("agent-2".to_string(), "check".to_string()),
            ],
        );
        assert_eq!(conflict.agents_involved.len(), 2);
        assert!(!conflict.is_blocking());

        let blocking = SemanticConflict::dependency_mismatch("provider", "consumer");
        assert!(blocking.is_blocking());
    }

    #[test]
    fn test_convergence_check_result() {
        let converged = ConvergenceCheckResult::Converged {
            score: 0.9,
            minor_conflicts: vec![],
        };
        assert!(converged.is_converged());
        assert!(!converged.is_blocked());

        let blocked = ConvergenceCheckResult::Blocked {
            conflicts: vec![SemanticConflict::dependency_mismatch("a", "b")],
        };
        assert!(!blocked.is_converged());
        assert!(blocked.is_blocked());
        assert_eq!(blocked.conflicts().len(), 1);
    }
}
