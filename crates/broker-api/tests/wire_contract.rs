use localsearch_broker_api::{
    BROKER_CODEC_VERSION, BROKER_PROTOCOL_VERSION, BrokerContractError, BrokerOperation,
    BrokerRequest, MAX_BROKER_DEADLINE_MS, MAX_BROKER_FRAME_BYTES, MAX_BROKER_PAGE_EVENTS,
    MAX_BROKER_REQUEST_ID_BYTES, decode_frame, encode_frame,
};
use localsearch_core::VolumeId;
use serde_json::{Value, json};

fn request(operation: BrokerOperation) -> BrokerRequest {
    BrokerRequest {
        protocol_version: BROKER_PROTOCOL_VERSION,
        codec_version: BROKER_CODEC_VERSION,
        request_id: "contract-1".to_owned(),
        deadline_ms: 5_000,
        operation,
    }
}

#[test]
fn request_round_trip_preserves_only_allowlisted_operation() {
    let request = request(BrokerOperation::StartScan {
        volume_id: VolumeId::from_u128(9),
        mode: localsearch_broker_api::ScanMode::Initial,
    });
    let encoded = encode_frame(&request).expect("encode");
    let decoded: BrokerRequest = decode_frame(&encoded).expect("decode");
    assert_eq!(decoded, request);
    assert!(decoded.validate().is_ok());
}

#[test]
fn unknown_versions_operations_and_hostile_lengths_fail_closed() {
    let mut wrong_version = request(BrokerOperation::BrokerGetCapabilities);
    wrong_version.protocol_version += 1;
    assert!(matches!(
        wrong_version.validate(),
        Err(BrokerContractError::UnsupportedProtocolVersion)
    ));

    let unknown = json!({
        "protocol_version": 1,
        "codec_version": 1,
        "request_id": "unknown-1",
        "deadline_ms": 1000,
        "method": "read_content",
        "params": {"path": "C:\\secret.txt"}
    });
    let frame = encode_frame(&unknown).expect("generic JSON frame");
    assert!(matches!(
        decode_frame::<BrokerRequest>(&frame),
        Err(BrokerContractError::InvalidJson(_))
    ));

    let declared = u32::try_from(MAX_BROKER_FRAME_BYTES + 1).expect("declared length");
    assert!(matches!(
        decode_frame::<BrokerRequest>(&declared.to_le_bytes()),
        Err(BrokerContractError::FrameTooLarge)
    ));
}

#[test]
fn checked_in_manifest_matches_compiled_limits_and_has_no_privileged_proxy_surface() {
    let manifest: Value =
        serde_json::from_str(include_str!("../../../contracts/broker-wire-v1.json"))
            .expect("manifest");
    assert_eq!(manifest["protocol_version"], BROKER_PROTOCOL_VERSION);
    assert_eq!(manifest["codec_version"], BROKER_CODEC_VERSION);
    assert_eq!(manifest["maximum_frame_bytes"], MAX_BROKER_FRAME_BYTES);
    assert_eq!(manifest["maximum_page_events"], MAX_BROKER_PAGE_EVENTS);
    assert_eq!(
        manifest["maximum_request_id_bytes"],
        MAX_BROKER_REQUEST_ID_BYTES
    );
    assert_eq!(manifest["maximum_deadline_ms"], MAX_BROKER_DEADLINE_MS);
    let operations = manifest["allowed_operations"]
        .as_array()
        .expect("operations");
    assert_eq!(operations.len(), 6);
    let joined = operations
        .iter()
        .map(|operation| operation.as_str().expect("operation"))
        .collect::<Vec<_>>()
        .join(" ");
    for forbidden in ["content", "path", "write", "execute", "admin", "search"] {
        assert!(
            !joined.contains(forbidden),
            "forbidden surface: {forbidden}"
        );
    }
}

#[test]
fn event_pages_and_deadlines_are_bounded_before_dispatch() {
    let too_many = request(BrokerOperation::ReadScanPage {
        scan_id: 1,
        maximum_events: MAX_BROKER_PAGE_EVENTS + 1,
    });
    assert!(matches!(
        too_many.validate(),
        Err(BrokerContractError::InvalidRequest(_))
    ));
    let mut late = request(BrokerOperation::DiscoverVolumes);
    late.deadline_ms = MAX_BROKER_DEADLINE_MS + 1;
    assert!(matches!(
        late.validate(),
        Err(BrokerContractError::InvalidRequest(_))
    ));
}
