//! Reusable behavioral contract for filesystem-provider implementations.

mod provider_contract;

pub use provider_contract::{
    ContractMutation, ProviderContractFixture, ProviderContractReport, run_provider_contract,
};
