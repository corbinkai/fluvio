use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use futures_util::StreamExt;
use kube::{
    Api, Client, CustomResource, ResourceExt,
    api::{DeleteParams, ListParams},
    runtime::controller::{Action, Controller},
};
use k8s_openapi::api::core::v1::Namespace;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

/// Label applied to Fluvio topics to associate them with a source namespace.
/// When the namespace is deleted, topics with this label are garbage collected.
const SOURCE_NAMESPACE_LABEL: &str = "fluvio.io/source-namespace";

/// Fluvio Topic CRD spec — we only need enough to list and delete topics,
/// so the spec can be minimal. The actual TopicSpec is complex but we don't
/// need to read or write it.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "fluvio.infinyon.com",
    version = "v2",
    kind = "Topic",
    plural = "topics",
    namespaced = false
)]
pub struct TopicSpec {
    /// We don't need to parse the full spec — serde will ignore unknown fields
    /// with the default deny_unknown_fields = false behavior.
    #[serde(default)]
    pub replicas: serde_json::Value,
}

#[derive(Parser)]
#[command(name = "fluvio-namespace-gc")]
#[command(about = "Garbage-collect Fluvio topics when their source namespace is deleted")]
struct Args {
    /// Dry run — log deletions without actually deleting
    #[arg(long, env = "DRY_RUN")]
    dry_run: bool,
}

struct Context {
    client: Client,
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fluvio_namespace_gc=info,kube=warn".into()),
        )
        .init();

    let args = Args::parse();
    let client = Client::try_default().await?;

    info!("starting fluvio-namespace-gc controller");
    if args.dry_run {
        warn!("DRY RUN mode — topics will not be deleted");
    }

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let topics: Api<Topic> = Api::all(client.clone());

    // Verify we can access the Topic CRD
    match topics.list(&ListParams::default().limit(1)).await {
        Ok(_) => info!("verified access to Fluvio Topic CRD"),
        Err(err) => {
            error!(%err, "cannot list Fluvio topics — is the CRD installed?");
            return Err(err.into());
        }
    }

    let ctx = Arc::new(Context {
        client: client.clone(),
        dry_run: args.dry_run,
    });

    let controller = Controller::new(namespaces, kube::runtime::watcher::Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|result| async move {
            match result {
                Ok((_obj, _action)) => {}
                Err(err) => {
                    error!(%err, "reconciliation error");
                }
            }
        });

    info!("controller running");
    controller.await;
    info!("controller shut down");

    Ok(())
}

async fn reconcile(ns: Arc<Namespace>, ctx: Arc<Context>) -> Result<Action, kube::Error> {
    let ns_name = ns.name_any();

    // Only act on namespaces that are being deleted
    if ns.metadata.deletion_timestamp.is_none() {
        return Ok(Action::await_change());
    }

    debug!(%ns_name, "namespace is being deleted, checking for associated topics");

    let topics: Api<Topic> = Api::all(ctx.client.clone());
    let label_selector = format!("{SOURCE_NAMESPACE_LABEL}={ns_name}");
    let topic_list = topics
        .list(&ListParams::default().labels(&label_selector))
        .await?;

    if topic_list.items.is_empty() {
        debug!(%ns_name, "no topics found with source-namespace label");
        return Ok(Action::await_change());
    }

    info!(
        ns = %ns_name,
        count = topic_list.items.len(),
        "deleting topics associated with namespace"
    );

    for topic in &topic_list.items {
        let topic_name = topic.name_any();
        if ctx.dry_run {
            info!(%topic_name, %ns_name, "DRY RUN: would delete topic");
        } else {
            info!(%topic_name, %ns_name, "deleting topic");
            match topics.delete(&topic_name, &DeleteParams::default()).await {
                Ok(_) => {
                    info!(%topic_name, "topic deleted");
                }
                Err(kube::Error::Api(err)) if err.code == 404 => {
                    debug!(%topic_name, "topic already deleted");
                }
                Err(err) => {
                    error!(%topic_name, %err, "failed to delete topic");
                    return Err(err);
                }
            }
        }
    }

    Ok(Action::await_change())
}

fn error_policy(_obj: Arc<Namespace>, err: &kube::Error, _ctx: Arc<Context>) -> Action {
    error!(%err, "reconciliation error, retrying in 30s");
    Action::requeue(std::time::Duration::from_secs(30))
}
