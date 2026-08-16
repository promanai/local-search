use std::collections::BTreeSet;

use localsearch_agent_api::{
    AGENT_API_VERSION, AGENT_CODEC_VERSION, AgentErrorCode, AgentRequest, AgentResponse,
    Capability, RequestOperation, ResponsePayload, WireContractError, decode_frame, encode_frame,
};
use localsearch_core::{SearchFilter, SearchRequest, SearchScope};

fn search_request() -> AgentRequest {
    AgentRequest {
        protocol_version: AGENT_API_VERSION,
        codec_version: AGENT_CODEC_VERSION,
        request_id: "contract-1".to_owned(),
        deadline_ms: 500,
        operation: RequestOperation::CatalogSearch(SearchRequest {
            query: "architecture".to_owned(),
            scope: SearchScope::All,
            filters: SearchFilter::default(),
            top_k: 25,
        }),
    }
}

#[test]
fn request_frame_round_trip_is_stable() {
    let request = search_request();
    let frame = encode_frame(&request).expect("encode");
    let decoded: AgentRequest = decode_frame(&frame).expect("decode");
    assert_eq!(decoded, request);
    decoded.validate().expect("valid contract");
}

#[test]
fn oversized_declared_frame_fails_before_payload_allocation() {
    let declared = u32::try_from(localsearch_agent_api::MAX_FRAME_BYTES + 1).expect("u32");
    let error = decode_frame::<AgentRequest>(&declared.to_le_bytes()).expect_err("bounded");
    assert!(matches!(error, WireContractError::FrameTooLarge));
}

#[test]
fn capability_names_are_stable_and_keep_content_explicit() {
    let encoded = serde_json::to_string(&BTreeSet::from([
        Capability::SearchCatalog,
        Capability::SearchContent,
        Capability::ReadMetadata,
        Capability::IndexStatus,
    ]))
    .expect("capabilities");
    assert_eq!(
        encoded,
        "[\"search.catalog\",\"search.content\",\"read.metadata\",\"index.status\"]"
    );
    assert!(!encoded.contains("admin"));
    assert!(encoded.contains("search.content"));
}

#[test]
fn unsupported_version_and_query_policy_are_rejected() {
    let mut request = search_request();
    request.protocol_version = localsearch_core::AgentProtocolVersion::new(99);
    assert!(matches!(
        request.validate(),
        Err(WireContractError::UnsupportedProtocolVersion)
    ));
    request.protocol_version = AGENT_API_VERSION;
    let RequestOperation::CatalogSearch(search) = &mut request.operation else {
        panic!("search")
    };
    search.query.clear();
    assert!(matches!(
        request.validate(),
        Err(WireContractError::InvalidRequest(_))
    ));
}

#[test]
fn response_union_requires_exactly_one_arm() {
    let success =
        AgentResponse::success("one".to_owned(), ResponsePayload::CatalogItems(Vec::new()));
    success.validate().expect("success union");
    let failure = AgentResponse::failure(
        "two".to_owned(),
        AgentErrorCode::Forbidden,
        "capability denied",
    );
    failure.validate().expect("error union");
    let invalid = AgentResponse {
        protocol_version: AGENT_API_VERSION,
        request_id: "three".to_owned(),
        result: None,
        error: None,
    };
    assert!(matches!(
        invalid.validate(),
        Err(WireContractError::InvalidResponse)
    ));
}

#[test]
fn unknown_additive_fields_are_ignored() {
    let request = search_request();
    let mut json = serde_json::to_value(request).expect("json");
    json.as_object_mut()
        .expect("object")
        .insert("future_optional".to_owned(), serde_json::json!(true));
    let decoded: AgentRequest = serde_json::from_value(json).expect("forward additive field");
    decoded.validate().expect("still valid");
}

#[test]
fn checked_in_contract_manifest_matches_compiled_limits() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../../contracts/agent-wire-v2.json"))
            .expect("contract manifest");
    assert_eq!(manifest["agent_api_version"], AGENT_API_VERSION.get());
    assert_eq!(
        manifest["codec"]["version"],
        serde_json::json!(AGENT_CODEC_VERSION)
    );
    assert_eq!(
        manifest["codec"]["maximum_frame_bytes"],
        serde_json::json!(localsearch_agent_api::MAX_FRAME_BYTES)
    );
    assert_eq!(
        manifest["limits"]["maximum_top_k"],
        serde_json::json!(localsearch_agent_api::MAX_TOP_K)
    );
    assert_eq!(
        manifest["methods"]["catalog_search"],
        serde_json::json!("search.catalog")
    );
    assert_eq!(
        manifest["methods"]["content_search"],
        serde_json::json!("search.content")
    );
}
