//! K8s integration tests for kube-rs controllers.
//!
//! These tests require a running k3d cluster with Fluvio CRDs installed.
//! Run with: `cargo test -p fluvio-sc --test k8_integration -- --ignored --test-threads=1`
//! Or via justfile: `just test-k8-integration`
//!
//! Setup: `just create-cluster`

use std::time::Duration;

use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::api::core::v1::Service;
use kube::api::{
    Api, ApiResource, DynamicObject, GroupVersionKind, ListParams, Patch, PatchParams,
    DeleteParams,
};
use kube::{Client, Config, ResourceExt};

const NAMESPACE: &str = "fluvio-system";
const SPG_GROUP: &str = "fluvio.infinyon.com";
const SPG_VERSION: &str = "v1";
const KUBE_CONTEXT: &str = "k3d-koshee-fluvio";

async fn make_client() -> Client {
    let config = Config::from_kubeconfig(&kube::config::KubeConfigOptions {
        context: Some(KUBE_CONTEXT.to_string()),
        ..Default::default()
    })
    .await
    .expect("failed to load kubeconfig for k3d-koshee-fluvio context");
    Client::try_from(config).expect("failed to create kube client")
}

fn spg_api(client: &Client) -> Api<DynamicObject> {
    let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, "SpuGroup"));
    Api::namespaced_with(client.clone(), NAMESPACE, &ar)
}

fn spu_api(client: &Client) -> Api<DynamicObject> {
    let ar = ApiResource::from_gvk(&GroupVersionKind::gvk(SPG_GROUP, SPG_VERSION, "Spu"));
    Api::namespaced_with(client.clone(), NAMESPACE, &ar)
}

async fn create_spugroup(client: &Client, name: &str, replicas: i32, min_id: i32) {
    let spg = serde_json::json!({
        "apiVersion": format!("{SPG_GROUP}/{SPG_VERSION}"),
        "kind": "SpuGroup",
        "metadata": { "name": name, "namespace": NAMESPACE },
        "spec": { "replicas": replicas, "minId": min_id }
    });
    spg_api(client)
        .patch(name, &PatchParams::apply("test").force(), &Patch::Apply(spg))
        .await
        .expect("failed to create SpuGroup");
}

async fn delete_spugroup(client: &Client, name: &str) {
    let _ = spg_api(client).delete(name, &DeleteParams::default()).await;
}

async fn wait_for_services(client: &Client, spg_name: &str, count: usize, timeout: Duration) -> Vec<Service> {
    let services: Api<Service> = Api::namespaced(client.clone(), NAMESPACE);
    let prefix = format!("fluvio-spu-{spg_name}-");
    let start = std::time::Instant::now();
    loop {
        let list = services
            .list(&ListParams::default().labels("fluvio.io/spu-name"))
            .await
            .unwrap();
        let matching: Vec<Service> = list.items.into_iter()
            .filter(|s| s.metadata.name.as_deref().unwrap_or("").starts_with(&prefix))
            .collect();
        if matching.len() >= count {
            return matching;
        }
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for {} services for {spg_name}, got {}",
                count,
                matching.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_spus(client: &Client, spg_name: &str, count: usize, timeout: Duration) -> Vec<DynamicObject> {
    let api = spu_api(client);
    let prefix = format!("{spg_name}-");
    let start = std::time::Instant::now();
    loop {
        let list = api.list(&ListParams::default()).await.unwrap();
        let matching: Vec<DynamicObject> = list.items.into_iter()
            .filter(|s| s.name_any().starts_with(&prefix))
            .collect();
        if matching.len() >= count {
            return matching;
        }
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for {} SPUs for {spg_name}, got {}",
                count,
                matching.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_statefulset(client: &Client, name: &str, timeout: Duration) -> StatefulSet {
    let sts_api: Api<StatefulSet> = Api::namespaced(client.clone(), NAMESPACE);
    let start = std::time::Instant::now();
    loop {
        match sts_api.get(name).await {
            Ok(s) => return s,
            Err(_) => {
                if start.elapsed() > timeout {
                    panic!("timed out waiting for statefulset {name}");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Helper: start SC in background, return child process handle
fn start_sc() -> std::process::Child {
    // Find the workspace root — the binary is at <workspace>/target/debug/fluvio-run
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = std::path::Path::new(manifest_dir)
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap();
    let binary = workspace_root.join("target/debug/fluvio-run");
    assert!(binary.exists(), "fluvio-run binary not found at {binary:?} — run `just build-cluster` first");

    // Kill any lingering SC processes and wait for port release
    // Use fuser to kill processes on the SC ports instead of pkill
    // (pkill -f can match the test binary itself)
    let _ = std::process::Command::new("fuser")
        .args(["-k", "9003/tcp", "9004/tcp"])
        .output();

    // Wait for ports 9003/9004 to be free
    for _ in 0..20 {
        let output = std::process::Command::new("lsof")
            .args(["-i", ":9003", "-i", ":9004"])
            .output()
            .ok();
        if let Some(out) = output {
            if out.stdout.is_empty() || String::from_utf8_lossy(&out.stdout).lines().count() <= 1 {
                break;
            }
        } else {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // Set kubectl context before starting SC
    std::process::Command::new("kubectl")
        .args(["config", "use-context", KUBE_CONTEXT])
        .output()
        .expect("failed to switch kubectl context");

    std::process::Command::new(binary)
        .args(["sc", "--namespace", NAMESPACE, "--k8"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to start SC")
}

// ============================================================
// Tests
// ============================================================

#[tokio::test]
#[ignore]
async fn test_sc_starts_and_dispatchers_sync() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    // SC should be running (not exited)
    assert!(sc.try_wait().unwrap().is_none(), "SC exited unexpectedly");

    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spugroup_creates_services() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Create SpuGroup with 2 replicas
    create_spugroup(&client, "integ-svc", 2, 100).await;

    // Wait for 2 services
    let services = wait_for_services(&client, "integ-svc", 2, Duration::from_secs(15)).await;

    let svc_names: Vec<String> = services.iter().map(|s| s.name_any()).collect();
    assert!(svc_names.contains(&"fluvio-spu-integ-svc-0".to_string()));
    assert!(svc_names.contains(&"fluvio-spu-integ-svc-1".to_string()));

    // Verify service ports
    for svc in &services {
        let ports = svc.spec.as_ref().unwrap().ports.as_ref().unwrap();
        assert_eq!(ports[0].port, 9005);
    }

    // Cleanup
    delete_spugroup(&client, "integ-svc").await;
    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spugroup_creates_statefulset() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    create_spugroup(&client, "integ-sts", 2, 200).await;

    let sts = wait_for_statefulset(&client, "fluvio-spg-integ-sts", Duration::from_secs(15)).await;

    assert_eq!(sts.name_any(), "fluvio-spg-integ-sts");
    assert_eq!(sts.spec.as_ref().unwrap().replicas, Some(2));

    // Cleanup
    delete_spugroup(&client, "integ-sts").await;
    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spugroup_creates_headless_service() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    create_spugroup(&client, "integ-hl", 1, 300).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let services: Api<Service> = Api::namespaced(client.clone(), NAMESPACE);
    let headless = services.get("fluvio-spg-integ-hl").await.unwrap();
    assert_eq!(
        headless.spec.as_ref().unwrap().cluster_ip.as_deref(),
        Some("None")
    );

    // Cleanup
    delete_spugroup(&client, "integ-hl").await;
    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spugroup_creates_spu_crds() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    create_spugroup(&client, "integ-spu", 2, 400).await;

    let spus = wait_for_spus(&client, "integ-spu", 2, Duration::from_secs(15)).await;
    let spu_names: Vec<String> = spus.iter().map(|s| s.name_any()).collect();
    assert!(spu_names.contains(&"integ-spu-0".to_string()));
    assert!(spu_names.contains(&"integ-spu-1".to_string()));

    // Verify SPU IDs
    for spu in spus.iter() {
        let spu_id = spu.data.get("spec")
            .and_then(|s| s.get("spuId"))
            .and_then(|v| v.as_i64())
            .unwrap();
        assert!((400..402).contains(&spu_id), "unexpected SPU ID: {spu_id}");
    }

    // Cleanup
    delete_spugroup(&client, "integ-spu").await;
    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spu_has_correct_private_endpoint() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    create_spugroup(&client, "integ-ep", 1, 500).await;

    let spus = wait_for_spus(&client, "integ-ep", 1, Duration::from_secs(15)).await;
    let spu = &spus[0];

    let private_host = spu.data.get("spec")
        .and_then(|s| s.get("privateEndpoint"))
        .and_then(|e| e.get("host"))
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(
        private_host.contains("fluvio-spg-main-0.fluvio-spg-integ-ep"),
        "unexpected private host: {private_host}"
    );

    let local_host = spu.data.get("spec")
        .and_then(|s| s.get("publicEndpointLocal"))
        .and_then(|e| e.get("host"))
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(
        local_host.contains("fluvio-spu-integ-ep-0"),
        "unexpected local host: {local_host}"
    );

    // Cleanup
    delete_spugroup(&client, "integ-ep").await;
    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spu_clusterip_fqdn_in_ingress() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    create_spugroup(&client, "integ-fqdn", 1, 600).await;

    let spus = wait_for_spus(&client, "integ-fqdn", 1, Duration::from_secs(15)).await;
    let spu = &spus[0];

    // The publicEndpoint should have the ClusterIP FQDN fallback
    let ingress = spu.data.get("spec")
        .and_then(|s| s.get("publicEndpoint"))
        .and_then(|e| e.get("ingress"))
        .and_then(|v| v.as_array())
        .unwrap();

    assert!(!ingress.is_empty(), "publicEndpoint ingress should not be empty");
    let first = &ingress[0];
    let hostname = first.get("hostname").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        hostname.contains("fluvio-spu-integ-fqdn-0") && hostname.contains("svc.cluster.local"),
        "expected ClusterIP FQDN, got: {hostname}"
    );

    // Cleanup
    delete_spugroup(&client, "integ-fqdn").await;
    sc.kill().ok();
    sc.wait().ok();
}

#[tokio::test]
#[ignore]
async fn test_spugroup_delete_cascades() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let client = make_client().await;
    let mut sc = start_sc();
    tokio::time::sleep(Duration::from_secs(5)).await;

    create_spugroup(&client, "integ-gc", 1, 700).await;
    wait_for_services(&client, "integ-gc", 1, Duration::from_secs(15)).await;
    wait_for_spus(&client, "integ-gc", 1, Duration::from_secs(15)).await;

    // Delete SpuGroup
    delete_spugroup(&client, "integ-gc").await;
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Verify cascade: services and SPUs should be gone (via owner references)
    let services: Api<Service> = Api::namespaced(client.clone(), NAMESPACE);
    let svc_list = services
        .list(&ListParams::default().labels("fluvio.io/spu-name"))
        .await
        .unwrap();
    let remaining: Vec<_> = svc_list.items.iter()
        .filter(|s| s.name_any().contains("integ-gc"))
        .collect();
    assert!(remaining.is_empty(), "services should be garbage collected");

    sc.kill().ok();
    sc.wait().ok();
}
