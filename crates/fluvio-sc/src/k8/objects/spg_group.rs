use std::{collections::HashMap, ops::Deref};

use tracing::{trace, instrument, debug};

use fluvio_controlplane_metadata::core::MetadataItem;
use fluvio_types::SpuId;
use fluvio_controlplane_metadata::{
    spg::SpuEndpointTemplate,
    spu::{Endpoint, IngressPort, SpuType},
};

use crate::stores::MetadataStoreObject;
use crate::stores::spg::SpuGroupSpec;
use crate::stores::spu::is_conflict;
use crate::stores::k8::K8MetaItem;
use crate::stores::spu::SpuSpec;
use crate::stores::LocalStore;
use crate::stores::actions::WSAction;

#[derive(Debug)]
pub struct SpuGroupObj {
    inner: MetadataStoreObject<SpuGroupSpec, K8MetaItem>,
    svc_name: String,
}

impl Deref for SpuGroupObj {
    type Target = MetadataStoreObject<SpuGroupSpec, K8MetaItem>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl SpuGroupObj {
    pub fn new(inner: MetadataStoreObject<SpuGroupSpec, K8MetaItem>) -> Self {
        let svc_name = format!("fluvio-spg-{}", inner.key());
        Self { inner, svc_name }
    }

    pub fn is_already_valid(&self) -> bool {
        self.status().is_already_valid()
    }

    pub async fn is_conflict_with(
        &self,
        spu_store: &LocalStore<SpuSpec, K8MetaItem>,
    ) -> Option<SpuId> {
        if self.is_already_valid() {
            return None;
        }

        let min_id = self.spec.min_id as SpuId;

        is_conflict(
            spu_store,
            self.ctx().item().uid().clone(),
            min_id,
            min_id + self.spec.replicas as SpuId,
        )
        .await
    }

    /// generate as SPU spec
    #[instrument(skip(self))]
    pub fn as_spu(
        &self,
        spu: u16,
        services: &HashMap<String, IngressPort>,
    ) -> (String, MetadataStoreObject<SpuSpec, K8MetaItem>) {
        let spec = self.spec();
        let spu_id = compute_spu_id(spec.min_id, spu);
        let spu_name = format!("{}-{}", self.key(), spu);

        let spu_private_ep = SpuEndpointTemplate::default_private();

        let spu_public_ep = SpuEndpointTemplate::default_public();
        let public_endpoint = if let Some(ingress) = services.get(&spu_name) {
            debug!(%ingress);
            ingress.clone()
        } else {
            IngressPort {
                port: spu_public_ep.port,
                encryption: spu_public_ep.encryption.clone(),
                ingress: vec![],
            }
        };

        let ns = self.ctx().item().namespace();
        let private_svc_fqdn = format!(
            "fluvio-spg-main-{spu}.fluvio-spg-{}.{ns}.svc.cluster.local",
            self.key()
        );
        let public_svc_fqdn = format!("fluvio-spu-{spu_name}.{ns}.svc.cluster.local");

        let spu_spec = SpuSpec {
            id: spu_id,
            spu_type: SpuType::Managed,
            public_endpoint,
            private_endpoint: Endpoint {
                host: private_svc_fqdn,
                port: spu_private_ep.port,
                encryption: spu_private_ep.encryption,
            },
            rack: None,
            public_endpoint_local: Some(Endpoint {
                host: public_svc_fqdn,
                port: spu_public_ep.port,
                encryption: spu_public_ep.encryption,
            }),
        };

        /*
        // add spu as children of spg
        let mut ctx = spg_obj.ctx().create_child().set_labels(vec![(
            "fluvio.io/spu-group".to_string(),
            spg_obj.key().to_string(),
        )]);
        */

        (
            spu_name.clone(),
            MetadataStoreObject::with_spec(spu_name, spu_spec)
                .with_context(self.ctx().create_child()),
        )
    }

}

/// compute spu id with min_id as base
fn compute_spu_id(min_id: i32, replica_index: u16) -> i32 {
    replica_index as i32 + min_id
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use fluvio_sc_schema::{
        core::MetadataContext,
        spg::SpuGroupSpec,
        store::{k8::K8MetaItem, MetadataStoreObject},
    };

    use crate::k8::objects::spg_group::SpuGroupObj;

    #[test]
    fn test_as_spu_id_private_endpoint_for_k8s() {
        let spu_cases = vec![0, 1, 2];

        for spu in spu_cases {
            let mut item = K8MetaItem::default();
            "default".clone_into(&mut item.namespace);
            let ctx = MetadataContext::new(item);

            let inner: MetadataStoreObject<SpuGroupSpec, K8MetaItem> =
                MetadataStoreObject::new_with_context(
                    spu.to_string(),
                    SpuGroupSpec::default(),
                    ctx,
                );

            let spu_group = SpuGroupObj::new(inner);
            let services = HashMap::new();
            let as_spu = spu_group.as_spu(spu, &services);
            let private_endpoint = as_spu.1.spec.private_endpoint;

            assert_eq!(
                private_endpoint.host,
                format!("fluvio-spg-main-{spu}.fluvio-spg-{spu}.default.svc.cluster.local")
            );
            assert_eq!(private_endpoint.port, 9006);
        }
    }
}
