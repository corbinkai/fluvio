pub mod spg_statefulset_v2;
pub mod spu_service_v2;
pub mod spu_controller_v2;
pub mod ingress;

pub use k8_operator::run_k8_operators;

mod k8_operator {

    use tracing::info;

    use crate::cli::TlsConfig;
    use crate::core::K8SharedContext;

    pub async fn run_k8_operators(
        namespace: String,
        kube_client: kube::Client,
        _global_ctx: K8SharedContext,
        tls: Option<TlsConfig>,
    ) {
        info!("starting k8 cluster operators (kube-rs)");

        super::spg_statefulset_v2::start(
            kube_client.clone(),
            namespace.clone(),
            tls,
        );

        super::spu_controller_v2::start(
            kube_client.clone(),
            namespace.clone(),
        );

        super::spu_service_v2::start(
            kube_client,
            namespace,
        );
    }
}
