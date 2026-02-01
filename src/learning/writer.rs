use tokio::fs;
use tracing::debug;

use super::{CandidateType, ExtractionCandidate};
use crate::config::ProjectPaths;
use crate::error::Result;

pub struct ClaudeCodeWriter {
    paths: ProjectPaths,
}

impl ClaudeCodeWriter {
    pub fn new(paths: ProjectPaths) -> Self {
        Self { paths }
    }

    pub async fn write(&self, candidate: &ExtractionCandidate) -> Result<String> {
        debug!(
            name = %candidate.name,
            candidate_type = %candidate.candidate_type,
            "Writing extraction candidate"
        );
        match candidate.candidate_type {
            CandidateType::Skill => self.write_skill(candidate).await,
            CandidateType::Rule => self.write_rule(candidate).await,
            CandidateType::Agent => self.write_agent(candidate).await,
        }
    }

    async fn write_skill(&self, candidate: &ExtractionCandidate) -> Result<String> {
        let skill_dir = self.paths.skills_dir().join(&candidate.name);
        fs::create_dir_all(&skill_dir).await?;
        debug!(path = %skill_dir.display(), "Created skill directory");

        let skill_path = skill_dir.join("SKILL.md");
        let content = self.format_skill(candidate);
        fs::write(&skill_path, &content).await?;

        Ok(skill_path.display().to_string())
    }

    async fn write_rule(&self, candidate: &ExtractionCandidate) -> Result<String> {
        fs::create_dir_all(self.paths.rules_dir()).await?;

        let rule_path = self
            .paths
            .rules_dir()
            .join(format!("{}.md", candidate.name));
        let content = self.format_rule(candidate);
        fs::write(&rule_path, &content).await?;

        Ok(rule_path.display().to_string())
    }

    async fn write_agent(&self, candidate: &ExtractionCandidate) -> Result<String> {
        fs::create_dir_all(self.paths.agents_dir()).await?;

        let agent_path = self
            .paths
            .agents_dir()
            .join(format!("{}.md", candidate.name));
        let content = self.format_agent(candidate);
        fs::write(&agent_path, &content).await?;

        Ok(agent_path.display().to_string())
    }

    fn format_skill(&self, candidate: &ExtractionCandidate) -> String {
        let mut frontmatter = format!(
            "---\nname: {}\ndescription: {}",
            candidate.name, candidate.description
        );

        if !candidate.metadata.tools.is_empty() {
            frontmatter.push_str(&format!(
                "\nallowed-tools: {}",
                candidate.metadata.tools.join(", ")
            ));
        }

        if let Some(model) = &candidate.metadata.model {
            frontmatter.push_str(&format!("\nmodel: {}", model));
        }

        frontmatter.push_str("\n---\n");

        format!(
            r"{}
# {}

{}

## When to Use

{}

## Source

Extracted from missions: {}
",
            frontmatter,
            Self::to_title_case(&candidate.name),
            candidate.content,
            candidate.description,
            candidate.source_tasks.join(", ")
        )
    }

    fn format_rule(&self, candidate: &ExtractionCandidate) -> String {
        let mut output = String::new();

        if !candidate.metadata.paths.is_empty() {
            output.push_str("---\npaths:\n");
            for path in &candidate.metadata.paths {
                output.push_str(&format!("  - \"{}\"\n", path));
            }
            output.push_str("---\n\n");
        }

        output.push_str(&format!(
            r"# {}

{}

{}

---
*Extracted from missions: {}*
",
            Self::to_title_case(&candidate.name),
            candidate.description,
            candidate.content,
            candidate.source_tasks.join(", ")
        ));

        output
    }

    fn format_agent(&self, candidate: &ExtractionCandidate) -> String {
        let tools = if candidate.metadata.tools.is_empty() {
            "Read, Grep, Glob, Bash, Edit, Write".to_string()
        } else {
            candidate.metadata.tools.join(", ")
        };

        let model = candidate.metadata.model.as_deref().unwrap_or("inherit");

        let mut frontmatter = format!(
            "---\nname: {}\ndescription: {}\ntools: {}\nmodel: {}",
            candidate.name, candidate.description, tools, model
        );

        if let Some(perm) = &candidate.metadata.permission_mode {
            frontmatter.push_str(&format!("\npermissionMode: {}", perm));
        }

        frontmatter.push_str("\n---\n");

        format!(
            r"{}
# {} Agent

{}

{}
",
            frontmatter,
            Self::to_title_case(&candidate.name),
            candidate.description,
            candidate.content
        )
    }

    fn to_title_case(s: &str) -> String {
        s.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}
