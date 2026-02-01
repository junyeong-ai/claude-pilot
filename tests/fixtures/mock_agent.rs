//! Mock task agent for testing without actual LLM calls.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::RwLock;
use regex::Regex;

#[derive(Debug, Clone)]
pub enum ResponseScenario {
    Static(String),
    Sequential(Vec<String>),
    Conditional {
        patterns: Vec<(Regex, String)>,
        default: String,
    },
}

impl ResponseScenario {
    pub fn static_response(response: impl Into<String>) -> Self {
        Self::Static(response.into())
    }

    pub fn sequential(responses: Vec<impl Into<String>>) -> Self {
        Self::Sequential(responses.into_iter().map(Into::into).collect())
    }

    pub fn conditional(
        patterns: Vec<(&str, impl Into<String>)>,
        default: impl Into<String>,
    ) -> Self {
        Self::Conditional {
            patterns: patterns
                .into_iter()
                .map(|(p, r)| {
                    let regex = Regex::new(p)
                        .unwrap_or_else(|e| panic!("Invalid regex pattern '{}': {}", p, e));
                    (regex, r.into())
                })
                .collect(),
            default: default.into(),
        }
    }

    fn match_conditional(&self, prompt: &str) -> Option<String> {
        if let Self::Conditional { patterns, default } = self {
            for (pattern, response) in patterns {
                if pattern.is_match(prompt) {
                    return Some(response.clone());
                }
            }
            return Some(default.clone());
        }
        None
    }
}

#[derive(Debug)]
pub struct MockTaskAgent {
    responses: RwLock<HashMap<String, ResponseScenario>>,
    call_counts: RwLock<HashMap<String, AtomicUsize>>,
    default_response: String,
}

impl Default for MockTaskAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTaskAgent {
    pub fn new() -> Self {
        Self {
            responses: RwLock::new(HashMap::new()),
            call_counts: RwLock::new(HashMap::new()),
            default_response: "OK".to_string(),
        }
    }

    pub fn set_response(&self, key: &str, scenario: ResponseScenario) {
        self.responses.write().insert(key.to_string(), scenario);
        self.call_counts
            .write()
            .insert(key.to_string(), AtomicUsize::new(0));
    }

    pub fn call_count(&self, key: &str) -> usize {
        self.call_counts
            .read()
            .get(key)
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    pub fn assert_called(&self, key: &str, times: usize) {
        let count = self.call_count(key);
        assert_eq!(
            count, times,
            "Expected '{}' to be called {} times, but was called {} times",
            key, times, count
        );
    }

    pub async fn run_prompt(&self, prompt: &str, _working_dir: &Path) -> String {
        let (key, response) = self.find_response(prompt);

        if let Some(key) = key
            && let Some(counter) = self.call_counts.read().get(&key)
        {
            counter.fetch_add(1, Ordering::SeqCst);
        }

        response
    }

    fn find_response(&self, prompt: &str) -> (Option<String>, String) {
        let responses = self.responses.read();

        for (key, scenario) in responses.iter() {
            if prompt.contains(key) {
                return self.resolve_scenario(key, scenario, prompt);
            }
        }

        for (key, scenario) in responses.iter() {
            if let Some(response) = scenario.match_conditional(prompt) {
                return (Some(key.clone()), response);
            }
        }

        (None, self.default_response.clone())
    }

    fn resolve_scenario(
        &self,
        key: &str,
        scenario: &ResponseScenario,
        prompt: &str,
    ) -> (Option<String>, String) {
        match scenario {
            ResponseScenario::Static(response) => (Some(key.to_string()), response.clone()),

            ResponseScenario::Sequential(responses) => {
                let count = self.call_count(key);
                let idx = count % responses.len();
                (Some(key.to_string()), responses[idx].clone())
            }

            ResponseScenario::Conditional { .. } => {
                let response = scenario.match_conditional(prompt).unwrap_or_default();
                (Some(key.to_string()), response)
            }
        }
    }
}

pub struct MockTaskAgentBuilder {
    agent: MockTaskAgent,
}

impl MockTaskAgentBuilder {
    pub fn new() -> Self {
        Self {
            agent: MockTaskAgent::new(),
        }
    }

    pub fn response(self, key: &str, scenario: ResponseScenario) -> Self {
        self.agent.set_response(key, scenario);
        self
    }

    pub fn static_response(self, key: &str, response: impl Into<String>) -> Self {
        self.response(key, ResponseScenario::static_response(response))
    }

    pub fn sequential_responses(self, key: &str, responses: Vec<&str>) -> Self {
        self.response(
            key,
            ResponseScenario::sequential(responses.into_iter().map(String::from).collect()),
        )
    }

    pub fn build(self) -> MockTaskAgent {
        self.agent
    }
}

impl Default for MockTaskAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_static_response() {
        let agent = MockTaskAgent::new();
        agent.set_response("test", ResponseScenario::static_response("hello"));

        let response = agent
            .run_prompt("this is a test prompt", &PathBuf::new())
            .await;
        assert_eq!(response, "hello");
    }

    #[tokio::test]
    async fn test_sequential_response() {
        let agent = MockTaskAgent::new();
        agent.set_response(
            "test",
            ResponseScenario::sequential(vec!["first", "second", "third"]),
        );

        let r1 = agent.run_prompt("test 1", &PathBuf::new()).await;
        let r2 = agent.run_prompt("test 2", &PathBuf::new()).await;
        let r3 = agent.run_prompt("test 3", &PathBuf::new()).await;
        let r4 = agent.run_prompt("test 4", &PathBuf::new()).await;

        assert_eq!(r1, "first");
        assert_eq!(r2, "second");
        assert_eq!(r3, "third");
        assert_eq!(r4, "first");
    }

    #[tokio::test]
    async fn test_conditional_response() {
        let agent = MockTaskAgent::new();
        agent.set_response(
            "pattern",
            ResponseScenario::conditional(
                vec![("auth", "auth response"), ("api", "api response")],
                "default response",
            ),
        );

        let r1 = agent
            .run_prompt("pattern auth check", &PathBuf::new())
            .await;
        let r2 = agent.run_prompt("pattern api check", &PathBuf::new()).await;
        let r3 = agent
            .run_prompt("pattern other check", &PathBuf::new())
            .await;

        assert_eq!(r1, "auth response");
        assert_eq!(r2, "api response");
        assert_eq!(r3, "default response");
    }

    #[tokio::test]
    async fn test_call_count() {
        let agent = MockTaskAgent::new();
        agent.set_response("test", ResponseScenario::static_response("ok"));

        for _ in 0..5 {
            agent.run_prompt("test", &PathBuf::new()).await;
        }

        agent.assert_called("test", 5);
    }

    #[tokio::test]
    async fn test_builder() {
        let agent = MockTaskAgentBuilder::new()
            .static_response("research", r#"{"files": []}"#)
            .sequential_responses("verify", vec!["pass", "pass"])
            .build();

        let r1 = agent.run_prompt("research task", &PathBuf::new()).await;
        assert!(r1.contains("files"));
    }
}
