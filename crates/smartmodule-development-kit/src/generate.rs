use std::{path::PathBuf, str::FromStr};

use anyhow::{Result, anyhow};
use clap::{Parser, ValueEnum};
use include_dir::{Dir, include_dir};
use enum_display::EnumDisplay;
use tracing::debug;

use fluvio_generate::{GenerateConfig, TemplateSource, generate};
use lib_cargo_crate::{Info, InfoOpts};

// Note: Cargo.toml.liquid files are changed by cargo-generate to Cargo.toml
// this avoids the problem of cargo trying to parse Cargo.toml template files
// and generating a lot of parsing errors

static SMART_MODULE_TEMPLATE: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../smartmodule/cargo_template");
const FLUVIO_SMARTMODULE_CRATE_NAME: &str = "fluvio-smartmodule";
const FLUVIO_SMARTMODULE_REPO: &str = "https://github.com/fluvio-community/fluvio.git";

/// Generate new SmartModule project
#[derive(Debug, Parser)]
pub struct GenerateCmd {
    /// SmartModule Project Name
    name: Option<String>,

    /// SmartModule Project Group Name.
    /// Default to Hub ID, if set. Overrides Hub ID if provided.
    #[arg(long, env = "SMDK_PROJECT_GROUP", value_name = "GROUP")]
    project_group: Option<String>,

    /// Local path to generate the SmartModule project.
    /// Default to directory with project name, created in current directory
    #[arg(long, env = "SMDK_DESTINATION", value_name = "PATH")]
    destination: Option<PathBuf>,

    /// Disable interactive prompt. Take all values from CLI flags. Fail if a value is missing.
    #[arg(long, hide_short_help = true)]
    silent: bool,

    /// URL to git repo containing templates for generating SmartModule projects.
    /// Using this option is discouraged. The default value is recommended.
    #[arg(
        long,
        hide_short_help = true,
        group("TemplateSourceGit"),
        conflicts_with = "TemplateSourcePath",
        value_name = "GIT_URL",
        env = "SMDK_TEMPLATE_REPO"
    )]
    template_repo: Option<String>,

    /// An optional git branch to use with `--template-repo`
    #[arg(
        long,
        hide_short_help = true,
        group("TemplateGit"),
        requires = "TemplateSourceGit",
        value_name = "BRANCH",
        env = "SMDK_TEMPLATE_REPO_BRANCH"
    )]
    template_repo_branch: Option<String>,

    /// An optional git tag to use with `--template-repo`
    #[arg(
        long,
        hide_short_help = true,
        group("TemplateGit"),
        requires = "TemplateSourceGit",
        value_name = "TAG",
        env = "SMDK_TEMPLATE_REPO_TAG"
    )]
    template_repo_tag: Option<String>,

    /// Local filepath containing templates for generating SmartModule projects.
    /// Using this option is discouraged. The default value is recommended.
    #[arg(
        long,
        hide_short_help = true,
        group("TemplateSourcePath"),
        conflicts_with = "TemplateSourceGit",
        value_name = "PATH",
        env = "SMDK_TEMPLATE_PATH"
    )]
    template_path: Option<String>,

    /// URL of git repo to include in generated Cargo.toml. Repo used for `fluvio-smartmodule` dependency.
    /// Using this option is discouraged. The default value is recommended.
    #[arg(
        long,
        hide_short_help = true,
        group("SmCrateSourceGit"),
        conflicts_with_all = &["SmCrateSourcePath", "SmCrateSourceCratesIo"],
        value_name = "GIT_URL",
        env = "SMDK_SM_CRATE_REPO",

    )]
    sm_crate_repo: Option<String>,

    /// An optional git branch to use with `--sm-crate-repo`
    #[arg(
        long,
        hide_short_help = true,
        group("SmGit"),
        requires = "SmCrateSourceGit",
        value_name = "BRANCH",
        env = "SMDK_SM_REPO_BRANCH"
    )]
    sm_repo_branch: Option<String>,

    /// An optional git tag to use with `--sm-crate-repo`
    #[arg(
        long,
        hide_short_help = true,
        group("SmGit"),
        requires = "SmCrateSourceGit",
        value_name = "TAG",
        env = "SMDK_SM_REPO_TAG"
    )]
    sm_repo_tag: Option<String>,

    /// An optional git rev to use with `--sm-crate-repo`
    #[arg(
        long,
        hide_short_help = true,
        group("SmGit"),
        requires = "SmCrateSourceGit",
        value_name = "GIT_SHA",
        env = "SMDK_SM_REPO_REV"
    )]
    sm_repo_rev: Option<String>,

    /// Local filepath to include in generated Cargo.toml. Path used for fluvio-smartmodule dependency.
    /// Using this option is discouraged. The default value is recommended.
    #[arg(
        long,
        hide_short_help = true,
        group("SmCrateSourcePath"),
        conflicts_with_all = &["SmCrateSourceGit", "SmCrateSourceCratesIo"],
        value_name = "PATH",
        env = "SMDK_SM_CRATE_PATH"
    )]
    sm_crate_path: Option<String>,

    /// Public version of `fluvio-smartmodule` from crates.io. Defaults to latest.
    /// Using this option is discouraged. The default value is recommended.
    #[arg(
        long,
        hide_short_help = true,
        group("SmCrateSourceCratesIo"),
        conflicts_with_all = &["SmCrateSourceGit", "SmCrateSourcePath"],
        value_name = "X.Y.Z",
        env = "SMDK_SM_CRATE_VERSION"
    )]
    sm_crate_version: Option<String>,

    /// Type of SmartModule project to generate.
    /// Skip prompt if value given.
    #[arg(long, value_enum, value_name = "TYPE", env = "SMDK_SM_TYPE")]
    sm_type: Option<SmartModuleType>,

    /// Visibility of SmartModule project to generate.
    /// Skip prompt if value given.
    #[arg(long, value_enum, value_name = "PUBLIC", env = "SMDK_SM_PUBLIC")]
    sm_public: Option<bool>,

    /// Include SmartModule input parameters in generated SmartModule project.
    /// Skip prompt if value given.
    #[arg(long, group("SmartModuleParams"), env = "SMDK_WITH_PARAMS")]
    with_params: bool,

    /// No SmartModule input parameters in generated SmartModule project.
    /// Skip prompt if value given.
    #[arg(long, group("SmartModuleParams"), env = "SMDK_NO_PARAMS")]
    no_params: bool,

    /// Set the remote URL for the hub
    #[arg(long, env = "SMDK_HUB_REMOTE", hide_short_help = true)]
    hub_remote: Option<String>,

    /// Using this option will always choose the Fluvio repo as source for templates and dependencies
    #[arg(long, env = "SMDK_DEVELOP", hide_short_help = true, conflicts_with_all =
        &["TemplateSourceGit", "TemplateSourcePath",
        "SmCrateSourceGit", "SmCrateSourceCratesIo", "SmCrateSourcePath"],)]
    develop: bool,
}

impl GenerateCmd {
    pub(crate) fn process(self) -> Result<()> {
        // If a name isn't specified, you'll get prompted in wizard
        if let Some(ref name) = self.name {
            println!("Generating new SmartModule project: {name}");
        }

        let group = self.project_group.and_then(|g| {
            debug!("Using user provided project group: \"{}\"", &g);

            if g.is_empty() { None } else { Some(g) }
        });

        let sm_params = match (self.with_params, self.no_params) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        };

        // fluvio-smartmodule source
        // Check: user version, user git, user path, develop, then default to latest crates.io version
        let sm_dep_source = if let Some(user_version) = self.sm_crate_version {
            CargoSmDependSource::CratesIo(user_version)
        } else if let Some(user_repo) = self.sm_crate_repo {
            if let Some(branch) = self.sm_repo_branch {
                CargoSmDependSource::GitBranch {
                    url: user_repo,
                    branch,
                }
            } else if let Some(tag) = self.sm_repo_tag {
                CargoSmDependSource::GitTag {
                    url: user_repo,
                    tag,
                }
            } else if let Some(rev) = self.sm_repo_rev {
                CargoSmDependSource::GitRev {
                    url: user_repo,
                    rev,
                }
            } else {
                CargoSmDependSource::Git(FLUVIO_SMARTMODULE_REPO.to_string())
            }
        } else if let Some(path) = self.sm_crate_path {
            CargoSmDependSource::Path(PathBuf::from_str(&path)?)
        } else if self.develop {
            CargoSmDependSource::Git(FLUVIO_SMARTMODULE_REPO.to_string())
        } else {
            let latest_sm_crate_info =
                Info::new().fetch(vec![FLUVIO_SMARTMODULE_CRATE_NAME], &InfoOpts::default())?;
            let version = &latest_sm_crate_info[0].krate.crate_data.max_version;

            CargoSmDependSource::CratesIo(version.to_string())
        };

        let mut maybe_user_input = SmdkTemplateUserValues::new();
        maybe_user_input
            .with_project_group(group.clone())
            .with_smart_module_type(self.sm_type)
            .with_smart_module_params(sm_params)
            .with_smart_module_cargo_dependency(Some(sm_dep_source))
            .with_smart_module_public(self.sm_public);

        let name = self.name.clone().unwrap_or_else(|| "my-smartmodule".to_string());

        // Template source: check user git, user path, develop, then default to built-in
        let source = if let Some(_git_url) = self.template_repo {
            // Git template support requires the "git" feature on fluvio-generate
            // For now, fall back to embedded templates
            debug!("Git template repos not yet supported, using embedded templates");
            TemplateSource::Embedded(&SMART_MODULE_TEMPLATE)
        } else if let Some(path) = self.template_path {
            TemplateSource::LocalDir(PathBuf::from(path))
        } else {
            TemplateSource::Embedded(&SMART_MODULE_TEMPLATE)
        };

        let config = GenerateConfig {
            name,
            destination: self.destination,
            define: maybe_user_input.to_cargo_generate(),
            silent: self.silent,
            verbose: !self.silent,
        };

        let output = generate(source, config)?;
        println!("Project generated at: {}", output.display());

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SmdkTemplateValue {
    UseParams(bool),
    SmCargoDependency(CargoSmDependSource),
    SmType(SmartModuleType),
    ProjectGroup(String),
    SmPublic(bool),
}

impl std::fmt::Display for SmdkTemplateValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmdkTemplateValue::SmCargoDependency(dependency) => {
                write!(f, "fluvio-smartmodule-cargo-dependency={dependency}")
            }
            SmdkTemplateValue::SmType(sm_type) => {
                write!(f, "smartmodule-type={sm_type}")
            }
            SmdkTemplateValue::UseParams(sm_params) => {
                write!(f, "smartmodule-params={sm_params}")
            }
            SmdkTemplateValue::ProjectGroup(group) => {
                write!(f, "project-group={group}")
            }
            SmdkTemplateValue::SmPublic(public) => {
                write!(f, "smartmodule-public={public}")
            }
        }
    }
}

#[derive(ValueEnum, Clone, Debug, Parser, PartialEq, Eq, EnumDisplay)]
#[clap(rename_all = "kebab-case")]
#[enum_display(case = "Kebab")]
enum SmartModuleType {
    Filter,
    Map,
    ArrayMap,
    Aggregate,
    FilterMap,
}


#[derive(Debug, Default, Clone)]
struct SmdkTemplateUserValues {
    values: Vec<SmdkTemplateValue>,
}

impl SmdkTemplateUserValues {
    fn new() -> Self {
        SmdkTemplateUserValues::default()
    }

    fn with_smart_module_cargo_dependency(
        &mut self,
        dependency: Option<CargoSmDependSource>,
    ) -> &mut Self {
        if let Some(d) = dependency {
            debug!("User provided fluvio-smartmodule Cargo.toml value: {d:#?}");
            self.values.push(SmdkTemplateValue::SmCargoDependency(d));
        }
        self
    }

    fn with_smart_module_type(&mut self, sm_type: Option<SmartModuleType>) -> &mut Self {
        if let Some(t) = sm_type {
            debug!("User provided SmartModule type: {t:#?}");
            self.values.push(SmdkTemplateValue::SmType(t));
        }
        self
    }

    fn with_smart_module_params(&mut self, request: Option<bool>) -> &mut Self {
        if let Some(i) = request {
            debug!("User provided SmartModule params request: {i:#?}");
            self.values.push(SmdkTemplateValue::UseParams(i));
        }
        self
    }

    fn with_project_group(&mut self, group: Option<String>) -> &mut Self {
        if let Some(i) = group {
            debug!("User default project group: {i:#?}");
            self.values.push(SmdkTemplateValue::ProjectGroup(i));
        }
        self
    }

    fn with_smart_module_public(&mut self, public: Option<bool>) -> &mut Self {
        if let Some(p) = public {
            debug!("User project public: {p:#?}");
            self.values.push(SmdkTemplateValue::SmPublic(p));
        }
        self
    }

    fn to_vec(&self) -> Vec<SmdkTemplateValue> {
        self.values.clone()
    }

    fn to_cargo_generate(&self) -> Vec<String> {
        self.to_vec().iter().map(|v| v.to_string()).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
enum CargoSmDependSource {
    CratesIo(String),
    Git(String),
    GitBranch { url: String, branch: String },
    GitTag { url: String, tag: String },
    GitRev { url: String, rev: String },
    Path(PathBuf),
}

impl std::fmt::Display for CargoSmDependSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CargoSmDependSource::CratesIo(version) => {
                write!(f, "\"{version}\"")
            }
            CargoSmDependSource::Git(url) => write!(f, "{{ git = \"{url}\" }}"),
            CargoSmDependSource::GitBranch { url, branch } => {
                write!(f, "{{ git = \"{url}\", branch = \"{branch}\" }}")
            }
            CargoSmDependSource::GitTag { url, tag } => {
                write!(f, "{{ git = \"{url}\", tag = \"{tag}\" }}")
            }
            CargoSmDependSource::GitRev { url, rev } => {
                write!(f, "{{ git = \"{url}\", rev = \"{rev}\" }}")
            }
            CargoSmDependSource::Path(path) => {
                write!(f, "{{ path = \"{}\" }}", path.as_path().display())
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::SmdkTemplateUserValues;
    use super::SmartModuleType;
    use super::SmdkTemplateValue;
    use super::CargoSmDependSource;
    use super::FLUVIO_SMARTMODULE_REPO;

    #[test]
    fn test_cargo_dependency_values() {
        let test_semver = "4.5.6".to_string();
        let test_template_values = vec![
            CargoSmDependSource::CratesIo(test_semver),
            CargoSmDependSource::Git(FLUVIO_SMARTMODULE_REPO.to_string()),
            CargoSmDependSource::GitBranch {
                url: FLUVIO_SMARTMODULE_REPO.to_string(),
                branch: "my-branch".to_string(),
            },
            CargoSmDependSource::GitTag {
                url: FLUVIO_SMARTMODULE_REPO.to_string(),
                tag: "my-tag".to_string(),
            },
            CargoSmDependSource::GitRev {
                url: FLUVIO_SMARTMODULE_REPO.to_string(),
                rev: "abcdef01189998119991197253".to_string(),
            },
        ];

        for value in test_template_values {
            match &value {
                CargoSmDependSource::CratesIo(version) => {
                    assert_eq!(value.to_string(), format!("\"{version}\""))
                }
                CargoSmDependSource::Git(url) => {
                    assert_eq!(value.to_string(), format!("{{ git = \"{url}\" }}"))
                }
                CargoSmDependSource::GitBranch { url, branch } => {
                    assert_eq!(
                        value.to_string(),
                        format!("{{ git = \"{url}\", branch = \"{branch}\" }}")
                    )
                }
                CargoSmDependSource::GitTag { url, tag } => {
                    assert_eq!(
                        value.to_string(),
                        format!("{{ git = \"{url}\", tag = \"{tag}\" }}")
                    )
                }
                CargoSmDependSource::GitRev { url, rev } => {
                    assert_eq!(
                        value.to_string(),
                        format!("{{ git = \"{url}\", rev = \"{rev}\" }}")
                    )
                }
                CargoSmDependSource::Path(path) => {
                    assert_eq!(
                        value.to_string(),
                        format!("{{ path = \"{}\" }}", path.as_path().display())
                    )
                }
            }
        }
    }

    #[test]
    fn test_generate_user_values() {
        let test_template_values = vec![
            SmdkTemplateValue::UseParams(true),
            SmdkTemplateValue::SmCargoDependency(CargoSmDependSource::CratesIo(
                "0.1.0".to_string(),
            )),
            SmdkTemplateValue::SmType(SmartModuleType::FilterMap),
            SmdkTemplateValue::ProjectGroup("ExampleGroupName".to_string()),
        ];

        for value in test_template_values {
            match value {
                SmdkTemplateValue::UseParams(_) => {
                    assert_eq!(
                        &SmdkTemplateValue::UseParams(true).to_string(),
                        "smartmodule-params=true"
                    );
                }

                SmdkTemplateValue::SmCargoDependency(_) => {
                    assert_eq!(
                        &SmdkTemplateValue::SmCargoDependency(CargoSmDependSource::CratesIo(
                            "0.1.0".to_string()
                        ))
                        .to_string(),
                        "fluvio-smartmodule-cargo-dependency=\"0.1.0\""
                    );
                }

                SmdkTemplateValue::SmType(_) => {
                    assert_eq!(
                        &SmdkTemplateValue::SmType(SmartModuleType::FilterMap).to_string(),
                        "smartmodule-type=filter-map"
                    );
                }

                SmdkTemplateValue::ProjectGroup(_) => {
                    assert_eq!(
                        &SmdkTemplateValue::ProjectGroup("ExampleGroupName".to_string())
                            .to_string(),
                        "project-group=ExampleGroupName"
                    );
                }
                SmdkTemplateValue::SmPublic(_) => {
                    assert_eq!(
                        &SmdkTemplateValue::SmPublic(true).to_string(),
                        "smartmodule-public=true"
                    );
                    assert_eq!(
                        &SmdkTemplateValue::SmPublic(false).to_string(),
                        "smartmodule-public=false"
                    );
                }
            }
        }
    }

    #[test]
    fn test_template_builder() {
        let mut values = SmdkTemplateUserValues::new();
        let test_version_number = "test-version-value".to_string();
        values
            .with_project_group(Some("ExampleGroupName".to_string()))
            .with_smart_module_type(Some(SmartModuleType::Aggregate))
            .with_smart_module_params(Some(true))
            .with_smart_module_cargo_dependency(Some(CargoSmDependSource::CratesIo(
                test_version_number.clone(),
            )))
            .with_smart_module_public(Some(false));

        let values_vec = values.to_vec();

        for v in values_vec {
            match v {
                SmdkTemplateValue::UseParams(_) => {
                    assert_eq!(v, SmdkTemplateValue::UseParams(true));
                }

                SmdkTemplateValue::SmCargoDependency(_) => {
                    assert_eq!(
                        v,
                        SmdkTemplateValue::SmCargoDependency(CargoSmDependSource::CratesIo(
                            test_version_number.clone()
                        ))
                    );
                }

                SmdkTemplateValue::SmType(_) => {
                    assert_eq!(v, SmdkTemplateValue::SmType(SmartModuleType::Aggregate));
                }

                SmdkTemplateValue::ProjectGroup(_) => {
                    assert_eq!(
                        v,
                        SmdkTemplateValue::ProjectGroup("ExampleGroupName".to_string())
                    );
                }
                SmdkTemplateValue::SmPublic(_) => {
                    assert_eq!(v, SmdkTemplateValue::SmPublic(false));
                }
            }
        }
    }
}
