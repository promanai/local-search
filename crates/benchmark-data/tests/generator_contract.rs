use localsearch_benchmark_data::{CatalogGenerator, DATASET_VERSION, QueryKind};

#[test]
fn same_seed_and_ordinal_are_stable() {
    let generator = CatalogGenerator::new(42);
    assert_eq!(generator.record(29), generator.record(29));
    assert_ne!(generator.record(29), CatalogGenerator::new(43).record(29));
    assert_eq!(DATASET_VERSION, 1);
}

#[test]
fn generation_is_order_independent() {
    let generator = CatalogGenerator::new(7);
    let expected = generator.record(999);
    let _discarded: Vec<_> = generator.records(50).collect();
    assert_eq!(expected, generator.record(999));
}

#[test]
fn dataset_contains_unicode_and_deep_realistic_paths() {
    let generator = CatalogGenerator::new(42);
    let records: Vec<_> = generator.records(100).collect();
    assert!(records.iter().any(|record| !record.name.is_ascii()));
    assert!(
        records
            .iter()
            .any(|record| record.path.matches('/').count() >= 6)
    );
    assert!(
        records
            .iter()
            .all(|record| record.path.ends_with(&record.name))
    );
}

#[test]
fn workload_is_balanced_and_points_to_relevant_records() {
    let generator = CatalogGenerator::new(42);
    let queries = generator.retrieval_queries(1_000, 8);
    assert_eq!(queries.len(), 24);
    for kind in [QueryKind::Exact, QueryKind::Token, QueryKind::Prefix] {
        assert_eq!(queries.iter().filter(|query| query.kind == kind).count(), 8);
    }
    for query in queries {
        let record = generator.record(query.expected_ordinal);
        let normalized_name = record.name.to_lowercase();
        assert!(normalized_name.contains(&query.text.to_lowercase()));
    }
}
