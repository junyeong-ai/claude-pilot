//! Search and retrieval module.
//!
//! Provides search capabilities over:
//! - **Fix History**: `FixSearcher`, `FixRecord` for past fix attempts
//! - **Analysis Index**: `AnalysisIndex`, `IndexedChunk` for reusing analysis results

mod index;
mod query;
mod record;

pub use index::{AnalysisIndex, ChunkSearchResult, ChunkType, IndexSummary, IndexedChunk};
pub use query::{FixSearchResult, FixSearcher, SearchQuery};
pub use record::{FixDetail, FixLoader, FixRecord, LoadedFix};
