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

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{ServiceSpec, ServicePort, ServiceStatus, LoadBalancerStatus, LoadBalancerIngress};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    fn make_service(name: &str, ns: &str, svc_type: Option<&str>, port: i32, labels: Option<BTreeMap<String, String>>, annotations: Option<BTreeMap<String, String>>) -> Service {
        Service {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(ns.to_string()),
                labels,
                annotations,
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: svc_type.map(|s| s.to_string()),
                ports: Some(vec![ServicePort { port, ..Default::default() }]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn spu_label(name: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("fluvio.io/spu-name".to_string(), name.to_string());
        m
    }

    fn ingress_annotation(addr: &str) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("fluvio.io/ingress-address".to_string(), addr.to_string());
        m
    }

    #[test]
    fn test_clusterip_service_gets_fqdn_fallback() {
        let svc = make_service("fluvio-spu-main-0", "fluvio-system", Some("ClusterIP"), 9005, None, None);
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        assert_eq!(result.port, 9005);
        assert_eq!(result.ingress.len(), 1);
        assert_eq!(
            result.ingress[0].hostname,
            Some("fluvio-spu-main-0.fluvio-system.svc.cluster.local".to_string())
        );
    }

    #[test]
    fn test_clusterip_default_type_gets_fqdn() {
        // No type_ set → defaults to ClusterIP
        let svc = make_service("fluvio-spu-main-0", "default", None, 9005, None, None);
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        assert_eq!(result.ingress.len(), 1);
        assert!(result.ingress[0].hostname.as_ref().unwrap().contains("default.svc.cluster.local"));
    }

    #[test]
    fn test_loadbalancer_service_gets_lb_ingress() {
        let mut svc = make_service("fluvio-spu-main-0", "ns", Some("LoadBalancer"), 9005, None, None);
        svc.status = Some(ServiceStatus {
            load_balancer: Some(LoadBalancerStatus {
                ingress: Some(vec![LoadBalancerIngress {
                    hostname: Some("lb.example.com".to_string()),
                    ..Default::default()
                }]),
            }),
            ..Default::default()
        });
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        assert_eq!(result.ingress.len(), 1);
        assert_eq!(result.ingress[0].hostname, Some("lb.example.com".to_string()));
    }

    #[test]
    fn test_nodeport_service_gets_node_port() {
        let mut svc = make_service("fluvio-spu-main-0", "ns", Some("NodePort"), 9005, None, None);
        svc.spec.as_mut().unwrap().ports = Some(vec![ServicePort {
            port: 9005,
            node_port: Some(30005),
            ..Default::default()
        }]);
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        assert_eq!(result.port, 30005);
    }

    #[test]
    fn test_annotation_overrides_fqdn() {
        let svc = make_service(
            "fluvio-spu-main-0", "ns", Some("ClusterIP"), 9005,
            None, Some(ingress_annotation("custom.host.io")),
        );
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        // Annotation is added first, then no FQDN fallback since ingress is non-empty
        assert_eq!(result.ingress.len(), 1);
        assert_eq!(result.ingress[0].hostname, Some("custom.host.io".to_string()));
    }

    #[test]
    fn test_annotation_ip_parsed_correctly() {
        let svc = make_service(
            "svc", "ns", Some("ClusterIP"), 9005,
            None, Some(ingress_annotation("10.0.0.1")),
        );
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        assert_eq!(result.ingress[0].ip, Some("10.0.0.1".to_string()));
        assert_eq!(result.ingress[0].hostname, None);
    }

    #[test]
    fn test_annotation_hostname_parsed_correctly() {
        let svc = make_service(
            "svc", "ns", Some("ClusterIP"), 9005,
            None, Some(ingress_annotation("my-custom-host.example.com")),
        );
        let result = get_ingress_from_k8s_service(&svc).unwrap();
        assert_eq!(result.ingress[0].hostname, Some("my-custom-host.example.com".to_string()));
        assert_eq!(result.ingress[0].ip, None);
    }

    #[test]
    fn test_service_without_spu_label_skipped() {
        let svc = make_service("no-label-svc", "ns", Some("ClusterIP"), 9005, None, None);
        let map = build_services_map(&[svc]);
        assert!(map.is_empty());
    }

    #[test]
    fn test_service_without_ports_returns_error() {
        let mut svc = make_service("svc", "ns", Some("ClusterIP"), 9005, None, None);
        svc.spec.as_mut().unwrap().ports = Some(vec![]);
        let result = get_ingress_from_k8s_service(&svc);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_services_map_multiple_services() {
        let svcs = vec![
            make_service("fluvio-spu-main-0", "ns", Some("ClusterIP"), 9005, Some(spu_label("main-0")), None),
            make_service("fluvio-spu-main-1", "ns", Some("ClusterIP"), 9005, Some(spu_label("main-1")), None),
            make_service("fluvio-spu-main-2", "ns", Some("ClusterIP"), 9005, Some(spu_label("main-2")), None),
        ];
        let map = build_services_map(&svcs);
        assert_eq!(map.len(), 3);
        assert!(map.contains_key("main-0"));
        assert!(map.contains_key("main-1"));
        assert!(map.contains_key("main-2"));
    }
}
