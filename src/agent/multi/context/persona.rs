//! Agent persona loading from claudegen-generated agent definitions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::debug;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusRole {
    pub priority: u8,
    pub can_veto: bool,
    #[serde(default = "default_vote_threshold")]
    pub vote_threshold: f64,
}

fn default_vote_threshold() -> f64 {
    0.67
}

impl Default for ConsensusRole {
    fn default() -> Self {
        Self {
            priority: 50,
            can_veto: false,
            vote_threshold: default_vote_threshold(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentPersona {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub skills: Vec<String>,
    pub consensus: ConsensusRole,
    pub source_path: PathBuf,
}

impl AgentPersona {
    pub fn has_skill(&self, skill: &str) -> bool {
        self.skills.iter().any(|s| s.eq_ignore_ascii_case(skill))
    }

    pub fn has_tool(&self, tool: &str) -> bool {
        self.allowed_tools
            .iter()
            .any(|t| t.eq_ignore_ascii_case(tool))
    }
}

pub struct PersonaLoader {
    personas: HashMap<String, AgentPersona>,
    personas_dir: PathBuf,
}

impl PersonaLoader {
    pub fn new(personas_dir: PathBuf) -> Self {
        Self {
            personas: HashMap::new(),
            personas_dir,
        }
    }

    pub async fn load(&mut self) -> Result<()> {
        if !self.personas_dir.exists() {
            debug!(path = %self.personas_dir.display(), "Personas directory not found");
            return Ok(());
        }

        let mut entries = fs::read_dir(&self.personas_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && let Some(persona) = self.parse_persona(&path).await?
            {
                self.personas.insert(persona.name.clone(), persona);
            }
        }

        debug!(count = self.personas.len(), "Loaded agent personas");
        Ok(())
    }

    async fn parse_persona(&self, path: &Path) -> Result<Option<AgentPersona>> {
        let content = fs::read_to_string(path).await?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let Some((frontmatter, body)) = Self::split_frontmatter(&content) else {
            return Ok(None);
        };

        #[derive(Deserialize)]
        struct Frontmatter {
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            tools: Vec<String>,
            #[serde(default)]
            skills: Vec<String>,
            #[serde(default)]
            consensus: Option<ConsensusRole>,
        }

        let fm: Frontmatter = serde_yaml_bw::from_str(frontmatter)
            .map_err(|e| crate::error::PilotError::Config(e.to_string()))?;

        Ok(Some(AgentPersona {
            name: fm.name.unwrap_or(name),
            description: fm.description.unwrap_or_default(),
            system_prompt: body.trim().to_string(),
            allowed_tools: fm.tools,
            skills: fm.skills,
            consensus: fm.consensus.unwrap_or_default(),
            source_path: path.to_path_buf(),
        }))
    }

    fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
        let rest = content.strip_prefix("---\n")?;
        let end = rest.find("\n---\n")?;
        Some((&rest[..end], &rest[end + 5..]))
    }

    pub fn get(&self, name: &str) -> Option<&AgentPersona> {
        self.personas.get(name)
    }

    pub fn get_by_role(&self, role_id: &str) -> Option<&AgentPersona> {
        let normalized = role_id.to_lowercase();
        self.personas
            .values()
            .find(|p| p.name.to_lowercase() == normalized)
    }

    pub fn all(&self) -> impl Iterator<Item = &AgentPersona> {
        self.personas.values()
    }

    pub fn is_empty(&self) -> bool {
        self.personas.is_empty()
    }

    pub fn len(&self) -> usize {
        self.personas.len()
    }
}

impl Default for PersonaLoader {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_load_persona() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path();

        let content = r#"---
name: reviewer
description: Code quality gatekeeper
tools:
  - Read
  - Grep
  - Glob
skills:
  - code-review
consensus:
  priority: 70
  can_veto: true
---

# Reviewer Agent

You are a code reviewer focused on quality.
"#;
        fs::write(agents_dir.join("reviewer.md"), content)
            .await
            .unwrap();

        let mut loader = PersonaLoader::new(agents_dir.to_path_buf());
        loader.load().await.unwrap();

        assert_eq!(loader.len(), 1);

        let persona = loader.get("reviewer").unwrap();
        assert_eq!(persona.name, "reviewer");
        assert_eq!(persona.description, "Code quality gatekeeper");
        assert!(persona.has_skill("code-review"));
        assert!(persona.has_tool("Read"));
        assert_eq!(persona.consensus.priority, 70);
        assert!(persona.consensus.can_veto);
        assert!(persona.system_prompt.contains("code reviewer"));
    }

    #[tokio::test]
    async fn test_empty_directory() {
        let temp = TempDir::new().unwrap();
        let mut loader = PersonaLoader::new(temp.path().to_path_buf());
        loader.load().await.unwrap();
        assert!(loader.is_empty());
    }

    #[tokio::test]
    async fn test_missing_directory() {
        let mut loader = PersonaLoader::new(PathBuf::from("/nonexistent"));
        loader.load().await.unwrap();
        assert!(loader.is_empty());
    }

    #[test]
    fn test_default_consensus_role() {
        let role = ConsensusRole::default();
        assert_eq!(role.priority, 50);
        assert!(!role.can_veto);
        assert!((role.vote_threshold - 0.67).abs() < 0.001);
    }
}
