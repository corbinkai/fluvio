//! In-memory implementation of `MetadataClient<K8MetaItem>`.
//!
//! Replaces k8-client's `MemoryClient` without depending on the k8-client crate.
//! Used for `--read-only` and `--local` SC modes.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_channel::{Sender, Receiver, bounded};
use async_lock::RwLock;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt, FutureExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::debug;

use fluvio_stream_model::{
    k8_types::{Spec as K8Spec, K8Obj, K8Watch},
    store::{
        MetadataStoreList,
        k8::{K8MetaItem, K8ExtendedSpec, K8ConvertError},
        MetadataStoreObject, NameSpace,
        actions::LSUpdate,
    },
    core::{Spec, MetadataItem},
};

use super::MetadataClient;

/// Per-type store: holds serialized K8 objects and a watch channel.
struct TypeStore {
    data: RwLock<HashMap<String, serde_json::Value>>,
    sender: Sender<serde_json::Value>,
    receiver: Receiver<serde_json::Value>,
    version_counter: RwLock<u64>,
}

impl Default for TypeStore {
    fn default() -> Self {
        let (sender, receiver) = bounded(100);
        Self {
            data: RwLock::new(HashMap::new()),
            sender,
            receiver,
            version_counter: RwLock::new(0),
        }
    }
}

impl TypeStore {
    async fn next_version(&self) -> String {
        let mut v = self.version_counter.write().await;
        *v += 1;
        v.to_string()
    }

    async fn current_version(&self) -> String {
        self.version_counter.read().await.to_string()
    }

    async fn insert_k8_obj<S>(&self, key: String, mut k8_obj: K8Obj<S>) -> Result<()>
    where
        S: K8Spec + Serialize + Clone + std::fmt::Debug,
        S::Status: Serialize + Clone,
        S::Header: Serialize + Clone,
    {
        let mut lock = self.data.write().await;
        let is_update = lock.contains_key(&key);

        k8_obj.metadata.resource_version = self.next_version().await;

        let watch: K8Watch<S> = if is_update {
            K8Watch::MODIFIED(k8_obj.clone())
        } else {
            K8Watch::ADDED(k8_obj.clone())
        };

        let value = serde_json::to_value(&k8_obj)?;
        lock.insert(key, value);
        drop(lock);

        let watch_value = serde_json::to_value(&watch)?;
        let _ = self.sender.send(watch_value).await;

        Ok(())
    }

    async fn remove_k8_obj<S>(&self, key: &str) -> Result<()>
    where
        S: K8Spec + DeserializeOwned + Clone,
        S::Status: DeserializeOwned + Clone,
        S::Header: DeserializeOwned + Clone,
    {
        let mut lock = self.data.write().await;
        if let Some(value) = lock.remove(key) {
            drop(lock);
            let k8_obj: K8Obj<S> = serde_json::from_value(value)?;
            let watch: K8Watch<S> = K8Watch::DELETED(k8_obj);
            let watch_value = serde_json::to_value(&watch)?;
            let _ = self.sender.send(watch_value).await;
        }
        Ok(())
    }

    async fn list_k8_objs<S>(&self) -> Result<(Vec<K8Obj<S>>, String)>
    where
        S: K8Spec + DeserializeOwned,
        S::Status: DeserializeOwned,
        S::Header: DeserializeOwned,
    {
        let lock = self.data.read().await;
        let mut items = Vec::with_capacity(lock.len());
        for value in lock.values() {
            let obj: K8Obj<S> = serde_json::from_value(value.clone())?;
            items.push(obj);
        }
        let version = self.current_version().await;
        Ok((items, version))
    }

    async fn update_status_k8_obj<S>(&self, key: &str, status: S::Status) -> Result<K8Obj<S>>
    where
        S: K8Spec + DeserializeOwned + Serialize + Clone,
        S::Status: DeserializeOwned + Serialize + Clone,
        S::Header: DeserializeOwned + Serialize + Clone,
    {
        let lock = self.data.read().await;
        let value = lock.get(key).ok_or_else(|| anyhow!("object not found: {key}"))?;
        let mut k8_obj: K8Obj<S> = serde_json::from_value(value.clone())?;
        drop(lock);

        k8_obj.status = status;
        self.insert_k8_obj(key.to_string(), k8_obj.clone()).await?;
        Ok(k8_obj)
    }

    fn watch_stream<S>(&self) -> BoxStream<'static, Result<Vec<K8Watch<S>>>>
    where
        S: K8Spec + DeserializeOwned + 'static,
        S::Status: DeserializeOwned + 'static,
        S::Header: DeserializeOwned + 'static,
    {
        self.receiver
            .clone()
            .map(|value| {
                let watch: K8Watch<S> = serde_json::from_value(value)?;
                Ok(vec![watch])
            })
            .boxed()
    }
}

/// In-memory metadata client that implements `MetadataClient<K8MetaItem>`.
///
/// Stores objects grouped by K8 spec kind. Supports create, read, update, delete,
/// and watch streams with ADDED/MODIFIED/DELETED events.
#[derive(Default)]
pub struct InMemoryClient {
    stores: async_lock::Mutex<HashMap<String, Arc<TypeStore>>>,
}

impl InMemoryClient {
    pub fn new() -> Self {
        Self::default()
    }

    async fn get_store<S: K8ExtendedSpec>(&self) -> Arc<TypeStore> {
        let kind = S::LABEL.to_string();
        let mut stores = self.stores.lock().await;
        stores.entry(kind).or_insert_with(|| Arc::new(TypeStore::default())).clone()
    }

    /// Pre-load a K8Obj into the store. Used by create_memory_client to seed topics.
    pub async fn load_k8_obj<S>(&self, k8_obj: K8Obj<S::K8Spec>) -> Result<()>
    where
        S: K8ExtendedSpec,
        S::K8Spec: Serialize + Clone + std::fmt::Debug,
        <S::K8Spec as K8Spec>::Status: Serialize + Clone,
        <S::K8Spec as K8Spec>::Header: Serialize + Clone,
    {
        let store = self.get_store::<S>().await;
        let name = k8_obj.metadata.name.clone();
        store.insert_k8_obj(name, k8_obj).await
    }
}

#[async_trait]
impl MetadataClient<K8MetaItem> for InMemoryClient {
    async fn retrieve_items<S>(
        &self,
        _namespace: &NameSpace,
    ) -> Result<MetadataStoreList<S, K8MetaItem>>
    where
        S: K8ExtendedSpec,
    {
        let store = self.get_store::<S>().await;
        let (k8_items, version) = store.list_k8_objs::<S::K8Spec>().await?;

        let multi_namespace = false;
        let mut items = Vec::with_capacity(k8_items.len());
        for k8_obj in k8_items {
            match S::convert_from_k8(k8_obj, multi_namespace) {
                Ok(converted) => items.push(converted),
                Err(K8ConvertError::Skip(_)) => continue,
                Err(K8ConvertError::KeyConvertionError(err)) => return Err(err.into()),
                Err(K8ConvertError::Other(err)) => return Err(err.into()),
            }
        }

        Ok(MetadataStoreList { version, items })
    }

    async fn delete_item<S>(&self, metadata: K8MetaItem) -> Result<()>
    where
        S: K8ExtendedSpec,
    {
        let store = self.get_store::<S>().await;
        store.remove_k8_obj::<S::K8Spec>(&metadata.name).await
    }

    async fn finalize_delete_item<S>(&self, metadata: K8MetaItem) -> Result<()>
    where
        S: K8ExtendedSpec,
    {
        // In memory, finalize is the same as delete
        self.delete_item::<S>(metadata).await
    }

    async fn apply<S>(&self, value: MetadataStoreObject<S, K8MetaItem>) -> Result<()>
    where
        S: K8ExtendedSpec,
        <S as Spec>::Owner: K8ExtendedSpec,
    {
        let (key, spec, _status, ctx) = value.parts();
        let k8_spec: S::K8Spec = spec.into_k8();
        let name = key.to_string();

        let mut metadata = ctx.item().inner().clone();
        metadata.name = name.clone();
        metadata.labels = ctx.item().get_labels();
        metadata.annotations.clone_from(&ctx.item().annotations);

        // Handle owner references
        if let Some(parent) = ctx.item().owner() {
            use fluvio_stream_model::k8_types::OwnerReferences;
            metadata.owner_references = vec![OwnerReferences {
                api_version: <<S as Spec>::Owner as K8ExtendedSpec>::K8Spec::api_version(),
                kind: <<S as Spec>::Owner as K8ExtendedSpec>::K8Spec::kind(),
                name: parent.name.clone(),
                uid: parent.uid.clone(),
                block_owner_deletion: S::FINALIZER.is_some(),
                ..Default::default()
            }];
            if let Some(finalizer) = S::FINALIZER {
                metadata.finalizers = vec![finalizer.to_owned()];
            }
        }

        let k8_obj = K8Obj {
            api_version: S::K8Spec::api_version(),
            kind: S::K8Spec::kind(),
            metadata,
            spec: k8_spec,
            ..Default::default()
        };

        let store = self.get_store::<S>().await;
        store.insert_k8_obj(name, k8_obj).await
    }

    async fn update_spec<S>(&self, metadata: K8MetaItem, spec: S) -> Result<()>
    where
        S: K8ExtendedSpec,
    {
        let k8_spec: S::K8Spec = spec.into_k8();
        let name = metadata.name.clone();

        let store = self.get_store::<S>().await;
        let (items, _) = store.list_k8_objs::<S::K8Spec>().await?;
        let existing = items.into_iter().find(|obj| obj.metadata.name == name);

        let k8_obj = if let Some(mut obj) = existing {
            obj.spec = k8_spec;
            obj
        } else {
            K8Obj {
                api_version: S::K8Spec::api_version(),
                kind: S::K8Spec::kind(),
                metadata: metadata.inner().clone(),
                spec: k8_spec,
                ..Default::default()
            }
        };

        store.insert_k8_obj(name, k8_obj).await
    }

    async fn update_spec_by_key<S>(
        &self,
        key: S::IndexKey,
        namespace: &NameSpace,
        spec: S,
    ) -> Result<()>
    where
        S: K8ExtendedSpec,
    {
        let meta = K8MetaItem::new(key.to_string(), namespace.to_string());
        self.update_spec(meta, spec).await
    }

    async fn update_status<S>(
        &self,
        metadata: K8MetaItem,
        status: S::Status,
        _namespace: &NameSpace,
    ) -> Result<MetadataStoreObject<S, K8MetaItem>>
    where
        S: K8ExtendedSpec,
    {
        let store = self.get_store::<S>().await;
        let k8_status = S::convert_status_from_k8(status);
        let k8_obj = store.update_status_k8_obj::<S::K8Spec>(&metadata.name, k8_status).await?;

        S::convert_from_k8(k8_obj, false)
            .map_err(|e| anyhow!("error converting back: {e:#?}"))
    }

    fn watch_stream_since<S>(
        &self,
        _namespace: &NameSpace,
        _resource_version: Option<String>,
    ) -> BoxStream<'_, Result<Vec<LSUpdate<S, K8MetaItem>>>>
    where
        S: K8ExtendedSpec,
    {
        let ft = async move {
            let store = self.get_store::<S>().await;
            let stream = store.watch_stream::<S::K8Spec>();

            stream.map(move |result| {
                let watches = result?;
                let mut updates = Vec::new();
                for watch in watches {
                    match watch {
                        K8Watch::ADDED(k8_obj) | K8Watch::MODIFIED(k8_obj) => {
                            match S::convert_from_k8(k8_obj, false) {
                                Ok(converted) => updates.push(LSUpdate::Mod(converted)),
                                Err(K8ConvertError::Skip(_)) => {}
                                Err(e) => {
                                    debug!("convert error in watch: {e:#?}");
                                }
                            }
                        }
                        K8Watch::DELETED(k8_obj) => {
                            match S::convert_from_k8(k8_obj, false) {
                                Ok(converted) => updates.push(LSUpdate::Delete(converted.key_owned())),
                                Err(K8ConvertError::Skip(_)) => {}
                                Err(e) => {
                                    debug!("convert error in watch: {e:#?}");
                                }
                            }
                        }
                    }
                }
                Ok(updates)
            }).boxed()
        };

        ft.flatten_stream().boxed()
    }

    async fn patch_status<S>(
        &self,
        metadata: K8MetaItem,
        status: S::Status,
        namespace: &NameSpace,
    ) -> Result<MetadataStoreObject<S, K8MetaItem>>
    where
        S: K8ExtendedSpec,
    {
        // In memory, patch_status is the same as update_status
        self.update_status(metadata, status, namespace).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::fixture::{TestSpec, TestStatus};

    #[fluvio_future::test]
    async fn test_apply_and_retrieve() {
        let client = InMemoryClient::new();
        let meta_object: MetadataStoreObject<TestSpec, K8MetaItem> = MetadataStoreObject::new(
            "spec1".to_string(),
            TestSpec::default(),
            TestStatus("ok".to_string()),
        );

        // Empty initially
        let empty = client.retrieve_items::<TestSpec>(&NameSpace::All).await.unwrap();
        assert!(empty.items.is_empty());

        // Apply
        client.apply::<TestSpec>(meta_object.clone()).await.unwrap();

        // Retrieve
        let items = client.retrieve_items::<TestSpec>(&NameSpace::All).await.unwrap();
        assert_eq!(items.items.len(), 1);
    }

    #[fluvio_future::test]
    async fn test_delete_item() {
        let client = InMemoryClient::new();
        let key = "spec1".to_string();
        let meta = K8MetaItem::new(key.clone(), "default".to_string());
        let meta_object: MetadataStoreObject<TestSpec, K8MetaItem> =
            MetadataStoreObject::new_with_context(
                key,
                TestSpec::default(),
                meta.clone().into(),
            );

        client.apply::<TestSpec>(meta_object.clone()).await.unwrap();
        let items = client.retrieve_items::<TestSpec>(&NameSpace::All).await.unwrap();
        assert_eq!(items.items.len(), 1);

        client.delete_item::<TestSpec>(meta).await.unwrap();
        let items = client.retrieve_items::<TestSpec>(&NameSpace::All).await.unwrap();
        assert!(items.items.is_empty());
    }

    #[fluvio_future::test]
    async fn test_update_status() {
        let client = InMemoryClient::new();
        let ns = NameSpace::Named("ns1".to_string());
        let key = "key".to_string();
        let meta = K8MetaItem::new(key.clone(), ns.to_string());
        let meta_object: MetadataStoreObject<TestSpec, K8MetaItem> =
            MetadataStoreObject::new_with_context(
                key.clone(),
                TestSpec::default(),
                meta.clone().into(),
            );

        client.apply(meta_object.clone()).await.unwrap();
        let result = client
            .update_status::<TestSpec>(meta, TestStatus("new status".to_string()), &ns)
            .await
            .unwrap();

        assert_eq!(result.status().to_string(), "new status");
    }

    #[fluvio_future::test]
    async fn test_watch_stream() {
        use std::time::Duration;

        let client = InMemoryClient::new();
        let ns = NameSpace::Named("ns1".to_string());
        let stream = client.watch_stream_since::<TestSpec>(&ns, None);

        let key = "key".to_string();
        let meta = K8MetaItem::new(key.clone(), "default".to_string());
        let meta_object: MetadataStoreObject<TestSpec, K8MetaItem> =
            MetadataStoreObject::new_with_context(
                key,
                TestSpec::default(),
                meta.clone().into(),
            );

        client.apply::<TestSpec>(meta_object.clone()).await.unwrap();
        client.delete_item::<TestSpec>(meta).await.unwrap();

        let updates = stream
            .take_until(fluvio_future::timer::sleep(Duration::from_secs(2)))
            .collect::<Vec<Result<Vec<LSUpdate<TestSpec, K8MetaItem>>>>>()
            .await;

        let updates: Vec<_> = updates.into_iter().flatten().flatten().collect();
        assert_eq!(updates.len(), 2);
        assert!(matches!(updates[0], LSUpdate::Mod(_)));
        assert!(matches!(updates[1], LSUpdate::Delete(_)));
    }
}
