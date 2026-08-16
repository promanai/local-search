use localsearch_core::{FileLinkId, FilesystemEvent, VolumeId};
use localsearch_platform_core::{
    PlatformError, PlatformErrorKind, PlatformResult, ProviderCheckpoint,
};
use serde::{Deserialize, Serialize};

pub(crate) const PROVIDER_ID: &str = "localsearch.windows-fs";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PendingRename {
    pub file_reference: u64,
    pub file_link_id: FileLinkId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct KnownLinkIdentity {
    pub parent_reference: u64,
    pub name: String,
    pub file_link_id: FileLinkId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct WindowsCheckpoint {
    pub journal_identity: u64,
    pub next_position: i64,
    pub lowest_valid_position: i64,
    pub snapshot_generation: u64,
    #[serde(default)]
    pub pending_events: Vec<FilesystemEvent>,
    #[serde(default)]
    pub pending_renames: Vec<PendingRename>,
    #[serde(default)]
    pub known_links: Vec<KnownLinkIdentity>,
}

pub(crate) fn encode(
    volume_id: VolumeId,
    value: &WindowsCheckpoint,
) -> PlatformResult<ProviderCheckpoint> {
    let opaque = serde_json::to_vec(value).map_err(|error| {
        PlatformError::new(
            PlatformErrorKind::Internal,
            "encode_checkpoint",
            error.to_string(),
        )
    })?;
    Ok(ProviderCheckpoint {
        provider_id: PROVIDER_ID.to_owned(),
        format_version: 3,
        volume_id,
        opaque,
    })
}

pub(crate) fn decode(value: &ProviderCheckpoint) -> PlatformResult<WindowsCheckpoint> {
    if value.provider_id != PROVIDER_ID || !matches!(value.format_version, 2 | 3) {
        return Err(PlatformError::new(
            PlatformErrorKind::InvalidCheckpoint,
            "decode_checkpoint",
            "provider or format mismatch",
        ));
    }
    let mut decoded: WindowsCheckpoint =
        serde_json::from_slice(&value.opaque).map_err(|error| {
            PlatformError::new(
                PlatformErrorKind::InvalidCheckpoint,
                "decode_checkpoint",
                error.to_string(),
            )
        })?;
    decoded
        .pending_renames
        .sort_by_key(|pending| pending.file_reference);
    decoded.known_links.sort_by(|left, right| {
        (left.parent_reference, left.name.as_str())
            .cmp(&(right.parent_reference, right.name.as_str()))
    });
    Ok(decoded)
}

pub(crate) fn validate(
    saved: &WindowsCheckpoint,
    current_journal_identity: u64,
    current_lowest: i64,
) -> PlatformResult<()> {
    if saved.journal_identity != current_journal_identity || saved.next_position < current_lowest {
        return Err(PlatformError::new(
            PlatformErrorKind::SourceHistoryGap,
            "validate_checkpoint",
            "journal identity changed or cursor fell below retained history",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_v2_checkpoint_migrates_with_empty_rename_correlation() -> PlatformResult<()> {
        let checkpoint = ProviderCheckpoint {
            provider_id: PROVIDER_ID.to_owned(),
            format_version: 2,
            volume_id: VolumeId::from_u128(9),
            opaque: serde_json::to_vec(&serde_json::json!({
                "journal_identity": 7,
                "next_position": 100,
                "lowest_valid_position": 80,
                "snapshot_generation": 2,
                "pending_events": []
            }))
            .expect("legacy checkpoint JSON"),
        };
        let decoded = decode(&checkpoint)?;
        assert!(decoded.pending_renames.is_empty());
        assert!(decoded.known_links.is_empty());
        Ok(())
    }

    #[test]
    fn detects_recreated_or_truncated_history() -> PlatformResult<()> {
        let value = WindowsCheckpoint {
            journal_identity: 7,
            next_position: 100,
            lowest_valid_position: 80,
            snapshot_generation: 2,
            pending_events: Vec::new(),
            pending_renames: Vec::new(),
            known_links: Vec::new(),
        };
        validate(&value, 7, 80)?;
        assert_eq!(
            validate(&value, 8, 80)
                .expect_err("new journal must gap")
                .kind,
            PlatformErrorKind::SourceHistoryGap
        );
        assert_eq!(
            validate(&value, 7, 101)
                .expect_err("expired cursor must gap")
                .kind,
            PlatformErrorKind::SourceHistoryGap
        );
        Ok(())
    }
}
