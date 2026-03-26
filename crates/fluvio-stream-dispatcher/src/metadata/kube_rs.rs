use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use tracing::{debug, error, trace};

use kube::api::{Api, DynamicObject, ApiResource, GroupVersionKind, ListParams, DeleteParams, Patch, PatchParams, PostParams};
use kube::runtime::watcher;
use kube::runtime::WatchStreamExt;
use kube::Client;

use fluvio_stream_model::{
    k8_types::{
        Spec as K8Spec,
        K8Obj,
    },
    store::{
        MetadataStoreList,
        k8::{K8MetaItem, K8ExtendedSpec, K8ConvertError},
        MetadataStoreObject, NameSpace,
        actions::LSUpdate,
    },
    core::{Spec, MetadataItem},
};

use super::MetadataClient;

/// Wrapper around kube::Client that implements MetadataClient<K8MetaItem>.
/// Uses DynamicObject + serde round-trips to convert between kube-rs types
/// and k8-types K8Obj (which the rest of the system uses).
#[derive(Clone)]
pub struct KubeClient {
    client: Client,
}

impl KubeClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub fn inner(&self) -> &Client {
        &self.client
    }
}

fn build_api_resource<S: K8ExtendedSpec>() -> ApiResource {
    let crd = S::K8Spec::metadata();
    // k8-types uses "core" for core API group, but kube-rs uses "" (empty string).
    // The core group maps to /api/v1/ not /apis/core/v1/.
    let group = if crd.group == "core" { "" } else { crd.group };
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        group, crd.version, crd.names.kind,
    ))
}

fn build_api<S: K8ExtendedSpec>(client: &Client, namespace: &NameSpace) -> Api<DynamicObject> {
    let ar = build_api_resource::<S>();
    match namespace {
        NameSpace::All => Api::all_with(client.clone(), &ar),
        NameSpace::Named(ns) => Api::namespaced_with(client.clone(), ns, &ar),
    }
}

/// Convert a DynamicObject (from kube-rs) into a K8Obj (k8-types) via serde JSON.
fn dynamic_to_k8obj<K: K8Spec>(obj: DynamicObject) -> Result<K8Obj<K>>
where
    K: serde::de::DeserializeOwned,
{
    let value = serde_json::to_value(obj)?;
    let k8_obj: K8Obj<K> = serde_json::from_value(value)?;
    Ok(k8_obj)
}

#[async_trait]
impl MetadataClient<K8MetaItem> for KubeClient {
    async fn retrieve_items<S>(
        &self,
        namespace: &NameSpace,
    ) -> Result<MetadataStoreList<S, K8MetaItem>>
    where
        S: K8ExtendedSpec,
    {
        let multi_namespace_context = matches!(namespace, NameSpace::All);
        let api = build_api::<S>(&self.client, namespace);
        let object_list = api.list(&ListParams::default()).await?;

        let version = object_list
            .metadata
            .resource_version
            .unwrap_or_default();

        let mut items = Vec::with_capacity(object_list.items.len());
        for obj in object_list.items {
            let k8_obj: K8Obj<S::K8Spec> = match dynamic_to_k8obj(obj) {
                Ok(o) => o,
                Err(err) => {
                    error!("error converting dynamic object: {err}");
                    continue;
                }
            };
            match S::convert_from_k8(k8_obj, multi_namespace_context) {
                Ok(converted) => {
                    trace!("converted val: {converted:#?}");
                    items.push(converted);
                }
                Err(K8ConvertError::Skip(obj)) => {
                    debug!("skipping: {} {}", S::LABEL, obj.metadata.name);
                    continue;
                }
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
        let api = build_api::<S>(&self.client, &NameSpace::Named(metadata.namespace.clone()));
        api.delete(&metadata.name, &DeleteParams::default()).await?;
        Ok(())
    }

    async fn finalize_delete_item<S>(&self, meta: K8MetaItem) -> Result<()>
    where
        S: K8ExtendedSpec,
    {
        let api = build_api::<S>(&self.client, &NameSpace::Named(meta.namespace.clone()));
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": null
            }
        });
        api.patch(
            &meta.name,
            &PatchParams::default(),
            &Patch::Merge(patch),
        )
        .await?;
        Ok(())
    }

    async fn apply<S>(&self, value: MetadataStoreObject<S, K8MetaItem>) -> Result<()>
    where
        S: K8ExtendedSpec,
        <S as Spec>::Owner: K8ExtendedSpec,
    {
        debug!(label = S::LABEL, key = ?value.key(), "K8 applying via kube-rs");
        trace!("adding KV {:#?} to k8 kv", value);

        let (key, spec, _status, ctx) = value.parts();
        let k8_spec: S::K8Spec = spec.into_k8();

        // Build the object to apply
        let mut metadata_json = serde_json::to_value(ctx.item().inner())?;

        // Override name from key
        if let Some(obj) = metadata_json.as_object_mut() {
            obj.insert("name".into(), serde_json::Value::String(key.to_string()));
            // Set labels from context
            let labels = ctx.item().get_labels();
            if !labels.is_empty() {
                obj.insert("labels".into(), serde_json::to_value(&labels)?);
            }
            // Set annotations from context
            let annotations = &ctx.item().annotations;
            if !annotations.is_empty() {
                obj.insert("annotations".into(), serde_json::to_value(annotations)?);
            }
        }

        // Handle owner references from parent
        if let Some(parent_metadata) = ctx.item().owner() {
            let owner_ref = serde_json::json!([{
                "apiVersion": <<S as Spec>::Owner as K8ExtendedSpec>::K8Spec::api_version(),
                "kind": <<S as Spec>::Owner as K8ExtendedSpec>::K8Spec::kind(),
                "name": parent_metadata.name,
                "uid": parent_metadata.uid,
                "blockOwnerDeletion": S::FINALIZER.is_some(),
            }]);
            if let Some(obj) = metadata_json.as_object_mut() {
                obj.insert("ownerReferences".into(), owner_ref);
                if let Some(finalizer) = S::FINALIZER {
                    obj.insert("finalizers".into(), serde_json::json!([finalizer]));
                }
            }
        }

        let apply_obj = serde_json::json!({
            "apiVersion": S::K8Spec::api_version(),
            "kind": S::K8Spec::kind(),
            "metadata": metadata_json,
            "spec": k8_spec,
        });

        let api = build_api::<S>(&self.client, &NameSpace::Named(ctx.item().namespace.clone()));
        api.patch(
            &key.to_string(),
            &PatchParams::apply("fluvio-sc").force(),
            &Patch::Apply(apply_obj),
        )
        .await?;
        Ok(())
    }

    async fn update_spec<S>(&self, metadata: K8MetaItem, spec: S) -> Result<()>
    where
        S: K8ExtendedSpec,
    {
        debug!("K8 Update Spec: {} key: {}", S::LABEL, metadata.name);
        let k8_spec: S::K8Spec = spec.into_k8();

        let apply_obj = serde_json::json!({
            "apiVersion": S::K8Spec::api_version(),
            "kind": S::K8Spec::kind(),
            "metadata": {
                "name": metadata.name,
                "namespace": metadata.namespace,
            },
            "spec": k8_spec,
        });

        let api = build_api::<S>(&self.client, &NameSpace::Named(metadata.namespace.clone()));
        api.patch(
            &metadata.name,
            &PatchParams::apply("fluvio-sc").force(),
            &Patch::Apply(apply_obj),
        )
        .await?;
        Ok(())
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
        namespace: &NameSpace,
    ) -> Result<MetadataStoreObject<S, K8MetaItem>>
    where
        S: K8ExtendedSpec,
    {
        debug!(
            key = %metadata.name,
            version = %metadata.resource_version,
            "update status begin (kube-rs)",
        );
        let k8_status: <S::K8Spec as K8Spec>::Status = S::convert_status_from_k8(status);

        let status_obj = serde_json::json!({
            "apiVersion": S::K8Spec::api_version(),
            "kind": S::K8Spec::kind(),
            "metadata": {
                "name": metadata.name,
                "namespace": metadata.namespace,
                "resourceVersion": metadata.resource_version,
            },
            "status": k8_status,
        });

        let api = build_api::<S>(&self.client, namespace);
        let result = api
            .replace_status(
                &metadata.name,
                &PostParams::default(),
                serde_json::to_vec(&status_obj)?,
            )
            .await?;

        let multi_namespace_context = matches!(namespace, NameSpace::All);
        let k8_obj: K8Obj<S::K8Spec> = dynamic_to_k8obj(result)?;
        S::convert_from_k8(k8_obj, multi_namespace_context)
            .map_err(|e| anyhow!("{}, error converting back: {e:#?}", S::LABEL))
    }

    fn watch_stream_since<S>(
        &self,
        namespace: &NameSpace,
        _resource_version: Option<String>,
    ) -> BoxStream<'_, Result<Vec<LSUpdate<S, K8MetaItem>>>>
    where
        S: K8ExtendedSpec,
    {
        let multi_namespace_context = matches!(namespace, NameSpace::All);
        let api = build_api::<S>(&self.client, namespace);

        let config = watcher::Config::default();

        // Use applied_objects() which flattens watch events into a stream of
        // individual objects with their action (add/modify/delete).
        // For deleted objects, we get a separate notification.
        let stream = watcher::watcher(api, config)
            .default_backoff()
            .map(move |event| {
                let mut changes: Vec<LSUpdate<S, K8MetaItem>> = Vec::new();
                match event {
                    Ok(watcher::Event::Apply(obj) | watcher::Event::InitApply(obj)) => {
                        match dynamic_to_k8obj::<S::K8Spec>(obj) {
                            Ok(k8_obj) => {
                                match S::convert_from_k8(k8_obj, multi_namespace_context) {
                                    Ok(converted) => {
                                        debug!("K8: Watch Apply: {}:{:?}", S::LABEL, converted.key());
                                        changes.push(LSUpdate::Mod(converted));
                                    }
                                    Err(K8ConvertError::Skip(obj)) => {
                                        debug!("skipping: {}", obj.metadata.name);
                                    }
                                    Err(err) => {
                                        error!("converting {} {:#?}", S::LABEL, err);
                                    }
                                }
                            }
                            Err(err) => {
                                error!("error converting dynamic object in watch: {err}");
                            }
                        }
                    }
                    Ok(watcher::Event::Delete(obj)) => {
                        match dynamic_to_k8obj::<S::K8Spec>(obj) {
                            Ok(k8_obj) => {
                                match S::convert_from_k8(k8_obj, multi_namespace_context) {
                                    Ok(kv_value) => {
                                        debug!("K8: Watch Delete {}:{:?}", S::LABEL, kv_value.key());
                                        changes.push(LSUpdate::Delete(kv_value.key_owned()));
                                    }
                                    Err(K8ConvertError::Skip(obj)) => {
                                        debug!("skipping: {}", obj.metadata.name);
                                    }
                                    Err(err) => {
                                        error!("converting {} {:#?}", S::LABEL, err);
                                    }
                                }
                            }
                            Err(err) => {
                                error!("error converting dynamic object in watch: {err}");
                            }
                        }
                    }
                    Ok(watcher::Event::Init | watcher::Event::InitDone) => {
                        // Initial list events — no action needed, InitApply handles items
                    }
                    Err(err) => {
                        error!("watcher error for {}: {err}", S::LABEL);
                    }
                }
                Ok(changes)
            });

        stream.boxed()
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
        debug!(
            key = %metadata.name,
            version = %metadata.resource_version,
            "patch status begin (kube-rs)",
        );
        let k8_status: <S::K8Spec as K8Spec>::Status = S::convert_status_from_k8(status);

        let patch = serde_json::json!({
            "apiVersion": S::K8Spec::api_version(),
            "kind": S::K8Spec::kind(),
            "metadata": {
                "name": metadata.name,
                "namespace": metadata.namespace,
            },
            "status": k8_status,
        });

        let api = build_api::<S>(&self.client, namespace);
        let result = api
            .patch_status(
                &metadata.name,
                &PatchParams::apply("fluvio").force(),
                &Patch::Apply(patch),
            )
            .await?;

        let multi_namespace_context = matches!(namespace, NameSpace::All);
        let k8_obj: K8Obj<S::K8Spec> = dynamic_to_k8obj(result)?;
        S::convert_from_k8(k8_obj, multi_namespace_context)
            .map_err(|e| anyhow!("{}, error converting back: {e:#?}", S::LABEL))
    }
}
