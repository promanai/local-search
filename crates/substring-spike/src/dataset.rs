use localsearch_benchmark_data::{
    CatalogGenerator, DATASET_NAME, DATASET_VERSION, SyntheticRecord,
};
use serde::{Deserialize, Serialize};

/// Shared START-002 record type; START-003 does not fork its semantics.
pub type CatalogRecord = SyntheticRecord;

/// Dataset provenance written to every report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DatasetDescriptor {
    /// Shared dataset name.
    pub name: &'static str,
    /// Shared generator semantics version.
    pub version: u32,
    /// Deterministic generator seed.
    pub seed: u64,
    /// Number of ordinal records indexed.
    pub records: u64,
    /// START-003 workload semantics version.
    pub workload: &'static str,
}

/// Thin adapter around the unmodified shared benchmark generator.
#[derive(Clone, Debug)]
pub struct CatalogDataset {
    generator: CatalogGenerator,
    descriptor: DatasetDescriptor,
}

impl CatalogDataset {
    /// Creates a fixed ordinal range using the shared generator.
    #[must_use]
    pub const fn new(records: u64, seed: u64) -> Self {
        Self {
            generator: CatalogGenerator::new(seed),
            descriptor: DatasetDescriptor {
                name: DATASET_NAME,
                version: DATASET_VERSION,
                seed,
                records,
                workload: "substring-product-ranking-v1",
            },
        }
    }

    /// Returns report provenance.
    #[must_use]
    pub const fn descriptor(&self) -> &DatasetDescriptor {
        &self.descriptor
    }

    /// Streams the unchanged shared records in ordinal order.
    pub fn records(&self) -> impl Iterator<Item = CatalogRecord> + '_ {
        self.generator.records(self.descriptor.records)
    }

    /// Returns one unchanged shared record by ordinal.
    #[must_use]
    pub fn record(&self, ordinal: u64) -> CatalogRecord {
        self.generator.record(ordinal)
    }
}
