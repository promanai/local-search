#![deny(unsafe_op_in_unsafe_fn)]

//! Per-user Agent application service. Transport adapters call [`AgentService::dispatch`].

mod observation;
mod service;

#[cfg(windows)]
pub use localsearch_local_transport::windows_pipe;

pub use observation::{
    BrokerObservationController, ObservationError, ObservationSelection, ObservationSource,
    ObservationStep,
};
pub use service::{AgentService, ClientAuthorization};
