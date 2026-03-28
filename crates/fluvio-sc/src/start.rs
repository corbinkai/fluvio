use std::{
    sync::Arc,
    path::{PathBuf, Path},
    time::Duration,
};

use anyhow::Result;
use tracing::info;

use fluvio_future::{task::run_block_on, timer::sleep};
use fluvio_stream_dispatcher::metadata::{SharedClient, MetadataClient, local::LocalMetadataStorage};
use fluvio_stream_dispatcher::metadata::kube_rs::KubeClient;
use fluvio_stream_dispatcher::metadata::memory_client::InMemoryClient;
use fluvio_stream_model::{store::k8::K8MetaItem, core::MetadataItem};

use crate::{
    cli::{ScOpt, TlsConfig, RunMode},
    services::auth::basic::BasicRbacPolicy,
    config::ScConfig,
    config::DEFAULT_NAMESPACE,
};

pub fn main_loop(opt: ScOpt) {
    // parse configuration (program exits on error)
    println!("CLI Option: {opt:#?}");

    inspect_system();
    crate::otel::init_otel();
    println!("Starting SC, platform: {}", crate::VERSION);

    match opt.mode() {
        RunMode::Local(metadata) => {
            info!(?metadata, "Running in local mode");
            let client = create_local_metadata_store(metadata);
            let ((sc_config, auth_policy), tls_option) = opt.parse_cli_or_exit();
            local_main_loop(sc_config, client, auth_policy, tls_option)
        }
        RunMode::ReadOnly(read_only_path) => {
            let read_only_path = read_only_path.to_path_buf();
            info!("Running in read only mode");
            let ((sc_config, auth_policy), tls_option) = opt.parse_cli_or_exit();

            info!("initializing metadata from read only configuration");
            let client = fluvio_future::task::run_block_on(async move {
                create_memory_client(read_only_path).await
            })
            .expect("failed to initialize metadata from read only configuration");
            local_main_loop(sc_config, client, auth_policy, tls_option)
        }
        RunMode::K8s => {
            info!("Running with K8 (kube-rs)");

            let ((mut sc_config, auth_policy), tls_option) = opt.parse_cli_or_exit();

            // Install rustls crypto provider (required by rustls 0.23+)
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("failed to install rustls crypto provider");

            // Create kube client and run main loop inside the same runtime.
            // kube::Client must be created in the runtime it will be used in,
            // because its HTTP connection pool is tied to the runtime.
            run_block_on(async move {
                let kube_client = kube::Client::try_default()
                    .await
                    .expect("failed to create kube-rs client");

                let k8_namespace = kube_client.default_namespace().to_owned();
                if sc_config.namespace == DEFAULT_NAMESPACE {
                    info!("using {} as namespace from kubernetes config", k8_namespace);
                    sc_config.namespace = k8_namespace;
                }

                let client: SharedClient<KubeClient> = Arc::new(KubeClient::new(kube_client.clone()));

                info!("starting k8 main loop");
                let ctx = crate::init::start_main_loop(
                    (sc_config.clone(), auth_policy),
                    client.clone(),
                )
                .await;

                crate::k8::controllers::run_k8_operators(
                    sc_config.namespace.clone(),
                    kube_client,
                    ctx,
                    tls_option.clone().map(|(_, config)| config),
                )
                .await;

                proxy::start_if(sc_config, tls_option).await;

                println!("Streaming Controller started successfully");
                loop {
                    sleep(Duration::from_secs(60)).await;
                }
            });
        }
    }
}

/// print out system information
fn inspect_system() {
    use sysinfo::System;

    sysinfo::set_open_files_limit(0);
    let mut sys = System::new_all();
    sys.refresh_all();
    info!(version = crate::VERSION, "Platform");
    info!(commit = env!("GIT_HASH"), "Git");
    info!(name = ?System::name(),"System");
    info!(kernel = ?System::kernel_version(),"System");
    info!(os_version = ?System::long_os_version(),"System");
    info!(core_count = ?sys.physical_core_count(),"System");
    info!(total_memory = sys.total_memory(), "System");
    info!(available_memory = sys.available_memory(), "System");
    info!(uptime = System::uptime(), "Uptime in secs");
}

fn local_main_loop<C, M>(
    sc_config: ScConfig,
    client: SharedClient<C>,
    auth_policy: Option<BasicRbacPolicy>,
    tls_option: Option<(String, TlsConfig)>,
) where
    C: MetadataClient<M> + 'static,
    M: MetadataItem,
    M::UId: Send + Sync,
{
    run_block_on(async move {
        info!("starting local main loop");

        crate::init::start_main_loop((sc_config.clone(), auth_policy), client).await;
        proxy::start_if(sc_config, tls_option).await;

        println!("Streaming Controller started successfully");
        // do infinite loop
        loop {
            sleep(Duration::from_secs(60)).await;
        }
    });
}

mod proxy {
    use std::process;
    use tracing::info;

    use fluvio_types::print_cli_err;
    pub use fluvio_future::rust_tls::TlsAcceptor;

    use fluvio_auth::x509::X509Authenticator;
    use flv_tls_proxy::{
        start as proxy_start, start_with_authenticator as proxy_start_with_authenticator,
    };

    use crate::{config::ScConfig, cli::TlsConfig};

    pub async fn start_if(sc_config: ScConfig, tls_option: Option<(String, TlsConfig)>) {
        if let Some((proxy_port, tls_config)) = tls_option {
            let tls_acceptor = tls_config
                .try_build_tls_acceptor()
                .expect("can't build tls acceptor");
            start_proxy(sc_config, (tls_acceptor, proxy_port)).await;
        }
    }

    async fn start_proxy(config: ScConfig, acceptor: (TlsAcceptor, String)) {
        let (tls_acceptor, proxy_addr) = acceptor;
        let target = config.public_endpoint;
        info!("starting TLS proxy: {}", proxy_addr);

        let result = if let Some(x509_auth_scopes) = config.x509_auth_scopes {
            let authenticator = Box::new(X509Authenticator::new(&x509_auth_scopes));
            proxy_start_with_authenticator(&proxy_addr, tls_acceptor, target, authenticator).await
        } else {
            proxy_start(&proxy_addr, tls_acceptor, target).await
        };

        if let Err(err) = result {
            print_cli_err!(err);
            process::exit(-1);
        }
    }
}

async fn create_memory_client(path: PathBuf) -> Result<Arc<InMemoryClient>> {
    use std::ops::Deref;
    use fluvio_sc_schema::remote_file::RemoteMetadataFile;
    use crate::stores::topic::TopicSpec;

    let metadata_file = RemoteMetadataFile::open(path)?;
    let config = metadata_file.deref();
    let client = InMemoryClient::new();

    info!(topics = config.topics.len(), "loading topics");
    for value in &config.topics {
        info!(name = value.metadata.name, "read topic");
        client.load_k8_obj::<TopicSpec>(value.clone()).await?;
    }

    Ok(Arc::new(client))
}

fn create_local_metadata_store(path: &Path) -> Arc<LocalMetadataStorage> {
    Arc::new(LocalMetadataStorage::new(path))
}
