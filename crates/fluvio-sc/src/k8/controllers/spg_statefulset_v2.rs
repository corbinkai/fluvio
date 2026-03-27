use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use kube::api::{
    Api, DynamicObject, ApiResource, GroupVersionKind,
    Patch, PatchParams,
};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::runtime::WatchStreamExt;
use kube::Client;
use kube::ResourceExt;

use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Service;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;

use fluvio_future::task::spawn;
use fluvio_types::defaults::{
    SPU_DEFAULT_NAME, SPU_PUBLIC_PORT, SPU_PRIVATE_PORT, SC_PRIVATE_PORT, PRODUCT_NAME,
    TLS_SERVER_SECRET_NAME,
};
use futures_util::StreamExt;

use crate::cli::TlsConfig;
use super::spu_service_v2::load_spu_k8_config;
use super::super::objects::spu_k8_config::ScK8Config;

const SPG_GROUP: &str = "fluvio.infinyon.com";
const SPG_VERSION: &str = "v1";
const SPG_KIND: &str = "SpuGroup";

pub struct SpgStatefulSetV2Context {
    pub client: Client,
    pub namespace: String,
    pub tls: Option<TlsConfig>,
}

pub fn start(client: Client, namespace: String, tls: Option<TlsConfig>) {
    let spg_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, SPG_KIND));
    let spugroups: Api<DynamicObject> = Api::namespaced_with(client.clone(), &namespace, &spg_ar);

    let ctx = Arc::new(SpgStatefulSetV2Context {
        client: client.clone(),
        namespace: namespace.clone(),
        tls,
    });

    let statefulsets: Api<StatefulSet> = Api::namespaced(client.clone(), &namespace);
    let services: Api<Service> = Api::namespaced(client.clone(), &namespace);

    spawn(async move {
        let controller = Controller::new_with(spugroups, watcher::Config::default(), spg_ar)
            .owns(statefulsets, watcher::Config::default())
            .owns(services, watcher::Config::default())
            .shutdown_on_signal()
            .run(reconcile, error_policy, ctx)
            .default_backoff()
            .for_each(|result| async move {
                match result {
                    Ok((_obj, _action)) => {}
                    Err(err) => {
                        error!(%err, "spg statefulset v2 reconciliation error");
                    }
                }
            });

        info!("SpgStatefulSetV2Controller started");
        controller.await;
        info!("SpgStatefulSetV2Controller shut down");
    });
}

async fn reconcile(
    spg: Arc<DynamicObject>,
    ctx: Arc<SpgStatefulSetV2Context>,
) -> Result<Action, kube::Error> {
    let spg_name = spg.name_any();
    let spg_ns = spg.namespace().unwrap_or_else(|| ctx.namespace.clone());

    debug!(%spg_name, "reconciling SpuGroup → StatefulSet + headless Service");

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
        .unwrap_or(1) as i32;
    let min_id = spec
        .get("minId")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    // Check SPU ID conflicts
    let spg_uid = spg.uid().unwrap_or_default();
    let spu_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, "Spu"));
    let spu_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &spg_ns, &spu_ar);
    let existing_spus = spu_api.list(&kube::api::ListParams::default()).await?;

    let end_id_exclusive = min_id + replicas;
    for spu_obj in &existing_spus.items {
        // Skip SPUs owned by this SpuGroup
        let is_owned = spu_obj
            .metadata
            .owner_references
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|or| or.uid == spg_uid);
        if is_owned {
            continue;
        }
        if let Some(spu_id) = spu_obj.data.get("spec").and_then(|s| s.get("spuId")).and_then(|v| v.as_i64()) {
            let spu_id = spu_id as i32;
            if spu_id >= min_id && spu_id < end_id_exclusive {
                warn!(%spg_name, conflict_id = spu_id, "SPU ID conflict with existing SPU");
                // Update SpuGroup status to invalid
                let status_patch = serde_json::json!({
                    "apiVersion": format!("{SPG_GROUP}/{SPG_VERSION}"),
                    "kind": SPG_KIND,
                    "status": {
                        "resolution": "Invalid",
                        "reason": format!("SPU ID conflict with existing id: {spu_id}"),
                    }
                });
                let spg_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, SPG_KIND));
                let spg_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &spg_ns, &spg_ar);
                let _ = spg_api.patch_status(
                    &spg_name,
                    &PatchParams::apply("fluvio-sc").force(),
                    &Patch::Apply(status_patch),
                ).await;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }
    }

    // Update SpuGroup status to Reserved if not already
    let current_resolution = spg
        .data
        .get("status")
        .and_then(|s| s.get("resolution"))
        .and_then(|v| v.as_str())
        .unwrap_or("Init");

    if current_resolution != "Reserved" {
        debug!(%spg_name, "setting SpuGroup status to Reserved");
        let status_patch = serde_json::json!({
            "apiVersion": format!("{SPG_GROUP}/{SPG_VERSION}"),
            "kind": SPG_KIND,
            "status": {
                "resolution": "Reserved",
            }
        });
        let spg_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, SPG_KIND));
        let spg_api: Api<DynamicObject> = Api::namespaced_with(ctx.client.clone(), &spg_ns, &spg_ar);
        let _ = spg_api.patch_status(
            &spg_name,
            &PatchParams::apply("fluvio-sc").force(),
            &Patch::Apply(status_patch),
        ).await;
    }

    // Load spu-k8 ConfigMap
    let config = match load_spu_k8_config(&ctx.client, &spg_ns).await {
        Ok(c) => c,
        Err(err) => {
            error!(%err, "failed to load spu-k8 ConfigMap");
            return Ok(Action::requeue(Duration::from_secs(30)));
        }
    };

    let owner_ref = OwnerReference {
        api_version: format!("{SPG_GROUP}/{SPG_VERSION}"),
        kind: SPG_KIND.to_string(),
        name: spg_name.clone(),
        uid: spg.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
        ..Default::default()
    };

    // 1. Create/update headless service
    let services_api: Api<Service> = Api::namespaced(ctx.client.clone(), &spg_ns);
    let headless_svc = build_headless_service(&spg_name, &spg_ns, &owner_ref);
    let svc_name = format!("fluvio-spg-{spg_name}");
    services_api
        .patch(
            &svc_name,
            &PatchParams::apply("fluvio-sc").force(),
            &Patch::Apply(&headless_svc),
        )
        .await?;
    debug!(%svc_name, "headless service applied");

    // 2. Create/update StatefulSet
    let statefulsets_api: Api<StatefulSet> = Api::namespaced(ctx.client.clone(), &spg_ns);
    let sts = build_statefulset(
        &spg_name, &spg_ns, replicas, min_id, &config,
        ctx.tls.as_ref(), &owner_ref,
    );
    let sts_name = format!("fluvio-spg-{spg_name}");
    statefulsets_api
        .patch(
            &sts_name,
            &PatchParams::apply("fluvio-sc").force(),
            &Patch::Apply(&sts),
        )
        .await?;
    debug!(%sts_name, "statefulset applied");

    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(
    _spg: Arc<DynamicObject>,
    err: &kube::Error,
    _ctx: Arc<SpgStatefulSetV2Context>,
) -> Action {
    error!(%err, "spg statefulset reconciliation error, retrying in 30s");
    Action::requeue(Duration::from_secs(30))
}

fn build_headless_service(
    group_name: &str,
    namespace: &str,
    owner_ref: &OwnerReference,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": format!("fluvio-spg-{group_name}"),
            "namespace": namespace,
            "ownerReferences": [owner_ref],
        },
        "spec": {
            "clusterIP": "None",
            "selector": {
                "app": SPU_DEFAULT_NAME,
                "group": group_name,
            },
            "ports": [
                {
                    "name": "public",
                    "port": SPU_PUBLIC_PORT,
                },
                {
                    "name": "private",
                    "port": SPU_PRIVATE_PORT,
                }
            ]
        }
    })
}

fn build_statefulset(
    group_name: &str,
    namespace: &str,
    replicas: i32,
    min_id: i32,
    config: &ScK8Config,
    tls_config: Option<&TlsConfig>,
    owner_ref: &OwnerReference,
) -> serde_json::Value {
    let spu_pod_config = &config.spu_pod_config;
    let svc_name = format!("fluvio-spg-{group_name}");
    let sts_name = format!("fluvio-spg-{group_name}");

    // Storage config defaults
    let storage_size = "10Gi";

    // Build env vars
    let mut env = vec![
        serde_json::json!({
            "name": "SPU_INDEX",
            "valueFrom": {
                "fieldRef": {
                    "fieldPath": "metadata.name"
                }
            }
        }),
        serde_json::json!({
            "name": "SPU_MIN",
            "value": format!("{min_id}")
        }),
        serde_json::json!({
            "name": "FLV_SHORT_RECONCILLATION",
            "value": "1"
        }),
    ];

    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        env.push(serde_json::json!({
            "name": "RUST_LOG",
            "value": rust_log
        }));
    }

    // Build args
    // ENTRYPOINT in the Dockerfile is /fluvio-run, so args start with the subcommand
    let mut args = vec![
        "spu".to_string(),
        "--sc-addr".to_string(),
        format!("fluvio-sc-internal.{namespace}.svc.cluster.local:{SC_PRIVATE_PORT}"),
        "--log-base-dir".to_string(),
        format!("/var/lib/{PRODUCT_NAME}/data"),
        "--log-size".to_string(),
        storage_size.to_string(),
    ];

    let mut volume_mounts = vec![
        serde_json::json!({
            "name": "data",
            "mountPath": format!("/var/lib/{PRODUCT_NAME}/data")
        }),
    ];

    let mut volumes: Vec<serde_json::Value> = vec![];

    if let Some(tls) = tls_config {
        args.push("--tls".to_string());
        if tls.enable_client_cert {
            args.push("--enable-client-cert".to_string());
            args.push("--ca-cert".to_string());
            args.push(tls.ca_cert.clone().unwrap_or_default());
            volume_mounts.push(serde_json::json!({
                "name": "cacert",
                "mountPath": "/var/certs/ca",
                "readOnly": true
            }));
            volumes.push(serde_json::json!({
                "name": "cacert",
                "secret": { "secretName": "fluvio-ca" }
            }));
        }
        args.push("--server-cert".to_string());
        args.push(tls.server_cert.clone().unwrap_or_default());
        args.push("--server-key".to_string());
        args.push(tls.server_key.clone().unwrap_or_default());

        volume_mounts.push(serde_json::json!({
            "name": "tls",
            "mountPath": "/var/certs/tls",
            "readOnly": true
        }));
        volumes.push(serde_json::json!({
            "name": "tls",
            "secret": {
                "secretName": tls.secret_name.clone()
                    .unwrap_or_else(|| TLS_SERVER_SECRET_NAME.to_string())
            }
        }));

        args.push("--bind-non-tls-public".to_string());
        args.push("0.0.0.0:9007".to_string());
    }

    // Container
    let mut container = serde_json::json!({
        "name": SPU_DEFAULT_NAME,
        "image": config.image,
        "imagePullPolicy": "Always",
        "ports": [
            { "name": "public", "containerPort": SPU_PUBLIC_PORT },
            { "name": "private", "containerPort": SPU_PRIVATE_PORT },
            { "name": "health", "containerPort": 9008 },
        ],
        "volumeMounts": volume_mounts,
        "env": env,
        "args": args,
        "livenessProbe": {
            "tcpSocket": { "port": 9008 },
            "initialDelaySeconds": 5,
            "periodSeconds": 10,
        },
        "readinessProbe": {
            "httpGet": { "path": "/readyz", "port": 9008 },
            "initialDelaySeconds": 5,
            "periodSeconds": 5,
        }
    });

    // Add resources if configured
    if let Some(resources) = &spu_pod_config.resources {
        if let Ok(res_json) = serde_json::to_value(resources) {
            container.as_object_mut().unwrap().insert("resources".into(), res_json);
        }
    }

    let mut pod_spec = serde_json::json!({
        "terminationGracePeriodSeconds": 10,
        "containers": [container],
    });

    if !volumes.is_empty() {
        pod_spec.as_object_mut().unwrap().insert("volumes".into(), serde_json::json!(volumes));
    }

    if let Some(psc) = &config.pod_security_context {
        if let Ok(psc_json) = serde_json::to_value(psc) {
            pod_spec.as_object_mut().unwrap().insert("securityContext".into(), psc_json);
        }
    }

    if !spu_pod_config.node_selector.is_empty() {
        pod_spec.as_object_mut().unwrap().insert(
            "nodeSelector".into(),
            serde_json::to_value(&spu_pod_config.node_selector).unwrap(),
        );
    }

    if let Some(pcn) = &spu_pod_config.priority_class_name {
        pod_spec.as_object_mut().unwrap().insert(
            "priorityClassName".into(),
            serde_json::json!(pcn),
        );
    }

    // Build the StatefulSet
    serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": {
            "name": sts_name,
            "namespace": namespace,
            "ownerReferences": [owner_ref],
        },
        "spec": {
            "replicas": replicas,
            "serviceName": svc_name,
            "selector": {
                "matchLabels": {
                    "app": SPU_DEFAULT_NAME,
                    "group": group_name,
                }
            },
            "template": {
                "metadata": {
                    "labels": {
                        "app": SPU_DEFAULT_NAME,
                        "group": group_name,
                    }
                },
                "spec": pod_spec,
            },
            "volumeClaimTemplates": [
                {
                    "metadata": { "name": "data" },
                    "spec": {
                        "accessModes": ["ReadWriteOnce"],
                        "storageClassName": spu_pod_config.storage_class,
                        "resources": {
                            "requests": {
                                "storage": storage_size,
                            }
                        }
                    }
                }
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_owner_ref() -> OwnerReference {
        OwnerReference {
            api_version: "fluvio.infinyon.com/v1".to_string(),
            kind: "SpuGroup".to_string(),
            name: "main".to_string(),
            uid: "test-uid".to_string(),
            controller: Some(true),
            block_owner_deletion: Some(true),
            ..Default::default()
        }
    }

    fn default_config() -> ScK8Config {
        ScK8Config {
            image: "infinyon/fluvio:latest".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_build_headless_service() {
        let owner = test_owner_ref();
        let svc = build_headless_service("main", "fluvio-system", &owner);
        assert_eq!(svc["metadata"]["name"], "fluvio-spg-main");
        assert_eq!(svc["metadata"]["namespace"], "fluvio-system");
        assert_eq!(svc["spec"]["clusterIP"], "None");
        assert_eq!(svc["spec"]["selector"]["app"], SPU_DEFAULT_NAME);
        assert_eq!(svc["spec"]["selector"]["group"], "main");
        let ports = svc["spec"]["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0]["name"], "public");
        assert_eq!(ports[0]["port"], SPU_PUBLIC_PORT);
        assert_eq!(ports[1]["name"], "private");
        assert_eq!(ports[1]["port"], SPU_PRIVATE_PORT);
    }

    #[test]
    fn test_build_headless_service_owner_ref() {
        let owner = test_owner_ref();
        let svc = build_headless_service("main", "ns", &owner);
        let refs = svc["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["name"], "main");
        assert_eq!(refs[0]["uid"], "test-uid");
    }

    #[test]
    fn test_build_statefulset_basic() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "fluvio-system", 2, 0, &default_config(), None, &owner);
        assert_eq!(sts["metadata"]["name"], "fluvio-spg-main");
        assert_eq!(sts["spec"]["replicas"], 2);
        assert_eq!(sts["spec"]["serviceName"], "fluvio-spg-main");
        assert_eq!(sts["spec"]["selector"]["matchLabels"]["app"], SPU_DEFAULT_NAME);
        assert_eq!(sts["spec"]["selector"]["matchLabels"]["group"], "main");
    }

    #[test]
    fn test_build_statefulset_container_ports() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        let ports = sts["spec"]["template"]["spec"]["containers"][0]["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 3);
        let port_names: Vec<&str> = ports.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert!(port_names.contains(&"public"));
        assert!(port_names.contains(&"private"));
        assert!(port_names.contains(&"health"));
    }

    #[test]
    fn test_build_statefulset_env_vars() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 5, &default_config(), None, &owner);
        let env = sts["spec"]["template"]["spec"]["containers"][0]["env"].as_array().unwrap();
        let spu_index = env.iter().find(|e| e["name"] == "SPU_INDEX").unwrap();
        assert!(spu_index["valueFrom"]["fieldRef"]["fieldPath"] == "metadata.name");
        let spu_min = env.iter().find(|e| e["name"] == "SPU_MIN").unwrap();
        assert_eq!(spu_min["value"], "5");
    }

    #[test]
    fn test_build_statefulset_args() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "fluvio-system", 1, 0, &default_config(), None, &owner);
        let args = sts["spec"]["template"]["spec"]["containers"][0]["args"].as_array().unwrap();
        let args_str: Vec<&str> = args.iter().map(|a| a.as_str().unwrap()).collect();
        assert!(!args_str.contains(&"/fluvio-run"), "args should not contain binary path");
        assert!(args_str.contains(&"spu"));
        assert!(args_str.contains(&"--sc-addr"));
        assert!(args_str.iter().any(|a| a.contains("fluvio-sc-internal.fluvio-system.svc.cluster.local")));
    }

    #[test]
    fn test_build_statefulset_liveness_probe() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        let probe = &sts["spec"]["template"]["spec"]["containers"][0]["livenessProbe"];
        assert_eq!(probe["tcpSocket"]["port"], 9008);
        assert_eq!(probe["initialDelaySeconds"], 5);
        assert_eq!(probe["periodSeconds"], 10);
    }

    #[test]
    fn test_build_statefulset_readiness_probe() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        let probe = &sts["spec"]["template"]["spec"]["containers"][0]["readinessProbe"];
        assert_eq!(probe["httpGet"]["path"], "/readyz");
        assert_eq!(probe["httpGet"]["port"], 9008);
    }

    #[test]
    fn test_build_statefulset_pvc() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        let claims = sts["spec"]["volumeClaimTemplates"].as_array().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["metadata"]["name"], "data");
        let modes = claims[0]["spec"]["accessModes"].as_array().unwrap();
        assert_eq!(modes[0], "ReadWriteOnce");
    }

    #[test]
    fn test_build_statefulset_owner_ref() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        let refs = sts["metadata"]["ownerReferences"].as_array().unwrap();
        assert_eq!(refs[0]["name"], "main");
        assert_eq!(refs[0]["uid"], "test-uid");
    }

    // TLS test skipped — TlsConfig has private fields (tls, bind_non_tls_public)
    // and can't be constructed in tests. TLS path is covered by k3d integration tests.

    #[test]
    fn test_build_statefulset_termination_grace_period() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        assert_eq!(sts["spec"]["template"]["spec"]["terminationGracePeriodSeconds"], 10);
    }

    #[test]
    fn test_build_statefulset_image() {
        let owner = test_owner_ref();
        let config = ScK8Config {
            image: "myregistry/fluvio:v1.0".to_string(),
            ..Default::default()
        };
        let sts = build_statefulset("main", "ns", 1, 0, &config, None, &owner);
        assert_eq!(sts["spec"]["template"]["spec"]["containers"][0]["image"], "myregistry/fluvio:v1.0");
    }

    #[test]
    fn test_build_statefulset_with_node_selector() {
        let owner = test_owner_ref();
        let mut config = default_config();
        config.spu_pod_config.node_selector.insert("kubernetes.io/arch".to_string(), "arm64".to_string());
        let sts = build_statefulset("main", "ns", 1, 0, &config, None, &owner);
        let ns = &sts["spec"]["template"]["spec"]["nodeSelector"];
        assert_eq!(ns["kubernetes.io/arch"], "arm64");
    }

    #[test]
    fn test_build_statefulset_has_short_reconcillation_env() {
        let owner = test_owner_ref();
        let sts = build_statefulset("main", "ns", 1, 0, &default_config(), None, &owner);
        let env = sts["spec"]["template"]["spec"]["containers"][0]["env"].as_array().unwrap();
        let reconcillation = env.iter().find(|e| e["name"] == "FLV_SHORT_RECONCILLATION").unwrap();
        assert_eq!(reconcillation["value"], "1");
    }

    #[test]
    fn test_build_statefulset_with_priority_class() {
        let owner = test_owner_ref();
        let mut config = default_config();
        config.spu_pod_config.priority_class_name = Some("high-priority".to_string());
        let sts = build_statefulset("main", "ns", 1, 0, &config, None, &owner);
        assert_eq!(sts["spec"]["template"]["spec"]["priorityClassName"], "high-priority");
    }
}
