use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, info};

use crate::error::Result;
use crate::utils::estimate_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkType {
    FileAnalysis,
    DependencyAnalysis,
    PatternAnalysis,
    ConstraintAnalysis,
    ContextSummary,
    EvidenceSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedChunk {
    pub id: String,
    pub chunk_type: ChunkType,
    pub content: String,
    pub keywords: Vec<String>,
    pub file_paths: Vec<PathBuf>,
    pub constraints: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub mission_id: Option<String>,
    pub tokens: usize,
    pub relevance_score: f32,
}

impl IndexedChunk {
    pub fn new(id: impl Into<String>, chunk_type: ChunkType, content: impl Into<String>) -> Self {
        let content_str = content.into();
        let tokens = estimate_tokens(&content_str);
        let keywords = Self::extract_keywords(&content_str);

        Self {
            id: id.into(),
            chunk_type,
            content: content_str,
            keywords,
            file_paths: Vec::new(),
            constraints: Vec::new(),
            created_at: Utc::now(),
            mission_id: None,
            tokens,
            relevance_score: 1.0,
        }
    }

    pub fn with_file(mut self, path: impl AsRef<Path>) -> Self {
        self.file_paths.push(path.as_ref().to_path_buf());
        self
    }

    pub fn with_files(mut self, paths: Vec<PathBuf>) -> Self {
        self.file_paths.extend(paths);
        self
    }

    pub fn with_constraint(mut self, constraint: impl Into<String>) -> Self {
        self.constraints.push(constraint.into());
        self
    }

    pub fn with_constraints(mut self, constraints: Vec<String>) -> Self {
        self.constraints.extend(constraints);
        self
    }

    pub fn with_mission(mut self, mission_id: impl Into<String>) -> Self {
        self.mission_id = Some(mission_id.into());
        self
    }

    pub fn with_keywords(mut self, keywords: Vec<String>) -> Self {
        self.keywords.extend(keywords);
        self.keywords.sort();
        self.keywords.dedup();
        self
    }

    pub fn with_relevance(mut self, score: f32) -> Self {
        self.relevance_score = score.clamp(0.0, 1.0);
        self
    }

    fn extract_keywords(content: &str) -> Vec<String> {
        // Tokenize with language awareness
        let tokens = Self::tokenize_multilingual(content);

        // Filter and deduplicate
        let mut keywords: Vec<String> = tokens
            .into_iter()
            .filter(|w| Self::is_valid_keyword(w))
            .collect();

        keywords.sort();
        keywords.dedup();
        keywords.truncate(50);
        keywords
    }

    /// Tokenize text with multilingual support.
    ///
    /// For CJK text, each character is a meaningful unit (word).
    /// For alphabetic text, split on word boundaries.
    fn tokenize_multilingual(content: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current_alphabetic = String::new();

        for c in content.to_lowercase().chars() {
            if Self::is_cjk_character(c) {
                // Flush any pending alphabetic token
                if !current_alphabetic.is_empty() {
                    tokens.push(std::mem::take(&mut current_alphabetic));
                }
                // CJK: each character is a token (single characters are meaningful)
                tokens.push(c.to_string());
            } else if c.is_alphanumeric() || c == '_' {
                // Build alphabetic token
                current_alphabetic.push(c);
            } else {
                // Non-alphanumeric: flush current token
                if !current_alphabetic.is_empty() {
                    tokens.push(std::mem::take(&mut current_alphabetic));
                }
            }
        }

        // Flush remaining
        if !current_alphabetic.is_empty() {
            tokens.push(current_alphabetic);
        }

        // For CJK, also extract bigrams for better matching
        let mut bigrams = Vec::new();
        let cjk_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| t.chars().next().is_some_and(Self::is_cjk_character))
            .collect();

        for window in cjk_tokens.windows(2) {
            bigrams.push(format!("{}{}", window[0], window[1]));
        }

        tokens.extend(bigrams);
        tokens
    }

    /// Check if a character is CJK (Chinese, Japanese, Korean).
    fn is_cjk_character(c: char) -> bool {
        // CJK Unified Ideographs
        ('\u{4E00}'..='\u{9FFF}').contains(&c) ||
        // CJK Extension A
        ('\u{3400}'..='\u{4DBF}').contains(&c) ||
        // Hangul Syllables
        ('\u{AC00}'..='\u{D7AF}').contains(&c) ||
        // Hiragana
        ('\u{3040}'..='\u{309F}').contains(&c) ||
        // Katakana
        ('\u{30A0}'..='\u{30FF}').contains(&c)
    }

    /// Check if a token is a valid keyword.
    fn is_valid_keyword(token: &str) -> bool {
        // English stopwords
        const ENGLISH_STOPWORDS: &[&str] = &[
            "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with",
            "by", "from", "as", "is", "was", "are", "were", "been", "be", "have", "has", "had",
            "do", "does", "did", "will", "would", "could", "should", "may", "might", "must",
            "shall", "can", "need", "this", "that", "these", "those", "it", "its", "which", "what",
            "who", "whom", "whose", "when", "where", "why", "how", "not", "no", "yes", "all",
            "any", "some", "none", "each", "every", "both", "few", "more", "most", "other", "such",
        ];

        // Check if it's a CJK token (single char or bigram)
        let first_char = token.chars().next();
        if first_char.is_some_and(Self::is_cjk_character) {
            // CJK tokens: accept all (no stopword filtering for CJK)
            return true;
        }

        // Alphabetic tokens: apply length and stopword filters
        token.len() >= 3 && !ENGLISH_STOPWORDS.contains(&token)
    }

    pub fn matches_query(&self, query: &str) -> f32 {
        let query_words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        if query_words.is_empty() {
            return 0.0;
        }

        let mut matches = 0;
        for word in &query_words {
            if self.keywords.iter().any(|k| k.contains(word)) {
                matches += 1;
            }
            if self.content.to_lowercase().contains(word) {
                matches += 1;
            }
        }

        let score = matches as f32 / (query_words.len() * 2) as f32;
        score * self.relevance_score
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk: IndexedChunk,
    pub score: f32,
    pub matched_keywords: Vec<String>,
}

pub struct AnalysisIndex {
    chunks: Vec<IndexedChunk>,
    keyword_index: HashMap<String, Vec<usize>>,
    file_index: HashMap<PathBuf, Vec<usize>>,
    constraint_index: HashMap<String, Vec<usize>>,
    type_index: HashMap<ChunkType, Vec<usize>>,
    mission_index: HashMap<String, Vec<usize>>,
}

impl Default for AnalysisIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisIndex {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            keyword_index: HashMap::new(),
            file_index: HashMap::new(),
            constraint_index: HashMap::new(),
            type_index: HashMap::new(),
            mission_index: HashMap::new(),
        }
    }

    pub fn add(&mut self, chunk: IndexedChunk) -> usize {
        let idx = self.chunks.len();

        for keyword in &chunk.keywords {
            self.keyword_index
                .entry(keyword.clone())
                .or_default()
                .push(idx);
        }

        for file in &chunk.file_paths {
            self.file_index.entry(file.clone()).or_default().push(idx);
        }

        for constraint in &chunk.constraints {
            self.constraint_index
                .entry(constraint.clone())
                .or_default()
                .push(idx);
        }

        self.type_index
            .entry(chunk.chunk_type)
            .or_default()
            .push(idx);

        if let Some(ref mission_id) = chunk.mission_id {
            self.mission_index
                .entry(mission_id.clone())
                .or_default()
                .push(idx);
        }

        debug!(chunk_id = %chunk.id, idx, "Added chunk to index");
        self.chunks.push(chunk);
        idx
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut candidate_indices: HashMap<usize, usize> = HashMap::new();

        for word in &query_words {
            if let Some(indices) = self.keyword_index.get(*word) {
                for &idx in indices {
                    *candidate_indices.entry(idx).or_default() += 1;
                }
            }

            for (keyword, indices) in &self.keyword_index {
                if keyword.contains(word) {
                    for &idx in indices {
                        *candidate_indices.entry(idx).or_default() += 1;
                    }
                }
            }
        }

        let mut results: Vec<SearchResult> = candidate_indices
            .into_iter()
            .filter_map(|(idx, _)| {
                let chunk = &self.chunks[idx];
                let score = chunk.matches_query(query);
                if score > 0.0 {
                    let matched_keywords: Vec<String> = query_words
                        .iter()
                        .filter(|w| chunk.keywords.iter().any(|k| k.contains(*w)))
                        .map(|w| w.to_string())
                        .collect();

                    Some(SearchResult {
                        chunk: chunk.clone(),
                        score,
                        matched_keywords,
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(20);
        results
    }

    pub fn file_analysis(&self, path: &Path) -> Vec<&IndexedChunk> {
        self.file_index
            .get(path)
            .map(|indices| indices.iter().map(|&idx| &self.chunks[idx]).collect())
            .unwrap_or_default()
    }

    pub fn related_constraints(&self, topic: &str) -> Vec<&IndexedChunk> {
        let topic_lower = topic.to_lowercase();
        let mut results = Vec::new();

        for (constraint, indices) in &self.constraint_index {
            if constraint.to_lowercase().contains(&topic_lower) {
                for &idx in indices {
                    results.push(&self.chunks[idx]);
                }
            }
        }

        results
    }

    pub fn by_type(&self, chunk_type: ChunkType) -> Vec<&IndexedChunk> {
        self.type_index
            .get(&chunk_type)
            .map(|indices| indices.iter().map(|&idx| &self.chunks[idx]).collect())
            .unwrap_or_default()
    }

    pub fn for_mission(&self, mission_id: &str) -> Vec<&IndexedChunk> {
        self.mission_index
            .get(mission_id)
            .map(|indices| indices.iter().map(|&idx| &self.chunks[idx]).collect())
            .unwrap_or_default()
    }

    pub fn chunk(&self, id: &str) -> Option<&IndexedChunk> {
        self.chunks.iter().find(|c| c.id == id)
    }

    pub fn total_tokens(&self) -> usize {
        self.chunks.iter().map(|c| c.tokens).sum()
    }

    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.keyword_index.clear();
        self.file_index.clear();
        self.constraint_index.clear();
        self.type_index.clear();
        self.mission_index.clear();
    }

    pub fn prune_by_relevance(&mut self, threshold: f32) {
        let to_remove: Vec<usize> = self
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.relevance_score < threshold)
            .map(|(i, _)| i)
            .collect();

        for idx in to_remove.into_iter().rev() {
            self.remove_from_indices(idx);
            self.chunks.remove(idx);
        }

        self.rebuild_indices();
    }

    fn remove_from_indices(&mut self, _idx: usize) {
        // This is called before rebuilding, so we just need to track what to remove
    }

    fn rebuild_indices(&mut self) {
        self.keyword_index.clear();
        self.file_index.clear();
        self.constraint_index.clear();
        self.type_index.clear();
        self.mission_index.clear();

        for (idx, chunk) in self.chunks.iter().enumerate() {
            for keyword in &chunk.keywords {
                self.keyword_index
                    .entry(keyword.clone())
                    .or_default()
                    .push(idx);
            }

            for file in &chunk.file_paths {
                self.file_index.entry(file.clone()).or_default().push(idx);
            }

            for constraint in &chunk.constraints {
                self.constraint_index
                    .entry(constraint.clone())
                    .or_default()
                    .push(idx);
            }

            self.type_index
                .entry(chunk.chunk_type)
                .or_default()
                .push(idx);

            if let Some(ref mission_id) = chunk.mission_id {
                self.mission_index
                    .entry(mission_id.clone())
                    .or_default()
                    .push(idx);
            }
        }
    }

    pub async fn save(&self, path: &Path) -> Result<()> {
        info!(path = %path.display(), chunks = self.chunks.len(), "Saving analysis index");

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let json = serde_json::to_string_pretty(&self.chunks)?;
        fs::write(path, json).await?;

        Ok(())
    }

    pub async fn load(path: &Path) -> Result<Self> {
        info!(path = %path.display(), "Loading analysis index");

        let content = fs::read_to_string(path).await?;
        let chunks: Vec<IndexedChunk> = serde_json::from_str(&content)?;

        let mut index = Self::new();
        for chunk in chunks {
            index.add(chunk);
        }

        Ok(index)
    }

    pub fn summary(&self) -> IndexSummary {
        IndexSummary {
            total_chunks: self.chunks.len(),
            total_tokens: self.total_tokens(),
            unique_keywords: self.keyword_index.len(),
            unique_files: self.file_index.len(),
            unique_constraints: self.constraint_index.len(),
            chunks_by_type: self.type_index.iter().map(|(t, v)| (*t, v.len())).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IndexSummary {
    pub total_chunks: usize,
    pub total_tokens: usize,
    pub unique_keywords: usize,
    pub unique_files: usize,
    pub unique_constraints: usize,
    pub chunks_by_type: HashMap<ChunkType, usize>,
}

impl std::fmt::Display for IndexSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Analysis Index Summary:")?;
        writeln!(f, "  Chunks: {}", self.total_chunks)?;
        writeln!(f, "  Tokens: {}", self.total_tokens)?;
        writeln!(f, "  Keywords: {}", self.unique_keywords)?;
        writeln!(f, "  Files: {}", self.unique_files)?;
        writeln!(f, "  Constraints: {}", self.unique_constraints)?;
        writeln!(f, "  By Type:")?;
        for (chunk_type, count) in &self.chunks_by_type {
            writeln!(f, "    {:?}: {}", chunk_type, count)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_chunk(id: &str, content: &str) -> IndexedChunk {
        IndexedChunk::new(id, ChunkType::FileAnalysis, content)
    }

    #[test]
    fn test_chunk_creation() {
        let chunk = create_test_chunk("chunk-1", "Authentication handler with JWT tokens")
            .with_file("src/auth.rs")
            .with_constraint("must validate token expiry");

        assert_eq!(chunk.id, "chunk-1");
        assert!(chunk.tokens > 0);
        assert!(!chunk.keywords.is_empty());
        assert!(chunk.keywords.contains(&"authentication".to_string()));
        assert_eq!(chunk.file_paths.len(), 1);
        assert_eq!(chunk.constraints.len(), 1);
    }

    #[test]
    fn test_index_add_and_search() {
        let mut index = AnalysisIndex::new();

        index.add(create_test_chunk(
            "auth-1",
            "User authentication with JWT tokens",
        ));
        index.add(create_test_chunk("db-1", "Database connection pooling"));
        index.add(create_test_chunk(
            "auth-2",
            "OAuth2 authentication provider",
        ));

        let results = index.search("authentication");
        assert_eq!(results.len(), 2);
        assert!(results[0].score >= results[1].score);
    }

    #[test]
    fn test_file_analysis() {
        let mut index = AnalysisIndex::new();

        let auth_path = PathBuf::from("src/auth.rs");
        index.add(create_test_chunk("auth-1", "Auth module").with_file(&auth_path));
        index.add(create_test_chunk("auth-2", "Token validation").with_file(&auth_path));
        index.add(create_test_chunk("db-1", "Database module").with_file("src/db.rs"));

        let results = index.file_analysis(&auth_path);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_related_constraints() {
        let mut index = AnalysisIndex::new();

        index.add(
            create_test_chunk("c-1", "Security check")
                .with_constraint("must validate user permissions"),
        );
        index.add(
            create_test_chunk("c-2", "Auth flow")
                .with_constraint("must validate token before access"),
        );
        index.add(create_test_chunk("c-3", "Data handling").with_constraint("must sanitize input"));

        let results = index.related_constraints("validate");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_by_type() {
        let mut index = AnalysisIndex::new();

        index.add(IndexedChunk::new(
            "f-1",
            ChunkType::FileAnalysis,
            "File content",
        ));
        index.add(IndexedChunk::new(
            "d-1",
            ChunkType::DependencyAnalysis,
            "Dependency",
        ));
        index.add(IndexedChunk::new(
            "f-2",
            ChunkType::FileAnalysis,
            "More file content",
        ));

        let file_chunks = index.by_type(ChunkType::FileAnalysis);
        assert_eq!(file_chunks.len(), 2);

        let dep_chunks = index.by_type(ChunkType::DependencyAnalysis);
        assert_eq!(dep_chunks.len(), 1);
    }

    #[test]
    fn test_for_mission() {
        let mut index = AnalysisIndex::new();

        index.add(create_test_chunk("m1-1", "Mission 1 task").with_mission("mission-001"));
        index.add(create_test_chunk("m1-2", "Mission 1 other").with_mission("mission-001"));
        index.add(create_test_chunk("m2-1", "Mission 2 task").with_mission("mission-002"));

        let m1_chunks = index.for_mission("mission-001");
        assert_eq!(m1_chunks.len(), 2);

        let m2_chunks = index.for_mission("mission-002");
        assert_eq!(m2_chunks.len(), 1);
    }

    #[test]
    fn test_keyword_extraction() {
        let keywords = IndexedChunk::extract_keywords(
            "The authentication handler processes JWT tokens for user validation",
        );

        assert!(keywords.contains(&"authentication".to_string()));
        assert!(keywords.contains(&"handler".to_string()));
        assert!(keywords.contains(&"tokens".to_string()));
        assert!(!keywords.contains(&"the".to_string()));
        assert!(!keywords.contains(&"for".to_string()));
    }

    #[test]
    fn test_search_scoring() {
        let mut index = AnalysisIndex::new();

        index.add(
            create_test_chunk("exact", "authentication authentication authentication")
                .with_relevance(1.0),
        );
        index.add(create_test_chunk("partial", "user auth").with_relevance(0.5));

        let results = index.search("authentication");
        assert!(!results.is_empty());
        assert_eq!(results[0].chunk.id, "exact");
    }

    #[test]
    fn test_summary() {
        let mut index = AnalysisIndex::new();

        index.add(
            create_test_chunk("c-1", "Content 1")
                .with_file("file1.rs")
                .with_constraint("constraint1"),
        );
        index.add(create_test_chunk("c-2", "Content 2").with_file("file2.rs"));

        let summary = index.summary();
        assert_eq!(summary.total_chunks, 2);
        assert_eq!(summary.unique_files, 2);
        assert_eq!(summary.unique_constraints, 1);
    }

    #[test]
    fn test_prune_by_relevance() {
        let mut index = AnalysisIndex::new();

        index.add(create_test_chunk("high", "Important").with_relevance(0.9));
        index.add(create_test_chunk("low", "Less important").with_relevance(0.3));
        index.add(create_test_chunk("medium", "Somewhat important").with_relevance(0.6));

        index.prune_by_relevance(0.5);
        assert_eq!(index.len(), 2);
        assert!(index.chunk("high").is_some());
        assert!(index.chunk("medium").is_some());
        assert!(index.chunk("low").is_none());
    }

    #[test]
    fn test_cjk_character_detection() {
        // Chinese
        assert!(IndexedChunk::is_cjk_character('中'));
        assert!(IndexedChunk::is_cjk_character('国'));

        // Korean
        assert!(IndexedChunk::is_cjk_character('한'));
        assert!(IndexedChunk::is_cjk_character('국'));

        // Japanese
        assert!(IndexedChunk::is_cjk_character('日'));
        assert!(IndexedChunk::is_cjk_character('本'));
        assert!(IndexedChunk::is_cjk_character('あ')); // Hiragana
        assert!(IndexedChunk::is_cjk_character('ア')); // Katakana

        // Non-CJK
        assert!(!IndexedChunk::is_cjk_character('a'));
        assert!(!IndexedChunk::is_cjk_character('Z'));
        assert!(!IndexedChunk::is_cjk_character('5'));
    }

    #[test]
    fn test_multilingual_tokenization() {
        // Pure English
        let tokens = IndexedChunk::tokenize_multilingual("hello world test");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));

        // Pure Chinese - each character is a token + bigrams
        let tokens = IndexedChunk::tokenize_multilingual("你好世界");
        assert!(tokens.contains(&"你".to_string()));
        assert!(tokens.contains(&"好".to_string()));
        assert!(tokens.contains(&"你好".to_string())); // bigram

        // Mixed content
        let tokens = IndexedChunk::tokenize_multilingual("Hello 你好 World");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"你".to_string()));
        assert!(tokens.contains(&"好".to_string()));
    }

    #[test]
    fn test_cjk_keyword_extraction() {
        // Chinese content
        let chunk = IndexedChunk::new("test", ChunkType::FileAnalysis, "用户认证模块处理JWT令牌");
        // Should contain CJK characters as keywords
        assert!(chunk.keywords.iter().any(|k| k.contains('用')));
        assert!(chunk.keywords.iter().any(|k| k.contains("jwt")));

        // Korean content
        let chunk = IndexedChunk::new("test", ChunkType::FileAnalysis, "사용자 인증 모듈");
        assert!(chunk.keywords.iter().any(|k| k.contains('사')));
    }

    #[test]
    fn test_cjk_search() {
        let mut index = AnalysisIndex::new();

        // Add chunks with CJK content
        index.add(IndexedChunk::new(
            "cn-1",
            ChunkType::FileAnalysis,
            "用户认证模块",
        ));
        index.add(IndexedChunk::new(
            "kr-1",
            ChunkType::FileAnalysis,
            "사용자 인증",
        ));
        index.add(IndexedChunk::new(
            "jp-1",
            ChunkType::FileAnalysis,
            "ユーザー認証",
        ));
        index.add(IndexedChunk::new(
            "en-1",
            ChunkType::FileAnalysis,
            "user authentication",
        ));

        // Search with Chinese
        let results = index.search("认证");
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.chunk.id == "cn-1"));

        // Search with Korean
        let results = index.search("인증");
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.chunk.id == "kr-1"));

        // Search with English
        let results = index.search("authentication");
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.chunk.id == "en-1"));
    }
}
