use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use kube::api::{
    Api, DynamicObject, ApiResource, GroupVersionKind, ListParams, DeleteParams,
    Patch, PatchParams,
};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::runtime::WatchStreamExt;
use kube::Client;
use kube::ResourceExt;

use k8s_openapi::api::core::v1::Service;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;

use fluvio_future::task::spawn;
use fluvio_types::defaults::{SPU_PUBLIC_PORT, SPU_PRIVATE_PORT};
use futures_util::StreamExt;

use super::ingress::build_services_map;

const SPG_GROUP: &str = "fluvio.infinyon.com";
const SPG_VERSION: &str = "v1";
const SPG_KIND: &str = "SpuGroup";
const SPU_KIND: &str = "Spu";

pub struct SpuControllerV2Context {
    pub client: Client,
    pub namespace: String,
}

pub fn start(client: Client, namespace: String) {
    let spg_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, SPG_KIND));
    let spugroups: Api<DynamicObject> = Api::namespaced_with(client.clone(), &namespace, &spg_ar);

    let ctx = Arc::new(SpuControllerV2Context {
        client: client.clone(),
        namespace: namespace.clone(),
    });

    let services: Api<Service> = Api::namespaced(client.clone(), &namespace);

    spawn(async move {
        let controller = Controller::new_with(spugroups, watcher::Config::default(), spg_ar)
            .owns(services, watcher::Config::default())
            .shutdown_on_signal()
            .run(reconcile, error_policy, ctx)
            .default_backoff()
            .for_each(|result| async move {
                match result {
                    Ok((_obj, _action)) => {}
                    Err(err) => {
                        error!(%err, "spu controller v2 reconciliation error");
                    }
                }
            });

        info!("SpuControllerV2 started");
        controller.await;
        info!("SpuControllerV2 shut down");
    });
}

async fn reconcile(
    spg: Arc<DynamicObject>,
    ctx: Arc<SpuControllerV2Context>,
) -> Result<Action, kube::Error> {
    let spg_name = spg.name_any();
    let spg_ns = spg.namespace().unwrap_or_else(|| ctx.namespace.clone());

    debug!(%spg_name, "reconciling SpuGroup → SPU CRDs");

    let spec = match spg.data.get("spec") {
        Some(s) => s,
        None => {
            warn!(%spg_name, "SpuGroup has no spec, skipping");
            return Ok(Action::requeue(Duration::from_secs(60)));
        }
    };

    let replicas = spec
        .get("replicas")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u16;
    let min_id = spec
        .get("minId")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    // Get services to extract ingress
    let services_api: Api<Service> = Api::namespaced(ctx.client.clone(), &spg_ns);
    let svc_list = services_api
        .list(&ListParams::default().labels("fluvio.io/spu-name"))
        .await?;
    let services = build_services_map(&svc_list.items);

    // SPU CRD API
    let spu_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, SPU_KIND));
    let spu_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &spg_ns, &spu_ar);

    // Owner reference from SpuGroup
    let owner_ref = OwnerReference {
        api_version: format!("{SPG_GROUP}/{SPG_VERSION}"),
        kind: SPG_KIND.to_string(),
        name: spg.name_any(),
        uid: spg.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
        ..Default::default()
    };

    for i in 0..replicas {
        let spu_id = min_id + i as i32;
        let spu_name = format!("{spg_name}-{i}");
        let public_endpoint = services.get(&spu_name).cloned().unwrap_or_default();

        let spu_obj = build_spu_object(
            &spu_name, &spg_name, i, spu_id, &public_endpoint, &spg_ns, &owner_ref,
        );

        debug!(%spu_name, spu_id, "applying SPU CRD");
        spu_api
            .patch(
                &spu_name,
                &PatchParams::apply("fluvio-sc").force(),
                &Patch::Apply(spu_obj),
            )
            .await?;
    }

    // Clean up orphaned SPU CRDs from scale-down
    let existing_spus = spu_api
        .list(&ListParams::default())
        .await?;

    for spu in &existing_spus.items {
        let name = spu.name_any();
        let prefix = format!("{spg_name}-");
        if let Some(idx_str) = name.strip_prefix(&prefix) {
            if let Ok(idx) = idx_str.parse::<u16>() {
                if idx >= replicas {
                    // Check this SPU is actually owned by this SpuGroup
                    let is_owned = spu
                        .metadata
                        .owner_references
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|or| or.name == spg_name && or.kind == SPG_KIND);

                    if is_owned {
                        info!(%name, "deleting orphaned SPU CRD from scale-down");
                        let _ = spu_api.delete(&name, &DeleteParams::default()).await;
                    }
                }
            }
        }
    }

    Ok(Action::requeue(Duration::from_secs(60)))
}

fn error_policy(
    _spg: Arc<DynamicObject>,
    err: &kube::Error,
    _ctx: Arc<SpuControllerV2Context>,
) -> Action {
    error!(%err, "spu controller reconciliation error, retrying in 30s");
    Action::requeue(Duration::from_secs(30))
}

fn build_spu_object(
    spu_name: &str,
    spg_name: &str,
    replica_index: u16,
    spu_id: i32,
    public_endpoint: &fluvio_controlplane_metadata::spu::IngressPort,
    namespace: &str,
    owner_ref: &k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> serde_json::Value {
    let public_svc_fqdn = format!("fluvio-spu-{spu_name}.{namespace}.svc.cluster.local");
    let private_svc_fqdn = format!(
        "fluvio-spg-main-{replica_index}.fluvio-spg-{spg_name}.{namespace}.svc.cluster.local"
    );

    let mut ingress_entries: Vec<serde_json::Value> = public_endpoint
        .ingress
        .iter()
        .map(|addr| {
            let mut entry = serde_json::Map::new();
            if let Some(ref hostname) = addr.hostname {
                entry.insert("hostname".into(), serde_json::json!(hostname));
            }
            if let Some(ref ip) = addr.ip {
                entry.insert("ip".into(), serde_json::json!(ip));
            }
            serde_json::Value::Object(entry)
        })
        .collect();

    if ingress_entries.is_empty() {
        ingress_entries.push(serde_json::json!({
            "hostname": public_svc_fqdn,
        }));
    }

    let public_port = if public_endpoint.port == 0 {
        SPU_PUBLIC_PORT
    } else {
        public_endpoint.port
    };

    serde_json::json!({
        "apiVersion": format!("{SPG_GROUP}/{SPG_VERSION}"),
        "kind": SPU_KIND,
        "metadata": {
            "name": spu_name,
            "namespace": namespace,
            "ownerReferences": [owner_ref],
        },
        "spec": {
            "spuId": spu_id,
            "spuType": "Managed",
            "publicEndpoint": {
                "port": public_port,
                "ingress": ingress_entries,
                "encryption": "PLAINTEXT",
            },
            "privateEndpoint": {
                "host": private_svc_fqdn,
                "port": SPU_PRIVATE_PORT,
                "encryption": "PLAINTEXT",
            },
            "publicEndpointLocal": {
                "host": public_svc_fqdn,
                "port": SPU_PUBLIC_PORT,
                "encryption": "PLAINTEXT",
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluvio_controlplane_metadata::spu::IngressPort;
    use crate::stores::spu::IngressAddr;

    fn test_owner_ref() -> k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
        k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
            api_version: "fluvio.infinyon.com/v1".to_string(),
            kind: "SpuGroup".to_string(),
            name: "main".to_string(),
            uid: "uid-123".to_string(),
            controller: Some(true),
            block_owner_deletion: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn test_spu_object_has_correct_spec_fields() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-0", "main", 0, 5, &endpoint, "ns", &test_owner_ref());
        assert_eq!(obj["spec"]["spuId"], 5);
        assert_eq!(obj["spec"]["spuType"], "Managed");
        assert!(obj["spec"]["publicEndpoint"].is_object());
        assert!(obj["spec"]["privateEndpoint"].is_object());
        assert!(obj["spec"]["publicEndpointLocal"].is_object());
    }

    #[test]
    fn test_spu_private_endpoint_fqdn() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-1", "main", 1, 1, &endpoint, "fluvio-system", &test_owner_ref());
        assert_eq!(
            obj["spec"]["privateEndpoint"]["host"],
            "fluvio-spg-main-1.fluvio-spg-main.fluvio-system.svc.cluster.local"
        );
        assert_eq!(obj["spec"]["privateEndpoint"]["port"], SPU_PRIVATE_PORT);
    }

    #[test]
    fn test_spu_public_endpoint_local_fqdn() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-0", "main", 0, 0, &endpoint, "fluvio-system", &test_owner_ref());
        assert_eq!(
            obj["spec"]["publicEndpointLocal"]["host"],
            "fluvio-spu-main-0.fluvio-system.svc.cluster.local"
        );
        assert_eq!(obj["spec"]["publicEndpointLocal"]["port"], SPU_PUBLIC_PORT);
    }

    #[test]
    fn test_spu_public_endpoint_from_service_ingress() {
        let endpoint = IngressPort {
            port: 9005,
            ingress: vec![IngressAddr {
                hostname: Some("lb.example.com".to_string()),
                ip: None,
            }],
            ..Default::default()
        };
        let obj = build_spu_object("main-0", "main", 0, 0, &endpoint, "ns", &test_owner_ref());
        assert_eq!(obj["spec"]["publicEndpoint"]["port"], 9005);
        let ingress = obj["spec"]["publicEndpoint"]["ingress"].as_array().unwrap();
        assert_eq!(ingress[0]["hostname"], "lb.example.com");
    }

    #[test]
    fn test_spu_public_endpoint_empty_when_no_service() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-0", "main", 0, 0, &endpoint, "ns", &test_owner_ref());
        let ingress = obj["spec"]["publicEndpoint"]["ingress"].as_array().unwrap();
        assert_eq!(obj["spec"]["publicEndpoint"]["port"], SPU_PUBLIC_PORT);
        assert_eq!(ingress[0]["hostname"], "fluvio-spu-main-0.ns.svc.cluster.local");
    }

    #[test]
    fn test_spu_object_sets_plaintext_encryption_fields() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-0", "main", 0, 0, &endpoint, "ns", &test_owner_ref());
        assert_eq!(obj["spec"]["publicEndpoint"]["encryption"], "PLAINTEXT");
        assert_eq!(obj["spec"]["privateEndpoint"]["encryption"], "PLAINTEXT");
        assert_eq!(obj["spec"]["publicEndpointLocal"]["encryption"], "PLAINTEXT");
    }

    #[test]
    fn test_spu_owner_reference() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-0", "main", 0, 0, &endpoint, "ns", &test_owner_ref());
        let refs = obj["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(refs[0]["name"], "main");
        assert_eq!(refs[0]["uid"], "uid-123");
        assert_eq!(refs[0]["kind"], "SpuGroup");
    }

    #[test]
    fn test_spu_metadata() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("main-0", "main", 0, 0, &endpoint, "fluvio-system", &test_owner_ref());
        assert_eq!(obj["metadata"]["name"], "main-0");
        assert_eq!(obj["metadata"]["namespace"], "fluvio-system");
        assert_eq!(obj["apiVersion"], "fluvio.infinyon.com/v1");
        assert_eq!(obj["kind"], "Spu");
    }

    #[test]
    fn test_spu_id_calculation() {
        let endpoint = IngressPort::default();
        let obj = build_spu_object("test-2", "test", 2, 102, &endpoint, "ns", &test_owner_ref());
        assert_eq!(obj["spec"]["spuId"], 102);
        assert_eq!(obj["metadata"]["name"], "test-2");
    }
}
