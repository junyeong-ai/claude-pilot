//! Context composition for agent execution.
//!
//! Combines Rules (WHAT), Skills (HOW), and Personas (WHO) into
//! a unified context for agent task execution.

use std::path::PathBuf;

use super::persona::{AgentPersona, PersonaLoader};
use crate::agent::multi::rules::{ResolvedRules, RuleRegistry, RuleResolver};
use crate::agent::multi::skills::{Skill, SkillComposer, SkillRegistry, SkillType};
use crate::agent::multi::traits::{AgentRole, AgentTask};

#[derive(Debug, Clone)]
pub struct ComposedContext {
    pub system_prompt: String,
    pub injected_rules: ResolvedRules,
    pub active_skill: Option<SkillType>,
    pub persona_name: Option<String>,
}

impl ComposedContext {
    pub fn empty() -> Self {
        Self {
            system_prompt: String::new(),
            injected_rules: ResolvedRules::new(),
            active_skill: None,
            persona_name: None,
        }
    }

    pub fn rule_count(&self) -> usize {
        self.injected_rules.len()
    }

    pub fn rule_categories(&self) -> Vec<String> {
        self.injected_rules
            .rules()
            .iter()
            .map(|r| format!("{:?}", r.category()))
            .collect()
    }
}

pub struct ContextComposer<'a> {
    rule_registry: &'a RuleRegistry,
    skill_registry: &'a SkillRegistry,
    persona_loader: &'a PersonaLoader,
}

impl<'a> ContextComposer<'a> {
    pub fn new(
        rule_registry: &'a RuleRegistry,
        skill_registry: &'a SkillRegistry,
        persona_loader: &'a PersonaLoader,
    ) -> Self {
        Self {
            rule_registry,
            skill_registry,
            persona_loader,
        }
    }

    pub fn compose(
        &self,
        role: &AgentRole,
        task: &AgentTask,
        affected_files: &[PathBuf],
    ) -> ComposedContext {
        let persona = self.persona_loader.get_by_role(&role.id);
        let skill_type = self.select_skill(role, task, persona);
        let skill = skill_type.map(|t| self.skill_registry.get(t));

        let resolver = RuleResolver::new(self.rule_registry);
        let rules = resolver.resolve(affected_files, &task.description);

        let system_prompt = self.build_prompt(persona, skill, &rules);

        ComposedContext {
            system_prompt,
            injected_rules: rules,
            active_skill: skill_type,
            persona_name: persona.map(|p| p.name.clone()),
        }
    }

    pub fn compose_for_skill(
        &self,
        skill_type: SkillType,
        affected_files: &[PathBuf],
        task_content: &str,
    ) -> ComposedContext {
        let skill = self.skill_registry.get(skill_type);
        let resolver = RuleResolver::new(self.rule_registry);
        let rules = resolver.resolve(affected_files, task_content);

        ComposedContext {
            system_prompt: SkillComposer::compose(skill, &rules),
            injected_rules: rules,
            active_skill: Some(skill_type),
            persona_name: None,
        }
    }

    fn select_skill(
        &self,
        role: &AgentRole,
        task: &AgentTask,
        persona: Option<&AgentPersona>,
    ) -> Option<SkillType> {
        if let Some(p) = persona
            && !p.skills.is_empty()
        {
            return SkillType::from_name(&p.skills[0]);
        }

        let desc_lower = task.description.to_lowercase();

        match role.id.as_str() {
            "reviewer" => Some(SkillType::CodeReview),
            "architect" | "planning" => Some(SkillType::Plan),
            "coder" => {
                if desc_lower.contains("debug") || desc_lower.contains("fix bug") {
                    Some(SkillType::Debug)
                } else if desc_lower.contains("refactor") || desc_lower.contains("restructure") {
                    Some(SkillType::Refactor)
                } else {
                    Some(SkillType::Implement)
                }
            }
            _ => None,
        }
    }

    fn build_prompt(
        &self,
        persona: Option<&AgentPersona>,
        skill: Option<&Skill>,
        rules: &ResolvedRules,
    ) -> String {
        let capacity = persona.map(|p| p.system_prompt.len()).unwrap_or(0)
            + skill.map(|s| s.methodology.len()).unwrap_or(0)
            + rules.rules().iter().map(|r| r.content.len()).sum::<usize>()
            + 256;

        let mut prompt = String::with_capacity(capacity);

        if let Some(p) = persona {
            prompt.push_str(&p.system_prompt);
            if skill.is_some() || !rules.is_empty() {
                prompt.push_str("\n\n---\n\n");
            }
        }

        if let Some(s) = skill {
            prompt.push_str(&s.methodology);
            if !rules.is_empty() {
                prompt.push_str("\n\n---\n\n");
            }
        }

        if !rules.is_empty() {
            prompt.push_str(&rules.to_prompt());
        }

        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::rules::{InjectionConfig, Rule, RuleCategory, RuleMetadata};
    use crate::agent::multi::traits::{RoleCategory, TaskContext};

    fn create_test_registries() -> (RuleRegistry, SkillRegistry, PersonaLoader) {
        (
            RuleRegistry::new(PathBuf::new()),
            SkillRegistry::default(),
            PersonaLoader::default(),
        )
    }

    fn create_test_task() -> AgentTask {
        AgentTask {
            id: "task-1".into(),
            description: "Implement new feature".into(),
            context: TaskContext::default(),
            priority: Default::default(),
            role: None,
        }
    }

    #[test]
    fn test_compose_empty() {
        let (rules, skills, personas) = create_test_registries();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "unknown".into(),
            category: RoleCategory::Core,
        };
        let task = create_test_task();

        let ctx = composer.compose(&role, &task, &[]);
        assert!(ctx.system_prompt.is_empty());
        assert!(ctx.active_skill.is_none());
    }

    #[test]
    fn test_select_skill_for_coder() {
        let (rules, skills, personas) = create_test_registries();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "coder".into(),
            category: RoleCategory::Core,
        };

        let implement_task = AgentTask {
            id: "t1".into(),
            description: "Add user authentication".into(),
            context: TaskContext::default(),
            priority: Default::default(),
            role: None,
        };
        assert_eq!(
            composer.select_skill(&role, &implement_task, None),
            Some(SkillType::Implement)
        );

        let debug_task = AgentTask {
            id: "t2".into(),
            description: "Debug the login issue".into(),
            context: TaskContext::default(),
            priority: Default::default(),
            role: None,
        };
        assert_eq!(
            composer.select_skill(&role, &debug_task, None),
            Some(SkillType::Debug)
        );

        let refactor_task = AgentTask {
            id: "t3".into(),
            description: "Refactor authentication module".into(),
            context: TaskContext::default(),
            priority: Default::default(),
            role: None,
        };
        assert_eq!(
            composer.select_skill(&role, &refactor_task, None),
            Some(SkillType::Refactor)
        );
    }

    #[test]
    fn test_select_skill_for_reviewer() {
        let (rules, skills, personas) = create_test_registries();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "reviewer".into(),
            category: RoleCategory::Reviewer,
        };
        let task = create_test_task();

        assert_eq!(
            composer.select_skill(&role, &task, None),
            Some(SkillType::CodeReview)
        );
    }

    #[test]
    fn test_compose_for_skill() {
        let (rules, skills, personas) = create_test_registries();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let ctx = composer.compose_for_skill(SkillType::CodeReview, &[], "review code");

        assert!(ctx.system_prompt.contains("Code Review"));
        assert_eq!(ctx.active_skill, Some(SkillType::CodeReview));
    }

    #[test]
    fn test_composed_context_helpers() {
        let mut rules = ResolvedRules::new();
        rules.add(Rule::new(
            RuleMetadata::new("project", RuleCategory::Project)
                .with_injection(InjectionConfig::always()),
            "Project rules".into(),
            PathBuf::new(),
        ));

        let ctx = ComposedContext {
            system_prompt: "test".into(),
            injected_rules: rules,
            active_skill: Some(SkillType::Implement),
            persona_name: Some("coder".into()),
        };

        assert_eq!(ctx.rule_count(), 1);
        assert!(ctx.rule_categories().contains(&"Project".to_string()));
    }
}
