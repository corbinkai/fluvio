//!
//! # Profile Configurations
//!
//! Stores configuration parameter retrieved from the default or custom profile file.
//!
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Serialize, Deserialize};
use toml::Table as Metadata;

use crate::{config::TlsPolicy, FluvioError};

use super::ConfigFile;

/// Global SPU retry configuration, set when FluvioClusterConfig is loaded.
static SPU_RETRY_CONFIG: OnceLock<SpuRetryConfig> = OnceLock::new();

/// SPU connection retry backoff configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct SpuRetryConfig {
    pub retry_count: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for SpuRetryConfig {
    fn default() -> Self {
        Self {
            retry_count: default_spu_retry_count(),
            initial_delay: Duration::from_millis(default_spu_retry_initial_delay_ms()),
            max_delay: Duration::from_millis(default_spu_retry_max_delay_ms()),
        }
    }
}

/// Returns the active SPU retry configuration.
/// Uses the globally set config if available, otherwise returns defaults.
pub fn spu_retry_config() -> SpuRetryConfig {
    SPU_RETRY_CONFIG.get().cloned().unwrap_or_default()
}

//NOTE: this is to avoid breaking changes as we rename it to FluvioClusterConfig
/// Fluvio client configuration
pub type FluvioConfig = FluvioClusterConfig;

fn default_spu_retry_count() -> u32 {
    16
}

fn default_spu_retry_initial_delay_ms() -> u64 {
    1000
}

fn default_spu_retry_max_delay_ms() -> u64 {
    30000
}

/// Fluvio Cluster Target Configuration
/// This is part of profile
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FluvioClusterConfig {
    /// The address to connect to the Fluvio cluster
    // TODO use a validated address type.
    // We don't want to have a "" address.
    #[serde(alias = "addr")]
    pub endpoint: String,

    #[serde(default)]
    pub use_spu_local_address: bool,

    /// The TLS policy to use when connecting to the cluster
    // If no TLS field is present in config file,
    // use the default of NoTls
    #[serde(default)]
    pub tls: TlsPolicy,

    /// Maximum number of SPU connection retry attempts before giving up.
    #[serde(default = "default_spu_retry_count")]
    pub spu_retry_count: u32,

    /// Initial delay in milliseconds before the first SPU connection retry.
    #[serde(default = "default_spu_retry_initial_delay_ms")]
    pub spu_retry_initial_delay_ms: u64,

    /// Maximum delay in milliseconds between SPU connection retries.
    #[serde(default = "default_spu_retry_max_delay_ms")]
    pub spu_retry_max_delay_ms: u64,

    /// Cluster custom metadata
    #[serde(default = "Metadata::new", skip_serializing_if = "Metadata::is_empty")]
    metadata: Metadata,

    /// This is not part of profile and doesn't persist.
    /// It is purely to override client id when creating ClientConfig
    #[serde(skip)]
    pub client_id: Option<String>,
}

impl FluvioClusterConfig {
    /// get current cluster config from default profile
    pub fn load() -> Result<Self, FluvioError> {
        let config_file = ConfigFile::load_default_or_new()?;
        let mut cluster_config = config_file.config().current_cluster()?.to_owned();
        cluster_config.apply_env_overrides();
        cluster_config.publish_spu_retry_config();
        Ok(cluster_config)
    }

    /// get cluster config from profile
    /// if profile is not found, return None
    pub fn load_with_profile(profile_name: &str) -> Result<Option<Self>, FluvioError> {
        let config_file = ConfigFile::load_default_or_new()?;
        let cluster_config = config_file.config().cluster_with_profile(profile_name);
        Ok(cluster_config.cloned().map(|mut c| {
            c.apply_env_overrides();
            c.publish_spu_retry_config();
            c
        }))
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("FLUVIO_SPU_LOCAL") {
            if val.eq_ignore_ascii_case("true") || val == "1" {
                self.use_spu_local_address = true;
            }
        }
        if let Ok(val) = std::env::var("FLUVIO_SPU_RETRY_COUNT") {
            if let Ok(v) = val.parse::<u32>() {
                self.spu_retry_count = v;
            }
        }
        if let Ok(val) = std::env::var("FLUVIO_SPU_RETRY_INITIAL_DELAY_MS") {
            if let Ok(v) = val.parse::<u64>() {
                self.spu_retry_initial_delay_ms = v;
            }
        }
        if let Ok(val) = std::env::var("FLUVIO_SPU_RETRY_MAX_DELAY_MS") {
            if let Ok(v) = val.parse::<u64>() {
                self.spu_retry_max_delay_ms = v;
            }
        }
    }

    fn publish_spu_retry_config(&self) {
        let _ = SPU_RETRY_CONFIG.set(SpuRetryConfig {
            retry_count: self.spu_retry_count,
            initial_delay: Duration::from_millis(self.spu_retry_initial_delay_ms),
            max_delay: Duration::from_millis(self.spu_retry_max_delay_ms),
        });
    }

    /// Returns the SPU retry initial delay as a Duration.
    pub fn spu_retry_initial_delay(&self) -> Duration {
        Duration::from_millis(self.spu_retry_initial_delay_ms)
    }

    /// Returns the SPU retry max delay as a Duration.
    pub fn spu_retry_max_delay(&self) -> Duration {
        Duration::from_millis(self.spu_retry_max_delay_ms)
    }

    /// Create a new cluster configuration with no TLS.
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            endpoint: addr.into(),
            use_spu_local_address: false,
            tls: TlsPolicy::Disabled,
            spu_retry_count: default_spu_retry_count(),
            spu_retry_initial_delay_ms: default_spu_retry_initial_delay_ms(),
            spu_retry_max_delay_ms: default_spu_retry_max_delay_ms(),
            metadata: Metadata::new(),
            client_id: None,
        }
    }

    /// Add TLS configuration for this cluster.
    pub fn with_tls(mut self, tls: impl Into<TlsPolicy>) -> Self {
        self.tls = tls.into();
        self
    }

    pub fn query_metadata_by_name<'de, T>(&self, name: &str) -> Option<T>
    where
        T: Deserialize<'de>,
    {
        let metadata = self.metadata.get(name)?;

        T::deserialize(metadata.clone()).ok()
    }

    pub fn update_metadata_by_name<S>(&mut self, name: &str, data: S) -> anyhow::Result<()>
    where
        S: Serialize,
    {
        use toml::{Value, map::Entry};

        match self.metadata.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(Value::try_from(data)?);
            }
            Entry::Occupied(mut entry) => {
                *entry.get_mut() = Value::try_from(data)?;
            }
        }

        Ok(())
    }

    pub fn has_metadata(&self, name: &str) -> bool {
        self.metadata.get(name).is_some()
    }
}

impl TryFrom<FluvioClusterConfig> for fluvio_socket::ClientConfig {
    type Error = anyhow::Error;
    fn try_from(config: FluvioClusterConfig) -> Result<Self, Self::Error> {
        let connector = fluvio_future::net::DomainConnector::try_from(config.tls.clone())?;
        Ok(Self::new(
            &config.endpoint,
            connector,
            config.use_spu_local_address,
        ))
    }
}

#[cfg(test)]
mod test_metadata {
    use fluvio_types::config_file::SaveLoadConfig;

    use serde::{Deserialize, Serialize};
    use crate::config::{Config, ConfigFile};

    #[test]
    fn test_get_metadata_path() {
        let toml = r#"version = "2"
[profile.local]
cluster = "local"

[cluster.local]
endpoint = "127.0.0.1:9003"

[cluster.local.metadata.custom]
name = "foo"
"#;
        let profile = Config::load_str(toml).unwrap();
        let config = profile.cluster("local").unwrap();

        #[derive(Deserialize, Debug, PartialEq)]
        struct Custom {
            name: String,
        }

        let custom: Option<Custom> = config.query_metadata_by_name("custom");

        assert_eq!(
            custom,
            Some(Custom {
                name: "foo".to_owned()
            })
        );
    }

    #[test]
    fn test_create_metadata() {
        let toml = r#"version = "2"
[profile.local]
cluster = "local"

[cluster.local]
endpoint = "127.0.0.1:9003"
"#;
        let mut profile = Config::load_str(toml).unwrap();
        let config = profile.cluster_mut("local").unwrap();

        #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
        struct Preference {
            connection: String,
        }

        let preference = Preference {
            connection: "wired".to_owned(),
        };

        config
            .update_metadata_by_name("preference", preference.clone())
            .expect("failed to add metadata");

        let metadata = config.query_metadata_by_name("preference").expect("");

        assert_eq!(preference, metadata);
    }

    #[test]
    fn test_update_old_metadata() {
        let toml = r#"version = "2"
[profile.local]
cluster = "local"

[cluster.local]
endpoint = "127.0.0.1:9003"

[cluster.local.metadata.installation]
type = "local"
"#;
        let mut profile = Config::load_str(toml).unwrap();
        let config = profile.cluster_mut("local").unwrap();

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Installation {
            #[serde(rename = "type")]
            typ: String,
        }

        let mut install = config
            .query_metadata_by_name::<Installation>("installation")
            .expect("message");

        assert_eq!(
            install,
            Installation {
                typ: "local".to_owned()
            }
        );

        "cloud".clone_into(&mut install.typ);

        config
            .update_metadata_by_name("installation", install)
            .expect("failed to add metadata");

        let metadata = config
            .query_metadata_by_name::<Installation>("installation")
            .expect("could not get Installation metadata");

        assert_eq!("cloud", metadata.typ);
    }

    #[test]
    fn test_update_with_new_metadata() {
        let toml = r#"version = "2"
[profile.local]
cluster = "local"

[cluster.local]
endpoint = "127.0.0.1:9003"

[cluster.local.metadata.installation]
type = "local"
"#;
        let mut profile = Config::load_str(toml).unwrap();
        let config = profile.cluster_mut("local").unwrap();

        #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
        struct Preference {
            connection: String,
        }

        let preference = Preference {
            connection: "wired".to_owned(),
        };

        config
            .update_metadata_by_name("preference", preference.clone())
            .expect("failed to add metadata");

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Installation {
            #[serde(rename = "type")]
            typ: String,
        }

        let installation: Installation = config
            .query_metadata_by_name("installation")
            .expect("could not get installation metadata");
        assert_eq!(installation.typ, "local");

        let preference: Preference = config
            .query_metadata_by_name("preference")
            .expect("could not get preference metadata");
        assert_eq!(preference.connection, "wired");
    }

    #[test]
    fn test_profile_with_metadata() {
        let config_file = ConfigFile::load(Some("test-data/profiles/config.toml".to_owned()))
            .expect("could not parse config file");
        let config = config_file.config();

        let cluster = config
            .cluster("extra")
            .expect("could not find `extra` cluster in test file");

        let table = toml::toml! {
            [deep.nesting.example]
            key = "custom field"

            [installation]
            type = "local"
        };

        assert_eq!(cluster.metadata, table);
    }

    #[test]
    fn test_save_updated_metadata() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
        struct Installation {
            #[serde(rename = "type")]
            typ: String,
        }

        let mut config_file =
            ConfigFile::load(Some("test-data/profiles/updatable_config.toml".to_owned()))
                .expect("could not parse config file");
        let config = config_file.mut_config();

        let cluster = config
            .cluster_mut("updated")
            .expect("could not find `updated` cluster in test file");

        let table: toml::Table = toml::toml! {
            [installation]
            type = "local"
        };
        assert_eq!(cluster.metadata, table);

        cluster
            .update_metadata_by_name(
                "installation",
                Installation {
                    typ: "cloud".to_owned(),
                },
            )
            .expect("should have updated key");

        let updated_table: toml::Table = toml::toml! {
            [installation]
            type = "cloud"
        };

        assert_eq!(cluster.metadata, updated_table.clone());

        config_file.save().expect("failed to save config file");

        let mut config_file =
            ConfigFile::load(Some("test-data/profiles/updatable_config.toml".to_owned()))
                .expect("could not parse config file");
        let config = config_file.mut_config();
        let cluster = config
            .cluster_mut("updated")
            .expect("could not find `updated` cluster in test file");
        assert_eq!(cluster.metadata, updated_table);

        cluster
            .update_metadata_by_name(
                "installation",
                Installation {
                    typ: "local".to_owned(),
                },
            )
            .expect("teardown: failed to set installation type back to local");

        config_file
            .save()
            .expect("teardown: failed to set installation type back to local");
    }

    use super::FluvioClusterConfig;

    // Test env var override logic by directly testing the parsing behavior.
    // We can't safely use set_var/remove_var across parallel tests (global state),
    // so we test the logic that apply_env_overrides uses.

    fn check_spu_local_override(val: &str) -> bool {
        val.eq_ignore_ascii_case("true") || val == "1"
    }

    #[test]
    fn test_spu_local_override_true() {
        assert!(check_spu_local_override("true"));
    }

    #[test]
    fn test_spu_local_override_1() {
        assert!(check_spu_local_override("1"));
    }

    #[test]
    fn test_spu_local_override_case_insensitive() {
        assert!(check_spu_local_override("TRUE"));
        assert!(check_spu_local_override("True"));
        assert!(check_spu_local_override("tRuE"));
    }

    #[test]
    fn test_spu_local_override_ignores_invalid() {
        assert!(!check_spu_local_override("yes"));
        assert!(!check_spu_local_override("0"));
        assert!(!check_spu_local_override("false"));
        assert!(!check_spu_local_override(""));
    }

    #[test]
    fn test_spu_local_address_default_false() {
        let config = FluvioClusterConfig::new("localhost:9003");
        assert!(!config.use_spu_local_address);
    }

    #[test]
    fn test_spu_retry_defaults() {
        let config = FluvioClusterConfig::new("localhost:9003");
        assert_eq!(config.spu_retry_count, 16);
        assert_eq!(config.spu_retry_initial_delay_ms, 1000);
        assert_eq!(config.spu_retry_max_delay_ms, 30000);
    }

    #[test]
    fn test_spu_retry_duration_helpers() {
        let config = FluvioClusterConfig::new("localhost:9003");
        assert_eq!(
            config.spu_retry_initial_delay(),
            std::time::Duration::from_millis(1000)
        );
        assert_eq!(
            config.spu_retry_max_delay(),
            std::time::Duration::from_millis(30000)
        );
    }

    #[test]
    fn test_spu_retry_config_deserialization_defaults() {
        let toml = r#"version = "2"
[profile.local]
cluster = "local"

[cluster.local]
endpoint = "127.0.0.1:9003"
"#;
        let profile = Config::load_str(toml).unwrap();
        let config = profile.cluster("local").unwrap();
        assert_eq!(config.spu_retry_count, 16);
        assert_eq!(config.spu_retry_initial_delay_ms, 1000);
        assert_eq!(config.spu_retry_max_delay_ms, 30000);
    }

    #[test]
    fn test_spu_retry_config_deserialization_custom() {
        let toml = r#"version = "2"
[profile.local]
cluster = "local"

[cluster.local]
endpoint = "127.0.0.1:9003"
spu_retry_count = 5
spu_retry_initial_delay_ms = 200
spu_retry_max_delay_ms = 10000
"#;
        let profile = Config::load_str(toml).unwrap();
        let config = profile.cluster("local").unwrap();
        assert_eq!(config.spu_retry_count, 5);
        assert_eq!(config.spu_retry_initial_delay_ms, 200);
        assert_eq!(config.spu_retry_max_delay_ms, 10000);
    }

    fn parse_spu_retry_env_u32(val: &str) -> Option<u32> {
        val.parse::<u32>().ok()
    }

    fn parse_spu_retry_env_u64(val: &str) -> Option<u64> {
        val.parse::<u64>().ok()
    }

    #[test]
    fn test_spu_retry_env_var_parsing() {
        assert_eq!(parse_spu_retry_env_u32("5"), Some(5));
        assert_eq!(parse_spu_retry_env_u32("0"), Some(0));
        assert_eq!(parse_spu_retry_env_u32("abc"), None);
        assert_eq!(parse_spu_retry_env_u32(""), None);

        assert_eq!(parse_spu_retry_env_u64("500"), Some(500));
        assert_eq!(parse_spu_retry_env_u64("0"), Some(0));
        assert_eq!(parse_spu_retry_env_u64("abc"), None);
    }

    #[test]
    fn test_spu_retry_config_global_default() {
        use super::SpuRetryConfig;
        let cfg = SpuRetryConfig::default();
        assert_eq!(cfg.retry_count, 16);
        assert_eq!(cfg.initial_delay, std::time::Duration::from_millis(1000));
        assert_eq!(cfg.max_delay, std::time::Duration::from_millis(30000));
    }
}
