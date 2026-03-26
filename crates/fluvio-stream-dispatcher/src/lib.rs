pub mod store;
pub mod dispatcher;
pub mod actions;

mod error;
pub mod metadata;

pub use error::StoreError;

pub mod core {
    pub use fluvio_stream_model::core::*;
}

#[cfg(any(feature = "k8", feature = "kube"))]
pub use fluvio_stream_model::k8_types;
