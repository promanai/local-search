use crate::normalize_search_text;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Requested planner behavior.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    /// Product search may safely fall back to exact/token/prefix retrieval.
    ProductSearch,
    /// The caller requires substring semantics and may be rejected by policy.
    SubstringOnly,
}

/// Stable typed policy errors returned before expensive backend work.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PolicyError {
    /// The normalized query has no searchable characters.
    #[error("the normalized query is empty")]
    EmptyQuery,
    /// A one/two-character substring shape has unbounded expansion risk.
    #[error("substring-only queries require at least three normalized characters (got {length})")]
    ExpensiveShortSubstring {
        /// Normalized character count.
        length: usize,
    },
    /// Candidate limit is zero or exceeds the hard experiment ceiling.
    #[error("candidate limit {requested} is outside 1..={maximum}")]
    CandidateLimit {
        /// Requested candidate window.
        requested: usize,
        /// Hard maximum accepted by the experiment.
        maximum: usize,
    },
}

/// Cost controls applied identically to all compared strategies.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidatePolicy {
    /// Maximum candidates passed to verification.
    pub candidate_limit: usize,
}

impl Default for CandidatePolicy {
    fn default() -> Self {
        Self {
            candidate_limit: 300,
        }
    }
}

/// Backend-neutral query plan used by the experiment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlan {
    /// Canonical query used by retrieval, verification, and ranking.
    pub normalized_query: String,
    /// Whether n-gram retrieval may be used for this query.
    pub allow_ngram: bool,
    /// Hard candidate window.
    pub candidate_limit: usize,
}

impl CandidatePolicy {
    /// Plans a query and rejects expensive shapes before backend execution.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] for empty input, invalid candidate limits, or
    /// forced one/two-character substring queries.
    pub fn plan(self, query: &str, mode: QueryMode) -> Result<QueryPlan, PolicyError> {
        const HARD_MAXIMUM: usize = 500;
        if self.candidate_limit == 0 || self.candidate_limit > HARD_MAXIMUM {
            return Err(PolicyError::CandidateLimit {
                requested: self.candidate_limit,
                maximum: HARD_MAXIMUM,
            });
        }

        let normalized_query = normalize_search_text(query);
        let length = normalized_query.chars().count();
        if length == 0 {
            return Err(PolicyError::EmptyQuery);
        }
        if mode == QueryMode::SubstringOnly && length < 3 {
            return Err(PolicyError::ExpensiveShortSubstring { length });
        }

        Ok(QueryPlan {
            normalized_query,
            allow_ngram: length >= 3,
            candidate_limit: self.candidate_limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CandidatePolicy, PolicyError, QueryMode};

    #[test]
    fn rejects_expensive_short_substring_with_typed_error() {
        let error = CandidatePolicy::default()
            .plan("ab", QueryMode::SubstringOnly)
            .expect_err("two-character substring must be rejected");
        assert_eq!(error, PolicyError::ExpensiveShortSubstring { length: 2 });
    }

    #[test]
    fn permits_short_product_query_without_ngram_expansion() -> Result<(), PolicyError> {
        let plan = CandidatePolicy::default().plan("a", QueryMode::ProductSearch)?;
        assert!(!plan.allow_ngram);
        Ok(())
    }
}
