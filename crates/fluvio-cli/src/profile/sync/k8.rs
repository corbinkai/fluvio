use std::convert::TryInto;

use clap::Parser;
use tracing::debug;
use anyhow::{Result, anyhow};

use fluvio::config::{Profile, ConfigFile};
use fluvio::FluvioClusterConfig;

use crate::common::tls::TlsClientOpt;

#[derive(Debug, Parser, Default)]
pub struct K8Opt {
    /// kubernetes namespace,
    #[arg(long, short, value_name = "namespace")]
    pub namespace: Option<String>,

    /// profile name
    #[arg(value_name = "name")]
    pub name: Option<String>,

    #[clap(flatten)]
    pub tls: TlsClientOpt,
}

impl K8Opt {
    pub async fn process(self) -> Result<()> {
        let external_addr = match discover_fluvio_addr(self.namespace.as_deref()).await? {
            Some(sc_addr) => sc_addr,
            None => return Err(anyhow!("fluvio service is not deployed")),
        };

        match set_k8_context(self, external_addr).await {
            Ok(profile) => {
                println!("updated profile: {profile:#?}");
            }
            Err(err) => {
                eprintln!("config creation failed: {err}");
            }
        }
        Ok(())
    }
}

/// compute profile name from kubeconfig current context
fn compute_profile_name() -> Result<String> {
    let kubeconfig = kube::config::Kubeconfig::read()?;
    match kubeconfig.current_context {
        Some(ctx) => Ok(ctx),
        None => Err(anyhow!("no context found")),
    }
}

/// create new k8 cluster and profile
pub async fn set_k8_context(opt: K8Opt, external_addr: String) -> Result<Profile> {
    let mut config_file = ConfigFile::load_default_or_new()?;
    let config = config_file.mut_config();

    let profile_name = if let Some(name) = &opt.name {
        name.to_owned()
    } else {
        compute_profile_name()?
    };

    match config.cluster_mut(&profile_name) {
        Some(cluster) => {
            cluster.endpoint = external_addr;
            cluster.tls = opt.tls.try_into()?;
        }
        None => {
            let mut local_cluster = FluvioClusterConfig::new(external_addr);
            local_cluster.tls = opt.tls.try_into()?;
            config.add_cluster(local_cluster, profile_name.clone());
        }
    };

    let new_profile = match config.profile_mut(&profile_name) {
        Some(profile) => {
            profile.set_cluster(profile_name.clone());
            profile.clone()
        }
        None => {
            let profile = Profile::new(profile_name.clone());
            config.add_profile(profile.clone(), profile_name.clone());
            profile
        }
    };

    assert!(config.set_current_profile(&profile_name));
    config_file.save()?;
    println!("k8 profile set");
    Ok(new_profile)
}

/// find fluvio addr by looking up the fluvio-sc-public Service
pub async fn discover_fluvio_addr(namespace: Option<&str>) -> Result<Option<String>> {
    use k8s_openapi::api::core::v1::Service;
    use kube::api::Api;

    let ns = namespace.unwrap_or("default");
    let client = kube::Client::try_default().await?;
    let services: Api<Service> = Api::namespaced(client, ns);

    let svc = match services.get_opt("fluvio-sc-public").await? {
        Some(svc) => svc,
        None => return Ok(None),
    };

    debug!("fluvio svc: {:#?}", svc);

    let ingress_addr = svc
        .status
        .as_ref()
        .and_then(|s| s.load_balancer.as_ref())
        .and_then(|lb| lb.ingress.as_ref())
        .and_then(|ingress| ingress.first())
        .and_then(|i| i.hostname.as_deref().or(i.ip.as_deref()));

    let target_port = svc
        .spec
        .as_ref()
        .and_then(|s| s.ports.as_ref())
        .and_then(|ports| ports.first())
        .and_then(|port| port.target_port.as_ref())
        .map(|tp| match tp {
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(p) => p.to_string(),
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(s) => s.clone(),
        });

    let address = match (ingress_addr, target_port) {
        (Some(addr), Some(port)) => Some(format!("{addr}:{port}")),
        _ => None,
    };

    Ok(address)
}
