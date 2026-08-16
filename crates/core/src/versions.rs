use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! define_version {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(u32);

        impl $name {
            /// Creates a version value.
            #[must_use]
            pub const fn new(value: u32) -> Self {
                Self(value)
            }

            /// Returns the numeric version value.
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

define_version!(
    DomainSchemaVersion,
    "Version of canonical domain semantics."
);
define_version!(AgentProtocolVersion, "Version of the Agent wire contract.");
define_version!(RankingVersion, "Version of product ranking semantics.");

/// Canonical domain schema implemented by this crate.
pub const DOMAIN_SCHEMA_VERSION: DomainSchemaVersion = DomainSchemaVersion::new(1);

/// Agent protocol version for DTOs implemented by this crate.
pub const AGENT_PROTOCOL_VERSION: AgentProtocolVersion = AgentProtocolVersion::new(1);

/// Monotonic generation of a materialized index.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct IndexGeneration(pub u64);

/// Monotonic sequence in the durable mutation outbox.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MutationSeq(pub u64);

/// Logical version of a projected document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct DocumentVersion(pub u64);
