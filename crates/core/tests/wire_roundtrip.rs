use std::error::Error;

use localsearch_core::{
    AGENT_PROTOCOL_VERSION, Availability, CatalogDocument, CatalogIdentity, DocumentId,
    DocumentVersion, FileId128, FileKey, FileKind, FileLinkId, FileLinkSnapshot, FileMetadata,
    FileObjectSnapshot, FilesystemEvent, IndexGeneration, IndexMutation, MatchType, MutationBatch,
    MutationSeq, RankingVersion, ReconciliationReason, SearchFilter, SearchHit, SearchRequest,
    SearchResponse, SearchScope, SequencedMutation, VolumeId,
};
use serde::{Serialize, de::DeserializeOwned};

fn round_trip<T>(value: &T) -> Result<T, serde_json::Error>
where
    T: DeserializeOwned + Serialize,
{
    let encoded = serde_json::to_vec(value)?;
    serde_json::from_slice(&encoded)
}

fn identity() -> CatalogIdentity {
    CatalogIdentity::new(
        FileKey::new(VolumeId::from_u128(0x10), FileId128::from_u128(0x20)),
        FileLinkId::from_u128(0x30),
        DocumentId::from_u128(0x40),
    )
}

fn document() -> CatalogDocument {
    CatalogDocument {
        identity: identity(),
        document_version: DocumentVersion(7),
        name: "Architecture.md".to_owned(),
        resolved_path: "C:\\Projects\\LocalSearch\\Architecture.md".to_owned(),
        extension: Some("md".to_owned()),
        metadata: FileMetadata {
            kind: FileKind::File,
            size: 18_423,
            created_at_unix_ms: None,
            modified_at_unix_ms: Some(1_786_662_000_000),
            hidden: false,
            availability: Availability::Online,
        },
    }
}

#[test]
fn search_request_has_a_stable_json_contract() -> Result<(), Box<dyn Error>> {
    let request = SearchRequest {
        query: "architecture".to_owned(),
        scope: SearchScope::Files,
        filters: SearchFilter {
            extensions: vec!["md".to_owned(), "pdf".to_owned()],
            directory_prefix: Some("C:/Projects".to_owned()),
            minimum_size: Some(10),
            maximum_size: Some(4096),
        },
        top_k: 20,
    };

    let encoded = serde_json::to_string(&request)?;
    assert_eq!(
        encoded,
        concat!(
            "{\"query\":\"architecture\",\"scope\":\"files\",\"filters\":{",
            "\"extensions\":[\"md\",\"pdf\"],",
            "\"directory_prefix\":\"C:/Projects\",",
            "\"minimum_size\":10,\"maximum_size\":4096},\"top_k\":20}"
        )
    );
    assert_eq!(round_trip(&request)?, request);

    Ok(())
}

#[test]
fn search_response_round_trip_preserves_identity_not_backend_scores() -> Result<(), Box<dyn Error>>
{
    let identity = identity();
    let response = SearchResponse {
        index_generation: IndexGeneration(9),
        took_micros: 18_000,
        hits: vec![SearchHit {
            document_id: identity.document_id,
            object_key: identity.object_key,
            file_link_id: identity.file_link_id,
            name: "Architecture.md".to_owned(),
            resolved_path: "C:\\Projects\\LocalSearch\\Architecture.md".to_owned(),
            extension: Some("md".to_owned()),
            kind: FileKind::File,
            size: 18_423,
            modified_at_unix_ms: Some(1_786_662_000_000),
            availability: Availability::Online,
            match_type: MatchType::PrefixName,
            rank: 1,
            ranking_version: RankingVersion::new(1),
        }],
    };

    assert_eq!(round_trip(&response)?, response);
    let value = serde_json::to_value(&response)?;
    let hit = &value["hits"][0];
    assert!(hit.get("score").is_none());
    assert_eq!(hit["rank"], 1);
    assert_eq!(hit["match_type"], "prefix_name");

    Ok(())
}

#[test]
fn mutation_batch_round_trip_and_sequence_contract() -> Result<(), Box<dyn Error>> {
    let document = document();
    let batch = MutationBatch {
        mutations: vec![
            SequencedMutation {
                sequence: MutationSeq(41),
                mutation: IndexMutation::Upsert { document },
            },
            SequencedMutation {
                sequence: MutationSeq(42),
                mutation: IndexMutation::Delete {
                    document_id: identity().document_id,
                    document_version: DocumentVersion(8),
                },
            },
        ],
    };

    batch.validate()?;
    assert_eq!(batch.first_sequence(), Some(MutationSeq(41)));
    assert_eq!(batch.last_sequence(), Some(MutationSeq(42)));
    assert_eq!(round_trip(&batch)?, batch);

    Ok(())
}

#[test]
fn mutation_batch_rejects_gaps_and_empty_batches() {
    let empty = MutationBatch { mutations: vec![] };
    assert!(empty.validate().is_err());

    let gap = MutationBatch {
        mutations: vec![
            SequencedMutation {
                sequence: MutationSeq(1),
                mutation: IndexMutation::Delete {
                    document_id: DocumentId::from_u128(1),
                    document_version: DocumentVersion(1),
                },
            },
            SequencedMutation {
                sequence: MutationSeq(3),
                mutation: IndexMutation::Delete {
                    document_id: DocumentId::from_u128(2),
                    document_version: DocumentVersion(1),
                },
            },
        ],
    };
    assert!(gap.validate().is_err());
}

#[test]
fn domain_and_agent_versions_are_independent_types() {
    assert_eq!(localsearch_core::DOMAIN_SCHEMA_VERSION.get(), 1);
    assert_eq!(AGENT_PROTOCOL_VERSION.get(), 1);

    let domain_type_name = std::any::type_name_of_val(&localsearch_core::DOMAIN_SCHEMA_VERSION);
    let agent_type_name = std::any::type_name_of_val(&AGENT_PROTOCOL_VERSION);
    assert_ne!(domain_type_name, agent_type_name);
}

#[test]
fn filesystem_events_round_trip_without_native_platform_vocabulary() -> Result<(), Box<dyn Error>> {
    let object_key = identity().object_key;
    let events = vec![
        FilesystemEvent::ObjectObserved {
            object: FileObjectSnapshot {
                object_key,
                metadata: document().metadata,
            },
        },
        FilesystemEvent::LinkObserved {
            link: FileLinkSnapshot {
                file_link_id: identity().file_link_id,
                object_key,
                parent_key: None,
                name: "Architecture.md".to_owned(),
            },
        },
        FilesystemEvent::ReconciliationRequired {
            volume_id: object_key.volume_id,
            reason: ReconciliationReason::SourceHistoryUnavailable,
        },
    ];

    let encoded = serde_json::to_string(&events)?;
    assert_eq!(
        serde_json::from_str::<Vec<FilesystemEvent>>(&encoded)?,
        events
    );
    for native_term in ["mft", "usn", "fsevent", "inotify", "inode"] {
        assert!(!encoded.contains(native_term));
    }

    Ok(())
}
