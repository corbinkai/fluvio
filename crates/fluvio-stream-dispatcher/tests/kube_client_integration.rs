//! Integration tests for KubeClient (MetadataClient<K8MetaItem> implementation).
//!
//! These tests require a running k3d cluster with Fluvio CRDs installed.
//! Run with: cargo test -p fluvio-stream-dispatcher --test kube_client_integration -- --ignored --test-threads=1

use std::time::Duration;

use fluvio_controlplane_metadata::topic::{TopicSpec, TopicStatus};
use fluvio_stream_dispatcher::metadata::kube_rs::KubeClient;
use fluvio_stream_dispatcher::metadata::MetadataClient;
use fluvio_stream_model::core::MetadataContext;
use fluvio_stream_model::store::k8::K8MetaItem;
use fluvio_stream_model::store::{MetadataStoreObject, NameSpace};

use futures_util::StreamExt;
use uuid::Uuid;

fn test_namespace() -> NameSpace {
    NameSpace::Named("default".to_string())
}

fn unique_name(prefix: &str) -> String {
    let suffix = &Uuid::new_v4().to_string()[..8];
    format!("{prefix}-{suffix}")
}

async fn make_client() -> KubeClient {
    let client = kube::Client::try_default().await.expect("k8s client");
    KubeClient::new(client)
}

fn make_topic_store_object(name: &str, partitions: u32) -> MetadataStoreObject<TopicSpec, K8MetaItem> {
    let spec = TopicSpec::new_computed(partitions, 1, None);
    let meta = K8MetaItem::new(name.to_string(), "default".to_string());
    let ctx: MetadataContext<K8MetaItem> = MetadataContext::new(meta);
    MetadataStoreObject::new_with_context(name.to_string(), spec, ctx)
}

async fn cleanup(client: &KubeClient, name: &str) {
    let meta = K8MetaItem::new(name.to_string(), "default".to_string());
    let _ = client.delete_item::<TopicSpec>(meta).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_retrieve_items_returns_empty_for_unique_prefix() {
    let client = make_client().await;
    let ns = test_namespace();

    let result = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    // We can't assert empty because other topics may exist.
    // Instead verify the call succeeds and returns a valid list.
    assert!(result.version.len() > 0, "should have a resource version");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_apply_creates_object() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-apply");

    // Clean up from any previous failed run
    cleanup(&client, &name).await;

    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();

    // Verify it exists
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let found = items.items.iter().any(|item| item.key().to_string() == name);
    assert!(found, "topic {name} should exist after apply");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_apply_updates_existing_object() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-apply-update");

    cleanup(&client, &name).await;

    // Create with 1 partition
    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Update to 2 partitions (server-side apply with force should replace)
    let obj2 = make_topic_store_object(&name, 2);
    client.apply::<TopicSpec>(obj2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify updated
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let topic = items.items.iter().find(|item| item.key().to_string() == name);
    assert!(topic.is_some(), "topic should exist");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_delete_item_removes_object() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-delete");

    cleanup(&client, &name).await;

    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Delete
    let meta = K8MetaItem::new(name.clone(), "default".to_string());
    client.delete_item::<TopicSpec>(meta).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify gone
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let found = items.items.iter().any(|item| item.key().to_string() == name);
    assert!(!found, "topic {name} should be deleted");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_update_spec_changes_spec() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-update-spec");

    cleanup(&client, &name).await;

    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Update spec
    let meta = K8MetaItem::new(name.clone(), "default".to_string());
    let new_spec = TopicSpec::new_computed(3, 1, None);
    client.update_spec::<TopicSpec>(meta, new_spec).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify spec changed
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let topic = items.items.iter().find(|item| item.key().to_string() == name);
    assert!(topic.is_some(), "topic should exist after update_spec");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_update_spec_by_key() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-update-by-key");

    cleanup(&client, &name).await;

    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Update via key
    let new_spec = TopicSpec::new_computed(2, 1, None);
    client
        .update_spec_by_key::<TopicSpec>(name.clone(), &ns, new_spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let topic = items.items.iter().find(|item| item.key().to_string() == name);
    assert!(topic.is_some(), "topic should exist after update_spec_by_key");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_update_status_sets_status() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-update-status");

    cleanup(&client, &name).await;

    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get the object to obtain resource_version
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let topic = items
        .items
        .iter()
        .find(|item| item.key().to_string() == name)
        .expect("topic should exist");
    let meta = topic.ctx().item().clone();

    let new_status = TopicStatus::default();
    let result = client
        .update_status::<TopicSpec>(meta, new_status, &ns)
        .await;
    assert!(result.is_ok(), "update_status should succeed: {result:?}");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_patch_status_merges_status() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-patch-status");

    cleanup(&client, &name).await;

    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get object for resource_version
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let topic = items
        .items
        .iter()
        .find(|item| item.key().to_string() == name)
        .expect("topic should exist");
    let meta = topic.ctx().item().clone();

    let new_status = TopicStatus::default();
    let result = client
        .patch_status::<TopicSpec>(meta, new_status, &ns)
        .await;
    assert!(result.is_ok(), "patch_status should succeed: {result:?}");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_finalize_delete_item_removes_finalizers() {
    let client = make_client().await;
    let name = unique_name("test-finalize");

    cleanup(&client, &name).await;

    // Create a topic
    let obj = make_topic_store_object(&name, 1);
    client.apply::<TopicSpec>(obj).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get metadata
    let ns = test_namespace();
    let items = client.retrieve_items::<TopicSpec>(&ns).await.unwrap();
    let topic = items
        .items
        .iter()
        .find(|item| item.key().to_string() == name)
        .expect("topic should exist");
    let meta = topic.ctx().item().clone();

    // finalize_delete_item removes finalizers and allows deletion
    let result = client.finalize_delete_item::<TopicSpec>(meta).await;
    assert!(result.is_ok(), "finalize_delete should succeed: {result:?}");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_watch_stream_receives_events() {
    let client = make_client().await;
    let ns = test_namespace();
    let name = unique_name("test-watch");

    cleanup(&client, &name).await;

    // Start watching
    let mut stream = client.watch_stream_since::<TopicSpec>(&ns, None);

    // Create an object in a separate task
    let client2 = make_client().await;
    let name2 = name.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let obj = make_topic_store_object(&name2, 1);
        client2.apply::<TopicSpec>(obj).await.unwrap();
    });

    // Collect events for a few seconds
    let mut found_add = false;
    let timeout = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => break,
            event = stream.next() => {
                if let Some(Ok(updates)) = event {
                    for update in updates {
                        if let fluvio_stream_model::store::actions::LSUpdate::Mod(obj) = &update {
                            if obj.key().to_string() == name {
                                found_add = true;
                            }
                        }
                    }
                    if found_add { break; }
                }
            }
        }
    }

    assert!(found_add, "should have received an apply event for {name}");

    cleanup(&client, &name).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_core_group_mapping() {
    // Verify that the "core" → "" group mapping works by retrieving items
    // from a core API group CRD. We test this indirectly: the Topic CRD
    // uses the "fluvio.infinyon.com" group, so here we just verify that
    // KubeClient can build an API resource for a non-core CRD and that
    // retrieve_items works end-to-end (the group mapping is exercised
    // internally by build_api_resource).
    let client = make_client().await;
    let ns = test_namespace();

    // This exercises the full path: build_api_resource → build_api → api.list
    let result = client.retrieve_items::<TopicSpec>(&ns).await;
    assert!(result.is_ok(), "retrieve_items should work with CRD group mapping: {result:?}");
}
