use std::fmt::Debug;
use std::fmt::Display;
use std::path::PathBuf;

use anyhow::Result;

use clap::{Parser, ValueEnum};
use include_dir::{Dir, include_dir};
use enum_display::EnumDisplay;

use fluvio_generate::{GenerateConfig, TemplateSource, generate};

static CONNECTOR_TEMPLATE: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../connector/cargo_template");

/// Generate new Connector project
#[derive(Debug, Parser)]
pub struct GenerateCmd {
    /// Connector Name
    name: Option<String>,

    #[arg(long, value_name = "GROUP")]
    /// Connector developer group
    group: Option<String>,

    /// Connector description used as part of the project metadata
    #[arg(long, value_name = "DESCRIPTION")]
    conn_description: Option<String>,

    /// Local path to generate the Connector project.
    /// Default to directory with project name, created in current directory
    #[arg(long, env = "CDK_DESTINATION", value_name = "PATH")]
    destination: Option<PathBuf>,

    /// Disable interactive prompt. Take all values from CLI flags. Fail if a value is missing.
    #[arg(long, hide_short_help = true)]
    silent: bool,

    /// Type of Connector project to generate.
    /// Skip prompt if value given.
    #[arg(long, value_enum, value_name = "TYPE", env = "CDK_CONN_TYPE")]
    conn_type: Option<ConnectorType>,

    /// Visibility of Connector project to generate.
    /// Skip prompt if value given.
    #[arg(long, value_enum, value_name = "PUBLIC", env = "CDK_CONN_PUBLIC")]
    conn_public: Option<bool>,
}

impl GenerateCmd {
    pub(crate) fn process(self) -> Result<()> {
        let name = self.name.clone().unwrap_or_else(|| "my-connector".to_string());
        println!("Generating new Connector project: {name}");

        let mut define = vec![
            format!("fluvio-cargo-dependency-hash={}", env!("GIT_HASH")),
        ];

        if let Some(ref group) = self.group {
            define.push(format!("project-group={group}"));
        }
        if let Some(ref desc) = self.conn_description {
            define.push(format!("project-description={desc}"));
        }
        if let Some(ref ct) = self.conn_type {
            define.push(format!("connector-type={ct}"));
        }
        if let Some(cp) = self.conn_public {
            define.push(format!("connector-public={cp}"));
        }

        let config = GenerateConfig {
            name,
            destination: self.destination,
            define,
            silent: self.silent,
            verbose: !self.silent,
        };

        let output = generate(TemplateSource::Embedded(&CONNECTOR_TEMPLATE), config)?;
        println!("Project generated at: {}", output.display());

        Ok(())
    }
}

#[derive(ValueEnum, Clone, Debug, Parser, PartialEq, Eq, EnumDisplay)]
#[clap(rename_all = "kebab-case")]
#[enum_display(case = "Kebab")]
enum ConnectorType {
    Sink,
    Source,
}
