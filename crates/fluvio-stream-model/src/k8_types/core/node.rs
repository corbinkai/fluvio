use serde::Deserialize;
use serde::Serialize;

use super::super::Crd;
use super::super::CrdNames;
use super::super::DefaultHeader;
use super::super::Spec;
use super::super::Status;

const NODE_API: Crd = Crd {
    group: "core",
    version: "v1",
    names: CrdNames {
        kind: "Node",
        plural: "nodes",
        singular: "node",
    },
};

impl Spec for NodeSpec {
    type Status = NodeStatus;
    type Header = DefaultHeader;
    const NAME_SPACED: bool = false;

    fn metadata() -> &'static Crd {
        &NODE_API
    }
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeSpec {
    #[serde(rename = "providerID")]
    pub provider_id: String,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeStatus {
    pub addresses: Vec<NodeAddress>,
}

impl Status for NodeStatus {}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeList {}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeAddress {
    pub address: String,
    pub r#type: String,
}
