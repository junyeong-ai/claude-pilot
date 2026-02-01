use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub skills: Vec<ExtractionCandidate>,
    pub rules: Vec<ExtractionCandidate>,
    pub agents: Vec<ExtractionCandidate>,
}

impl ExtractionResult {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.rules.is_empty() && self.agents.is_empty()
    }

    pub fn total_count(&self) -> usize {
        self.skills.len() + self.rules.len() + self.agents.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionCandidate {
    pub name: String,
    pub candidate_type: CandidateType,
    pub description: String,
    pub content: String,
    pub confidence: f32,
    pub source_tasks: Vec<String>,
    #[serde(default)]
    pub metadata: CandidateMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateMetadata {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
}

impl ExtractionCandidate {
    pub fn skill(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            candidate_type: CandidateType::Skill,
            description: description.into(),
            content: String::new(),
            confidence: 0.0,
            source_tasks: Vec::new(),
            metadata: CandidateMetadata::default(),
        }
    }

    pub fn rule(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            candidate_type: CandidateType::Rule,
            description: description.into(),
            content: String::new(),
            confidence: 0.0,
            source_tasks: Vec::new(),
            metadata: CandidateMetadata::default(),
        }
    }

    pub fn agent(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            candidate_type: CandidateType::Agent,
            description: description.into(),
            content: String::new(),
            confidence: 0.0,
            source_tasks: Vec::new(),
            metadata: CandidateMetadata::default(),
        }
    }

    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence;
        self
    }

    pub fn with_source_tasks(mut self, tasks: Vec<String>) -> Self {
        self.source_tasks = tasks;
        self
    }

    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.metadata.paths = paths;
        self
    }

    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.metadata.tools = tools;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.metadata.model = Some(model.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType {
    Skill,
    Rule,
    Agent,
}

impl std::fmt::Display for CandidateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skill => write!(f, "Skill"),
            Self::Rule => write!(f, "Rule"),
            Self::Agent => write!(f, "Agent"),
        }
    }
}
