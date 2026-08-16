use crate::{CatalogRecord, normalize_search_text, tokenize};
use serde::{Deserialize, Serialize};

/// Stable primary product match class. Declaration order is ranking order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchClass {
    /// Whole normalized filename match.
    ExactName,
    /// Normalized filename starts with the query.
    PrefixName,
    /// One normalized filename token equals the query.
    TokenName,
    /// Verified normalized filename substring.
    SubstringName,
    /// Verified normalized path-only substring.
    Path,
}

/// Fully verified and deterministically ordered result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RankedHit {
    /// Shared dataset ordinal, used as the stable document identity.
    pub ordinal: u64,
    /// Original display filename.
    pub name: String,
    /// Original display path.
    pub path: String,
    /// Stable primary class.
    pub match_class: MatchClass,
}

/// Classifies a candidate after exact normalized substring verification.
#[must_use]
pub fn classify_match(
    normalized_name: &str,
    normalized_path: &str,
    normalized_query: &str,
) -> Option<MatchClass> {
    if normalized_name == normalized_query {
        Some(MatchClass::ExactName)
    } else if normalized_name.starts_with(normalized_query) {
        Some(MatchClass::PrefixName)
    } else if tokenize(normalized_name).contains(&normalized_query) {
        Some(MatchClass::TokenName)
    } else if normalized_name.contains(normalized_query) {
        Some(MatchClass::SubstringName)
    } else if normalized_path.contains(normalized_query) {
        Some(MatchClass::Path)
    } else {
        None
    }
}

/// Verifies and ranks candidate records with stable tie-breaking.
#[must_use]
pub fn rank_verified(candidates: Vec<CatalogRecord>, normalized_query: &str) -> Vec<RankedHit> {
    let mut hits: Vec<(String, String, RankedHit)> = candidates
        .into_iter()
        .filter_map(|record| {
            let normalized_name = normalize_search_text(&record.name);
            let normalized_path = normalize_search_text(&record.path);
            classify_match(&normalized_name, &normalized_path, normalized_query).map(
                |match_class| {
                    (
                        normalized_name,
                        normalized_path,
                        RankedHit {
                            ordinal: record.ordinal,
                            name: record.name,
                            path: record.path,
                            match_class,
                        },
                    )
                },
            )
        })
        .collect();

    hits.sort_by(|left, right| {
        left.2
            .match_class
            .cmp(&right.2.match_class)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.ordinal.cmp(&right.2.ordinal))
    });
    hits.into_iter().map(|(_, _, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::{MatchClass, rank_verified};
    use crate::CatalogRecord;

    fn record(ordinal: u64, name: &str, path: &str) -> CatalogRecord {
        CatalogRecord {
            ordinal,
            name: name.to_owned(),
            path: path.to_owned(),
            extension: "txt".to_owned(),
            size: 1,
            modified_at_unix_ms: 0,
        }
    }

    #[test]
    fn primary_class_dominates_and_ties_are_stable() {
        let candidates = vec![
            record(9, "xalphaq.txt", "/alpha/xalphaq.txt"),
            record(8, "x alpha report.txt", "/z/x alpha report.txt"),
            record(7, "alphabet.txt", "/z/alphabet.txt"),
            record(6, "alpha", "/z/alpha"),
            record(5, "other.txt", "/alpha/other.txt"),
            record(4, "xalphaq.txt", "/alpha/xalphaq.txt"),
        ];
        let ranked = rank_verified(candidates, "alpha");
        let classes: Vec<_> = ranked.iter().map(|hit| hit.match_class).collect();
        assert_eq!(
            classes,
            vec![
                MatchClass::ExactName,
                MatchClass::PrefixName,
                MatchClass::TokenName,
                MatchClass::SubstringName,
                MatchClass::SubstringName,
                MatchClass::Path,
            ]
        );
        assert_eq!(ranked[3].ordinal, 4);
        assert_eq!(ranked[4].ordinal, 9);
    }

    #[test]
    fn exact_verification_rejects_ngram_false_positive() {
        // "abcXXbcd" contains both trigrams of "abcd" but not the substring.
        let ranked = rank_verified(vec![record(1, "abcXXbcd.txt", "/z/abcXXbcd.txt")], "abcd");
        assert!(ranked.is_empty());
    }
}
