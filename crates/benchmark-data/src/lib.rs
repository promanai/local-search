//! Deterministic, versioned synthetic catalog data for engineering spikes.
//!
//! Records are generated independently from `(seed, ordinal)`, so callers can
//! stream multi-million-record datasets without retaining them in memory.

use serde::{Deserialize, Serialize};

/// Current dataset semantics. Increment this when generation changes.
pub const DATASET_VERSION: u32 = 1;
/// Current catalog retrieval workload semantics.
pub const RETRIEVAL_WORKLOAD_VERSION: u32 = 1;
/// Stable dataset name used in benchmark reports.
pub const DATASET_NAME: &str = "synthetic-catalog";

const EXTENSIONS: &[&str] = &[
    "txt", "pdf", "docx", "xlsx", "jpg", "png", "rs", "toml", "json", "ts", "tsx", "js", "py",
    "md", "zip", "mp4", "log", "csv", "dll", "exe",
];
const ASCII_STEMS: &[&str] = &[
    "report",
    "invoice",
    "project",
    "meeting",
    "notes",
    "archive",
    "backup",
    "photo",
    "document",
    "presentation",
    "budget",
    "contract",
    "release",
    "readme",
    "config",
    "source",
    "customer",
    "analysis",
    "design",
    "schedule",
];
const UNICODE_STEMS: &[&str] = &[
    "отчёт",
    "договор",
    "проект",
    "заметки",
    "фотография",
    "résumé",
    "café",
    "überblick",
    "東京",
    "資料",
    "δοκιμή",
    "mañana",
];
const DIRECTORIES: &[&str] = &[
    "Users",
    "Documents",
    "Downloads",
    "Pictures",
    "Projects",
    "src",
    "target",
    "node_modules",
    ".git",
    "vendor",
    "archive",
    "clients",
    "2024",
    "2025",
    "2026",
    "Работа",
    "Документы",
    "Фото",
    "資料",
    "shared",
];

/// One generated catalog record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyntheticRecord {
    /// Zero-based deterministic ordinal.
    pub ordinal: u64,
    /// File name including extension.
    pub name: String,
    /// Full synthetic path using `/` as a benchmark-only separator.
    pub path: String,
    /// Extension without a dot.
    pub extension: String,
    /// Deterministic size in bytes.
    pub size: u64,
    /// Deterministic last-modified Unix timestamp in milliseconds.
    pub modified_at_unix_ms: i64,
}

/// Expected retrieval class for a generated query.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// Full normalized file name.
    Exact,
    /// A complete filename token.
    Token,
    /// Leading characters of a filename token.
    Prefix,
}

/// One deterministic measured query.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyntheticQuery {
    /// Query text before the experimental planner normalizes it.
    pub text: String,
    /// Retrieval operation exercised by the query.
    pub kind: QueryKind,
    /// Ordinal guaranteed to be relevant for this query.
    pub expected_ordinal: u64,
}

/// Stateless catalog generator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogGenerator {
    seed: u64,
}

impl CatalogGenerator {
    /// Creates a generator for a fixed seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Returns one record without depending on generation order.
    #[must_use]
    pub fn record(&self, ordinal: u64) -> SyntheticRecord {
        let mut random = SplitMix64::new(self.seed ^ ordinal.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let unicode = ordinal.is_multiple_of(29);
        let stem_pool = if unicode { UNICODE_STEMS } else { ASCII_STEMS };
        let stem = pick(stem_pool, &mut random);
        let qualifier = pick(ASCII_STEMS, &mut random);
        let extension = weighted_extension(&mut random).to_owned();
        let name = if ordinal.is_multiple_of(11) {
            format!("{stem}_{qualifier}_{ordinal:08}.{extension}")
        } else if ordinal.is_multiple_of(7) {
            format!("{stem}-{ordinal:08}.{extension}")
        } else {
            format!("{stem}_{:04}.{extension}", ordinal % 10_000)
        };

        let depth = 2 + usize::try_from(random.next() % 6).unwrap_or(2);
        let mut path = String::from("/catalog");
        for _ in 0..depth {
            path.push('/');
            path.push_str(pick(DIRECTORIES, &mut random));
        }
        path.push('/');
        path.push_str(&name);

        SyntheticRecord {
            ordinal,
            name,
            path,
            extension,
            size: 128 + random.next() % (8 * 1_024 * 1_024 * 1_024),
            modified_at_unix_ms: 1_577_836_800_000
                + i64::try_from(random.next() % 220_752_000_000).unwrap_or(0),
        }
    }

    /// Streams exactly `count` records in ordinal order.
    pub fn records(&self, count: u64) -> impl Iterator<Item = SyntheticRecord> + '_ {
        (0..count).map(|ordinal| self.record(ordinal))
    }

    /// Builds a stable workload with an equal number of exact, token, and prefix queries.
    #[must_use]
    pub fn retrieval_queries(
        &self,
        record_count: u64,
        queries_per_kind: usize,
    ) -> Vec<SyntheticQuery> {
        if record_count == 0 {
            return Vec::new();
        }
        let mut queries = Vec::with_capacity(queries_per_kind.saturating_mul(3));
        for index in 0..queries_per_kind {
            let index_u64 = u64::try_from(index).unwrap_or(u64::MAX);
            let ordinal = mix(self.seed.wrapping_add(index_u64)) % record_count;
            let record = self.record(ordinal);
            let stem = record
                .name
                .split(['_', '-', '.'])
                .next()
                .unwrap_or(&record.name);
            let prefix_len = stem.chars().count().clamp(1, 4);
            let prefix: String = stem.chars().take(prefix_len).collect();
            queries.push(SyntheticQuery {
                text: record.name.clone(),
                kind: QueryKind::Exact,
                expected_ordinal: ordinal,
            });
            queries.push(SyntheticQuery {
                text: stem.to_owned(),
                kind: QueryKind::Token,
                expected_ordinal: ordinal,
            });
            queries.push(SyntheticQuery {
                text: prefix,
                kind: QueryKind::Prefix,
                expected_ordinal: ordinal,
            });
        }
        queries
    }
}

fn weighted_extension(random: &mut SplitMix64) -> &'static str {
    // Squaring biases the deterministic draw toward common entries at the front.
    let draw = random.next() % 10_000;
    let scaled = draw.saturating_mul(draw) / 10_000;
    let index = usize::try_from(scaled).unwrap_or(0) * EXTENSIONS.len() / 10_000;
    EXTENSIONS[index.min(EXTENSIONS.len() - 1)]
}

fn pick<'a>(items: &'a [&'a str], random: &mut SplitMix64) -> &'a str {
    let index =
        usize::try_from(random.next() % u64::try_from(items.len()).unwrap_or(1)).unwrap_or(0);
    items[index]
}

const fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = mix(self.0);
        self.0
    }
}
