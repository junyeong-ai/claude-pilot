//! Search and retrieval module.
//!
//! Provides search capabilities over:
//! - **Fix History**: `FixSearcher`, `FixRecord` for past fix attempts
//! - **Analysis Index**: `AnalysisIndex`, `IndexedChunk` for reusing analysis results

mod index;
mod query;
mod record;

pub use index::{AnalysisIndex, ChunkType, IndexSummary, IndexedChunk, SearchResult};
pub use query::{FixSearcher, SearchQuery, SearchResult as FixSearchResult};
pub use record::{FixDetail, FixLoader, FixRecord, LoadedFix};
