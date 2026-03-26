use std::collections::BTreeMap;
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

use k8s_openapi::api::core::v1::{ConfigMap, Service, ServiceSpec, ServicePort};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use fluvio_future::task::spawn;
use fluvio_types::defaults::SPU_PUBLIC_PORT;
use futures_util::StreamExt;

use super::super::objects::spu_k8_config::ScK8Config;

const SPG_GROUP: &str = "fluvio.infinyon.com";
const SPG_VERSION: &str = "v1";
const SPG_KIND: &str = "SpuGroup";

pub struct SpuServiceV2Context {
    pub client: Client,
    pub namespace: String,
}

pub fn start(client: Client, namespace: String) {
    let spg_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, SPG_KIND));
    let spugroups: Api<DynamicObject> = Api::namespaced_with(client.clone(), &namespace, &spg_ar);

    let ctx = Arc::new(SpuServiceV2Context {
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
                        error!(%err, "spu service v2 reconciliation error");
                    }
                }
            });

        info!("SpuServiceV2Controller started");
        controller.await;
        info!("SpuServiceV2Controller shut down");
    });
}

async fn reconcile(
    spg: Arc<DynamicObject>,
    ctx: Arc<SpuServiceV2Context>,
) -> Result<Action, kube::Error> {
    let spg_name = spg.name_any();
    let spg_ns = spg
        .namespace()
        .unwrap_or_else(|| ctx.namespace.clone());

    debug!(%spg_name, "reconciling SpuGroup services");

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

    // Load spu-k8 ConfigMap
    let config = match load_spu_k8_config(&ctx.client, &spg_ns).await {
        Ok(c) => c,
        Err(err) => {
            error!(%err, "failed to load spu-k8 ConfigMap");
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
    };

    let services_api: Api<Service> = Api::namespaced(ctx.client.clone(), &spg_ns);

    // Create/update a service for each replica
    for i in 0..replicas {
        let spu_name = format!("{spg_name}-{i}");
        let svc_name = format!("fluvio-spu-{spu_name}");

        let svc = build_spu_service(i, &spu_name, &spg, &config, &spg_ns);

        match services_api
            .patch(
                &svc_name,
                &PatchParams::apply("fluvio-sc").force(),
                &Patch::Apply(&svc),
            )
            .await
        {
            Ok(_) => {
                debug!(%svc_name, "service applied");
            }
            Err(kube::Error::Api(ref err)) if err.code == 422 => {
                // Immutable field (e.g. service type change) — delete and recreate
                warn!(%svc_name, "immutable field error, deleting and recreating");
                let _ = services_api
                    .delete(&svc_name, &DeleteParams::default())
                    .await;
                services_api
                    .patch(
                        &svc_name,
                        &PatchParams::apply("fluvio-sc").force(),
                        &Patch::Apply(&svc),
                    )
                    .await?;
            }
            Err(err) => return Err(err),
        }
    }

    // Clean up orphaned services from scale-down
    let label_selector = "fluvio.io/spu-name";
    let existing_svcs = services_api
        .list(&ListParams::default().labels(label_selector))
        .await?;

    for svc in &existing_svcs.items {
        let svc_name = svc.metadata.name.as_deref().unwrap_or_default();
        let prefix = format!("fluvio-spu-{spg_name}-");
        if let Some(idx_str) = svc_name.strip_prefix(&prefix) {
            if let Ok(idx) = idx_str.parse::<u16>() {
                if idx >= replicas {
                    info!(%svc_name, "deleting orphaned service from scale-down");
                    let _ = services_api
                        .delete(svc_name, &DeleteParams::default())
                        .await;
                }
            }
        }
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(
    _spg: Arc<DynamicObject>,
    err: &kube::Error,
    _ctx: Arc<SpuServiceV2Context>,
) -> Action {
    error!(%err, "spu service reconciliation error, retrying in 30s");
    Action::requeue(Duration::from_secs(30))
}

fn build_spu_service(
    replica: u16,
    spu_name: &str,
    spg: &DynamicObject,
    config: &ScK8Config,
    namespace: &str,
) -> Service {
    let pod_name = format!("fluvio-spg-{spu_name}");

    let mut labels = BTreeMap::new();
    labels.insert("fluvio.io/spu-name".to_string(), spu_name.to_string());

    // Template annotations
    let mut annotations = BTreeMap::new();
    for (key, value) in &config.lb_service_annotations {
        let mut v = value.clone();
        v = v.replace("{replica}", &replica.to_string());
        v = v.replace("{spu_name}", spu_name);
        annotations.insert(key.clone(), v);
    }

    let mut selector = BTreeMap::new();
    selector.insert(
        "statefulset.kubernetes.io/pod-name".to_string(),
        pod_name,
    );

    // Build service port from config
    let port = SPU_PUBLIC_PORT as i32;
    let mut service_type: Option<String> = None;
    let mut node_port: Option<i32> = None;
    let mut external_traffic_policy: Option<String> = None;

    if let Some(service_template) = &config.service {
        if let Some(ty) = &service_template.r#type {
            use fluvio_stream_model::k8_types::core::service::LoadBalancerType;
            let type_str = match ty {
                LoadBalancerType::ClusterIP => "ClusterIP",
                LoadBalancerType::NodePort => "NodePort",
                LoadBalancerType::LoadBalancer => "LoadBalancer",
                LoadBalancerType::ExternalName => "ExternalName",
            };
            service_type = Some(type_str.to_string());
            if matches!(ty, LoadBalancerType::NodePort) {
                if let Some(base) = config.spu_pod_config.base_node_port {
                    node_port = Some((base + replica) as i32);
                }
            }
        }
        if let Some(etp) = &service_template.external_traffic_policy {
            external_traffic_policy = Some(format!("{etp:?}"));
        }
    }

    let svc_port = ServicePort {
        port,
        target_port: Some(IntOrString::Int(port)),
        node_port,
        ..Default::default()
    };

    // Build owner reference to SpuGroup
    let owner_ref = OwnerReference {
        api_version: format!("{SPG_GROUP}/{SPG_VERSION}"),
        kind: SPG_KIND.to_string(),
        name: spg.name_any(),
        uid: spg.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
        ..Default::default()
    };

    Service {
        metadata: ObjectMeta {
            name: Some(format!("fluvio-spu-{spu_name}")),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            annotations: if annotations.is_empty() {
                None
            } else {
                Some(annotations)
            },
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(selector),
            type_: service_type,
            ports: Some(vec![svc_port]),
            external_traffic_policy,
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub async fn load_spu_k8_config(client: &Client, namespace: &str) -> Result<ScK8Config> {
    let configmaps: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    match configmaps.get("spu-k8").await {
        Ok(cm) => {
            let data = cm.data.unwrap_or_default();
            let btree: BTreeMap<String, String> = data.into_iter().collect();
            ScK8Config::from_data(btree)
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            warn!("spu-k8 ConfigMap not found, using defaults");
            Ok(ScK8Config::default())
        }
        Err(err) => Err(err.into()),
    }
}
