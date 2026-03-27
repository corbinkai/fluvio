use serde::Deserialize;
use serde::Serialize;

use super::super::core::pod::PodSpec;
use super::super::{Crd, CrdNames, DefaultHeader, LabelSelector, Spec, Status, TemplateSpec};

const STATEFUL_API: Crd = Crd {
    group: "apps",
    version: "v1",
    names: CrdNames {
        kind: "StatefulSet",
        plural: "statefulsets",
        singular: "statefulset",
    },
};

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct StatefulSetSpec {
    pub pod_management_policy: Option<PodMangementPolicy>,
    pub replicas: Option<u16>,
    pub revision_history_limit: Option<u16>,
    pub selector: LabelSelector,
    pub service_name: String,
    pub template: TemplateSpec<PodSpec>,
    pub volume_claim_templates: Vec<TemplateSpec<PersistentVolumeClaim>>,
    pub update_strategy: Option<StatefulSetUpdateStrategy>,
}

impl Spec for StatefulSetSpec {
    type Status = StatefulSetStatus;
    type Header = DefaultHeader;

    fn metadata() -> &'static Crd {
        &STATEFUL_API
    }

    // statefulset doesnt' like to change volume claim template
    fn make_same(&mut self, other: &Self) {
        self.volume_claim_templates
            .clone_from(&other.volume_claim_templates)
    }
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatefulSetUpdateStrategy {
    pub _type: String,
    pub rolling_ipdate: Option<RollingUpdateStatefulSetStrategy>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RollingUpdateStatefulSetStrategy {
    partition: u32,
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone)]
pub enum PodMangementPolicy {
    OrderedReady,
    Parallel,
}

#[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistentVolumeClaim {
    pub access_modes: Vec<VolumeAccessMode>,
    pub storage_class_name: Option<String>,
    pub resources: ResourceRequirements,
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone)]
pub enum VolumeAccessMode {
    ReadWriteOnce,
    ReadWrite,
    ReadOnlyMany,
}

#[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq)]
pub struct ResourceRequirements {
    pub requests: VolumeRequest,
}

#[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq)]
pub struct VolumeRequest {
    pub storage: String,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatefulSetStatus {
    pub replicas: u16,
    pub collision_count: Option<u32>,
    #[serde(default)]
    pub conditions: Vec<StatefulSetCondition>,
    pub current_replicas: Option<u16>,
    pub current_revision: Option<String>,
    pub observed_generation: Option<u32>,
    pub ready_replicas: Option<u16>,
    pub update_revision: Option<String>,
    pub updated_replicas: Option<u16>,
}

impl Status for StatefulSetStatus {}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone)]
pub enum StatusEnum {
    True,
    False,
    Unknown,
}

#[derive(Deserialize, Serialize, Debug, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatefulSetCondition {
    pub message: String,
    pub reason: StatusEnum,
    pub status: String,
    #[serde(rename = "type")]
    pub _type: String,
}
