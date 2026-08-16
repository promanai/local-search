use localsearch_benchmark_data::{CatalogGenerator, QueryKind, SyntheticQuery};
use localsearch_catalog_spike::{CatalogIndex, EXPERIMENTAL_SCHEMA_ID};

#[test]
fn exact_token_prefix_and_unicode_are_retrievable() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let index = CatalogIndex::create(directory.path())?;
    let generator = CatalogGenerator::new(42);
    let mut writer = index.writer(20_000_000)?;
    for record in generator.records(2_000) {
        writer.add(&record)?;
    }
    writer.commit()?;
    writer.wait_merging_threads()?;

    let reader = index.reader()?;
    reader.reload()?;
    for query in generator.retrieval_queries(2_000, 20) {
        assert!(!reader.search(&query, 100)?.is_empty(), "query: {query:?}");
    }
    let unicode = generator.record(29);
    let exact_unicode = SyntheticQuery {
        text: unicode.name,
        kind: QueryKind::Exact,
        expected_ordinal: 29,
    };
    assert!(reader.search(&exact_unicode, 100)?.contains(&29));
    assert!(EXPERIMENTAL_SCHEMA_ID.starts_with("start002-"));
    Ok(())
}

#[test]
fn reader_snapshot_changes_only_after_explicit_reload() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let index = CatalogIndex::create(directory.path())?;
    let reader = index.reader()?;
    let generator = CatalogGenerator::new(9);
    let record = generator.record(1);
    let query = SyntheticQuery {
        text: record.name.clone(),
        kind: QueryKind::Exact,
        expected_ordinal: 1,
    };
    let mut writer = index.writer(20_000_000)?;
    writer.add(&record)?;
    writer.commit()?;
    assert!(reader.search(&query, 10)?.is_empty());
    reader.reload()?;
    assert_eq!(reader.search(&query, 10)?, vec![1]);
    writer.wait_merging_threads()?;
    Ok(())
}
