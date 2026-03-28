//!
//! # Clear object (non-destructive data truncation)
//!

use std::fmt::Debug;
use std::io::Error as IoError;

use anyhow::Result;

use fluvio_protocol::{Encoder, Decoder, Version};
use fluvio_protocol::bytes::Buf;
use fluvio_protocol::api::Request;

use crate::{DeletableAdminSpec, TryEncodableFrom};
use crate::Status;
use crate::AdminPublicApiKey;
use super::{COMMON_VERSION, TypeBuffer};

/// Request to clear all data from a topic without deleting it.
/// Preserves topic metadata and consumer connections.
#[derive(Debug, Default, Encoder, Decoder)]
pub struct ClearRequest<S: DeletableAdminSpec> {
    key: S::DeleteKey,
    reset_high_watermark: bool,
}

impl<S> ClearRequest<S>
where
    S: DeletableAdminSpec,
{
    pub fn new(key: S::DeleteKey) -> Self {
        Self {
            key,
            reset_high_watermark: false,
        }
    }

    pub fn with_reset_hw(key: S::DeleteKey, reset_hw: bool) -> Self {
        Self {
            key,
            reset_high_watermark: reset_hw,
        }
    }

    pub fn key(self) -> S::DeleteKey {
        self.key
    }

    pub fn reset_high_watermark(&self) -> bool {
        self.reset_high_watermark
    }
}

#[derive(Debug, Default, Encoder)]
pub struct ObjectApiClearRequest(TypeBuffer);

impl Decoder for ObjectApiClearRequest {
    fn decode<T>(&mut self, src: &mut T, version: Version) -> Result<(), IoError>
    where
        T: Buf,
    {
        self.0.decode(src, version)?;
        Ok(())
    }
}

impl<S> TryEncodableFrom<ClearRequest<S>> for ObjectApiClearRequest
where
    S: DeletableAdminSpec,
{
    fn try_encode_from(input: ClearRequest<S>, version: Version) -> Result<Self> {
        Ok(Self(TypeBuffer::encode::<S, _>(input, version)?))
    }

    fn downcast(&self) -> Result<Option<ClearRequest<S>>> {
        self.0.downcast::<S, _>()
    }
}

impl Request for ObjectApiClearRequest {
    const API_KEY: u16 = AdminPublicApiKey::Clear as u16;
    const MIN_API_VERSION: i16 = 1;
    const DEFAULT_API_VERSION: i16 = COMMON_VERSION;
    type Response = Status;
}
