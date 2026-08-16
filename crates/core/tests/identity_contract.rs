use std::{collections::HashSet, error::Error};

use localsearch_core::{
    Availability, CatalogDocument, CatalogIdentity, DocumentId, DocumentVersion, FileId128,
    FileKey, FileKind, FileLinkId, FileMetadata, MachineId, VolumeId,
};

#[test]
fn canonical_ids_have_typed_stable_wire_forms() -> Result<(), Box<dyn Error>> {
    let file_id = FileId128::from_bytes([
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ]);

    assert_eq!(file_id.to_string(), "file:000102030405060708090a0b0c0d0e0f");
    assert_eq!(
        "file:000102030405060708090A0B0C0D0E0F".parse::<FileId128>()?,
        file_id
    );
    assert!(
        "volume:000102030405060708090a0b0c0d0e0f"
            .parse::<FileId128>()
            .is_err()
    );

    let encoded = serde_json::to_string(&file_id)?;
    assert_eq!(encoded, "\"file:000102030405060708090a0b0c0d0e0f\"");
    assert_eq!(serde_json::from_str::<FileId128>(&encoded)?, file_id);

    Ok(())
}

#[test]
fn file_key_wire_contract_does_not_use_a_path_or_drive_letter() -> Result<(), Box<dyn Error>> {
    let key = FileKey::new(VolumeId::from_u128(0x10), FileId128::from_u128(0x20));
    let encoded = serde_json::to_string(&key)?;

    assert_eq!(
        encoded,
        concat!(
            "{\"volume_id\":\"volume:00000000000000000000000000000010\",",
            "\"file_id\":\"file:00000000000000000000000000000020\"}"
        )
    );
    assert_eq!(serde_json::from_str::<FileKey>(&encoded)?, key);

    Ok(())
}

#[test]
fn catalog_identity_is_stable_when_name_and_path_change() {
    let identity = CatalogIdentity::new(
        FileKey::new(VolumeId::from_u128(1), FileId128::from_u128(2)),
        FileLinkId::from_u128(3),
        DocumentId::from_u128(4),
    );
    let mut document = CatalogDocument {
        identity,
        document_version: DocumentVersion(1),
        name: "old-name.md".to_owned(),
        resolved_path: "C:\\Old\\old-name.md".to_owned(),
        extension: Some("md".to_owned()),
        metadata: FileMetadata {
            kind: FileKind::File,
            size: 42,
            created_at_unix_ms: None,
            modified_at_unix_ms: Some(1_786_662_000_000),
            hidden: false,
            availability: Availability::Online,
        },
    };

    document.name = "new-name.md".to_owned();
    document.resolved_path = "D:\\New\\new-name.md".to_owned();
    document.document_version = DocumentVersion(2);

    assert_eq!(document.identity(), identity);
}

#[test]
fn hard_links_share_an_object_but_keep_distinct_link_and_document_ids() {
    let object_key = FileKey::new(VolumeId::from_u128(1), FileId128::from_u128(2));
    let first = CatalogIdentity::new(
        object_key,
        FileLinkId::from_u128(10),
        DocumentId::from_u128(20),
    );
    let second = CatalogIdentity::new(
        object_key,
        FileLinkId::from_u128(11),
        DocumentId::from_u128(21),
    );

    assert_eq!(first.object_key, second.object_key);
    assert_ne!(first.file_link_id, second.file_link_id);
    assert_ne!(first.document_id, second.document_id);

    let identities = HashSet::from([first, second]);
    assert_eq!(identities.len(), 2);
}

#[test]
fn every_canonical_id_type_rejects_cross_type_values() {
    let machine = MachineId::from_u128(1).to_string();
    let volume = VolumeId::from_u128(1).to_string();
    let link = FileLinkId::from_u128(1).to_string();
    let document = DocumentId::from_u128(1).to_string();

    assert!(machine.parse::<VolumeId>().is_err());
    assert!(volume.parse::<MachineId>().is_err());
    assert!(link.parse::<DocumentId>().is_err());
    assert!(document.parse::<FileLinkId>().is_err());
}
