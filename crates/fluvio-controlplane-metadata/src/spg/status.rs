#![allow(clippy::assign_op_pattern)]

use std::fmt;

use fluvio_protocol::{Encoder, Decoder};

#[derive(Encoder, Decoder, Default, Debug, Clone, Eq, PartialEq)]
#[cfg_attr(
    feature = "use_serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "camelCase")
)]
pub struct SpuGroupStatus {
    /// Status resolution
    pub resolution: SpuGroupStatusResolution,

    /// Reason for Status resolution (if applies)
    pub reason: Option<String>,
}

impl fmt::Display for SpuGroupStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#?}", self.resolution)
    }
}

impl SpuGroupStatus {
    pub fn invalid(reason: String) -> Self {
        Self {
            resolution: SpuGroupStatusResolution::Invalid,
            reason: Some(reason),
        }
    }

    pub fn reserved() -> Self {
        Self {
            resolution: SpuGroupStatusResolution::Reserved,
            ..Default::default()
        }
    }

    pub fn is_already_valid(&self) -> bool {
        self.resolution == SpuGroupStatusResolution::Reserved
    }
}

#[cfg_attr(feature = "use_serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Encoder, Decoder, Debug, Clone, Eq, PartialEq, Default)]
pub enum SpuGroupStatusResolution {
    #[fluvio(tag = 0)]
    #[default]
    Init,
    #[fluvio(tag = 1)]
    Invalid,
    #[fluvio(tag = 2)]
    Reserved,
}

// -----------------------------------
// Implementation - FlvSpuGroupResolution
// -----------------------------------

impl fmt::Display for SpuGroupStatusResolution {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Init => write!(f, "Init"),
            Self::Invalid => write!(f, "Invalid"),
            Self::Reserved => write!(f, "Reserved"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spugroup_status_default_is_init() {
        let status = SpuGroupStatus::default();
        assert_eq!(status.resolution, SpuGroupStatusResolution::Init);
        assert_eq!(status.reason, None);
    }

    #[test]
    fn test_spugroup_status_invalid_has_reason() {
        let status = SpuGroupStatus::invalid("conflict with id 5".to_string());
        assert_eq!(status.resolution, SpuGroupStatusResolution::Invalid);
        assert_eq!(status.reason, Some("conflict with id 5".to_string()));
    }

    #[test]
    fn test_spugroup_status_reserved() {
        let status = SpuGroupStatus::reserved();
        assert_eq!(status.resolution, SpuGroupStatusResolution::Reserved);
        assert_eq!(status.reason, None);
    }

    #[test]
    fn test_is_already_valid_returns_true_for_reserved() {
        assert!(SpuGroupStatus::reserved().is_already_valid());
    }

    #[test]
    fn test_is_already_valid_returns_false_for_init() {
        assert!(!SpuGroupStatus::default().is_already_valid());
    }

    #[test]
    fn test_is_already_valid_returns_false_for_invalid() {
        assert!(!SpuGroupStatus::invalid("x".to_string()).is_already_valid());
    }
}
