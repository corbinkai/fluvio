use serde::Deserialize;
use serde::Serialize;

use crate::default_store_spec;
use super::super::Crd;
use super::super::CrdNames;
use super::super::DefaultHeader;
use super::super::Spec;
use super::super::Status;

const API: Crd = Crd {
    group: "core",
    version: "v1",
    names: CrdNames {
        kind: "ServiceAccount",
        plural: "serviceaccounts",
        singular: "serviceaccount",
    },
};

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceAccountSpec {}

impl Spec for ServiceAccountSpec {
    type Status = ServiceAccountStatus;
    type Header = DefaultHeader;
    fn metadata() -> &'static Crd {
        &API
    }
}

default_store_spec!(ServiceAccountSpec, ServiceAccountStatus, "ServiceAccount");

#[derive(Deserialize, Serialize, Eq, PartialEq, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct ServiceAccountStatus {}

impl Status for ServiceAccountStatus {}
