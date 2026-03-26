use std::collections::HashMap;
use std::net::IpAddr;

use tracing::debug;

use k8s_openapi::api::core::v1::Service;
use fluvio_controlplane_metadata::spu::IngressPort;
use crate::stores::spu::IngressAddr;

/// Build a map of spu_name → IngressPort from a list of K8 Services.
pub fn build_services_map(services: &[Service]) -> HashMap<String, IngressPort> {
    let mut map = HashMap::new();
    for svc in services {
        let svc_name = svc.metadata.name.as_deref().unwrap_or_default();
        if let Some(spu_name) = svc
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("fluvio.io/spu-name"))
        {
            match get_ingress_from_k8s_service(svc) {
                Ok(ingress) => {
                    map.insert(spu_name.clone(), ingress);
                }
                Err(err) => {
                    tracing::error!(%svc_name, %err, "error reading ingress from service");
                }
            }
        }
    }
    map
}

/// Extract ingress info from a k8s-openapi Service object.
fn get_ingress_from_k8s_service(svc: &Service) -> anyhow::Result<IngressPort> {
    let spec = svc.spec.as_ref().ok_or_else(|| anyhow::anyhow!("service has no spec"))?;
    let svc_name = svc.metadata.name.as_deref().unwrap_or_default();
    let namespace = svc.metadata.namespace.as_deref().unwrap_or("default");

    let ports = spec.ports.as_deref().unwrap_or_default();
    if ports.is_empty() {
        return Err(anyhow::anyhow!("service has no ports"));
    }

    let service_type = spec.type_.as_deref().unwrap_or("ClusterIP");

    let mut computed = match service_type {
        "NodePort" => {
            let port = ports[0]
                .node_port
                .ok_or_else(|| anyhow::anyhow!("SPU service missing NodePort"))? as u16;
            IngressPort {
                port,
                ..Default::default()
            }
        }
        _ => {
            let port = ports[0].port as u16;
            let lb_ingresses = svc
                .status
                .as_ref()
                .and_then(|s| s.load_balancer.as_ref())
                .and_then(|lb| lb.ingress.as_deref())
                .unwrap_or_default();

            let ingress: Vec<IngressAddr> = lb_ingresses
                .iter()
                .map(|i| IngressAddr {
                    hostname: i.hostname.clone(),
                    ip: i.ip.clone(),
                })
                .collect();

            IngressPort {
                port,
                ingress,
                ..Default::default()
            }
        }
    };

    // Add annotation-based ingress
    if let Some(address) = svc
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("fluvio.io/ingress-address"))
    {
        if let Ok(ip_addr) = address.parse::<IpAddr>() {
            computed.ingress.push(IngressAddr {
                hostname: None,
                ip: Some(ip_addr.to_string()),
            });
        } else {
            computed.ingress.push(IngressAddr {
                hostname: Some(address.clone()),
                ip: None,
            });
        }
    }

    // ClusterIP fallback to service FQDN
    if computed.ingress.is_empty()
        && matches!(service_type, "ClusterIP" | "")
    {
        let fqdn = format!("{svc_name}.{namespace}.svc.cluster.local");
        debug!(%fqdn, "ClusterIP service with no ingress, using service FQDN");
        computed.ingress.push(IngressAddr {
            hostname: Some(fqdn),
            ip: None,
        });
    }

    Ok(computed)
}
