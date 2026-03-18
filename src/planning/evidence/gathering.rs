use chrono::Utc;

use super::types::EvidenceCompleteness;
use super::Evidence;

impl Evidence {
    pub fn empty() -> Self {
        Self {
            codebase_analysis: Default::default(),
            dependency_analysis: Default::default(),
            prior_knowledge: Default::default(),
            gathered_at: Utc::now(),
            completeness: EvidenceCompleteness::complete(),
        }
    }

    pub fn merge(&mut self, other: Evidence) {
        for file in other.codebase_analysis.relevant_files {
            if !self
                .codebase_analysis
                .relevant_files
                .iter()
                .any(|f| f.path == file.path)
            {
                self.codebase_analysis.relevant_files.push(file);
            }
        }

        for pattern in other.codebase_analysis.existing_patterns {
            if !self.codebase_analysis.existing_patterns.contains(&pattern) {
                self.codebase_analysis.existing_patterns.push(pattern);
            }
        }

        for convention in other.codebase_analysis.conventions {
            if !self.codebase_analysis.conventions.contains(&convention) {
                self.codebase_analysis.conventions.push(convention);
            }
        }

        for area in other.codebase_analysis.affected_areas {
            if !self.codebase_analysis.affected_areas.contains(&area) {
                self.codebase_analysis.affected_areas.push(area);
            }
        }

        for dep in other.dependency_analysis.current_dependencies {
            if !self
                .dependency_analysis
                .current_dependencies
                .iter()
                .any(|d| d.name == dep.name)
            {
                self.dependency_analysis.current_dependencies.push(dep);
            }
        }

        for skill in other.prior_knowledge.relevant_skills {
            if !self
                .prior_knowledge
                .relevant_skills
                .iter()
                .any(|s| s.path == skill.path)
            {
                self.prior_knowledge.relevant_skills.push(skill);
            }
        }

        for rule in other.prior_knowledge.relevant_rules {
            if !self
                .prior_knowledge
                .relevant_rules
                .iter()
                .any(|r| r.path == rule.path)
            {
                self.prior_knowledge.relevant_rules.push(rule);
            }
        }

        if !other.completeness.is_complete {
            self.completeness.is_complete = false;
        }
        self.completeness
            .warnings
            .extend(other.completeness.warnings);
    }

    pub fn all_sources(&self) -> Vec<String> {
        let mut sources = Vec::new();

        for file in &self.codebase_analysis.relevant_files {
            sources.push(format!(
                "File '{}' via {}",
                file.path,
                file.source.description()
            ));
        }

        for skill in &self.prior_knowledge.relevant_skills {
            sources.push(format!(
                "Skill '{}' via {}",
                skill.name,
                skill.source.description()
            ));
        }

        for rule in &self.prior_knowledge.relevant_rules {
            sources.push(format!(
                "Rule '{}' via {}",
                rule.name,
                rule.source.description()
            ));
        }

        sources
    }
}
