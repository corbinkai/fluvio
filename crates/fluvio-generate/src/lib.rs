//! Lightweight template expansion for Fluvio project scaffolding.
//!
//! Replaces `cargo-generate` without pulling in git2/libgit2/openssl.
//! Expands Liquid `{{ }}` template variables in all files, renames `.liquid` files.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, Context};
use include_dir::Dir;
use tracing::{debug, info};

/// Configuration for template expansion.
pub struct GenerateConfig {
    /// Project name (used as `project-name` and `crate_name` variables)
    pub name: String,
    /// Output directory for the generated project
    pub destination: Option<PathBuf>,
    /// Template variables as `key=value` strings (cargo-generate compatible format)
    pub define: Vec<String>,
    /// Run without interactive prompts
    pub silent: bool,
    /// Verbose output
    pub verbose: bool,
}

/// Source of template files.
pub enum TemplateSource<'a> {
    /// Embedded directory from `include_dir!` macro
    Embedded(&'a Dir<'a>),
    /// Local directory path
    LocalDir(PathBuf),
}

/// Generate a project from a template source.
///
/// Expands `{{ variable }}` Liquid tags in all files.
/// Renames files ending in `.liquid` (strips the extension).
/// Skips `cargo-generate.toml` config files.
///
/// Returns the path to the generated project directory.
pub fn generate(source: TemplateSource<'_>, config: GenerateConfig) -> Result<PathBuf> {
    let name = &config.name;

    // Parse template variables from key=value strings
    let mut variables = parse_variables(&config.define);

    // Add standard variables
    variables.insert("project-name".to_string(), name.clone());
    variables.insert("project_name".to_string(), name.replace('-', "_"));
    variables.insert("crate_name".to_string(), name.replace('-', "_"));

    // Build Liquid template globals
    let globals = build_liquid_globals(&variables)?;
    let parser = liquid::ParserBuilder::with_stdlib().build()?;

    // Determine output directory
    let output_dir = config
        .destination
        .unwrap_or_else(|| PathBuf::from("."))
        .join(name);

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("Failed to create output directory: {}", output_dir.display()))?;

    info!(name, output = %output_dir.display(), "Generating project");

    // Extract or read template files and expand
    match source {
        TemplateSource::Embedded(dir) => {
            expand_embedded_dir(dir, &output_dir, &parser, &globals, Path::new(""))?;
        }
        TemplateSource::LocalDir(path) => {
            expand_local_dir(&path, &output_dir, &parser, &globals, Path::new(""))?;
        }
    }

    Ok(output_dir)
}

/// Parse `key=value` strings into a HashMap.
fn parse_variables(define: &[String]) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for item in define {
        if let Some((key, value)) = item.split_once('=') {
            vars.insert(key.to_string(), value.to_string());
            // Also insert underscore version for Liquid compatibility
            let underscore_key = key.replace('-', "_");
            if underscore_key != key {
                vars.insert(underscore_key, value.to_string());
            }
        }
    }
    vars
}

/// Build Liquid template globals from variables.
fn build_liquid_globals(
    variables: &HashMap<String, String>,
) -> Result<liquid::Object> {
    let mut globals = liquid::Object::new();
    for (key, value) in variables {
        // Liquid variables can't have hyphens, so use underscore version
        let liquid_key = key.replace('-', "_");
        globals.insert(
            liquid_key.into(),
            liquid_core::Value::scalar(value.clone()),
        );
        // Also keep the original key if it's different
        if *key != key.replace('-', "_") {
            globals.insert(
                key.clone().into(),
                liquid_core::Value::scalar(value.clone()),
            );
        }
    }
    Ok(globals)
}

/// Recursively expand an embedded directory.
fn expand_embedded_dir(
    dir: &Dir<'_>,
    output_dir: &Path,
    parser: &liquid::Parser,
    globals: &liquid::Object,
    relative: &Path,
) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_relative = relative.join(subdir.path().file_name().unwrap());
                let subdir_output = output_dir.join(&subdir_relative);
                fs::create_dir_all(&subdir_output)?;
                expand_embedded_dir(subdir, &subdir_output, parser, globals, &subdir_relative)?;
            }
            include_dir::DirEntry::File(file) => {
                let file_name = file
                    .path()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap_or_default();

                // Skip cargo-generate config files
                if file_name == "cargo-generate.toml" {
                    continue;
                }

                let contents = file.contents_utf8().unwrap_or_default();
                expand_and_write(contents, file_name, output_dir, parser, globals)?;
            }
        }
    }
    Ok(())
}

/// Recursively expand a local directory.
fn expand_local_dir(
    src_dir: &Path,
    output_dir: &Path,
    parser: &liquid::Parser,
    globals: &liquid::Object,
    relative: &Path,
) -> Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_str().unwrap_or_default();

        if path.is_dir() {
            let subdir_output = output_dir.join(file_name);
            fs::create_dir_all(&subdir_output)?;
            expand_local_dir(&path, &subdir_output, parser, globals, &relative.join(file_name))?;
        } else {
            if file_name == "cargo-generate.toml" {
                continue;
            }
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read template file: {}", path.display()))?;
            expand_and_write(&contents, file_name, output_dir, parser, globals)?;
        }
    }
    Ok(())
}

/// Expand Liquid templates in content and write to output.
fn expand_and_write(
    contents: &str,
    file_name: &str,
    output_dir: &Path,
    parser: &liquid::Parser,
    globals: &liquid::Object,
) -> Result<()> {
    // Determine output filename (strip .liquid extension)
    let output_name = if file_name.ends_with(".liquid") {
        &file_name[..file_name.len() - 7]
    } else {
        file_name
    };

    // Expand template variables
    let expanded = if contents.contains("{{") || contents.contains("{%") {
        match parser.parse(contents) {
            Ok(template) => template.render(globals)
                .unwrap_or_else(|err| {
                    debug!(%err, file_name, "Liquid render error, using raw content");
                    contents.to_string()
                }),
            Err(err) => {
                debug!(%err, file_name, "Liquid parse error, using raw content");
                contents.to_string()
            }
        }
    } else {
        contents.to_string()
    };

    let output_path = output_dir.join(output_name);
    fs::write(&output_path, expanded)
        .with_context(|| format!("Failed to write: {}", output_path.display()))?;

    debug!(file = output_name, "Generated");
    Ok(())
}

#[cfg(feature = "git")]
pub mod git {
    use std::path::Path;
    use anyhow::Result;

    /// Clone a git repository to a local directory.
    pub fn clone_repo(url: &str, branch: Option<&str>, target: &Path) -> Result<()> {
        use gix::clone::PrepareFetch;
        use gix::progress::Discard;

        let mut prepare = PrepareFetch::new(
            url,
            target,
            gix::create::Kind::WithWorktree,
            gix::create::Options::default(),
            gix::open::Options::isolated(),
        )?;

        if let Some(branch) = branch {
            prepare = prepare.with_ref_name(Some(branch))?;
        }

        let (mut checkout, _) = prepare
            .fetch_then_checkout(Discard, &gix::interrupt::IS_INTERRUPTED)?;

        checkout.main_worktree(Discard, &gix::interrupt::IS_INTERRUPTED)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_variables() {
        let define = vec![
            "project-name=my-project".to_string(),
            "connector-type=source".to_string(),
        ];
        let vars = parse_variables(&define);
        assert_eq!(vars.get("project-name"), Some(&"my-project".to_string()));
        assert_eq!(vars.get("project_name"), Some(&"my-project".to_string()));
        assert_eq!(vars.get("connector-type"), Some(&"source".to_string()));
        assert_eq!(vars.get("connector_type"), Some(&"source".to_string()));
    }

    #[test]
    fn test_expand_and_write_plain() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let globals = liquid::Object::new();

        expand_and_write("hello world", "test.txt", dir.path(), &parser, &globals).unwrap();

        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_expand_and_write_liquid() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let mut globals = liquid::Object::new();
        globals.insert("name".into(), liquid_core::Value::scalar("fluvio"));

        expand_and_write("hello {{ name }}", "test.txt", dir.path(), &parser, &globals).unwrap();

        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello fluvio");
    }

    #[test]
    fn test_expand_and_write_strips_liquid_extension() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let globals = liquid::Object::new();

        expand_and_write("[package]", "Cargo.toml.liquid", dir.path(), &parser, &globals).unwrap();

        assert!(dir.path().join("Cargo.toml").exists());
        assert!(!dir.path().join("Cargo.toml.liquid").exists());
    }

    #[test]
    fn test_build_liquid_globals() {
        let mut vars = HashMap::new();
        vars.insert("project-name".to_string(), "my-project".to_string());
        vars.insert("crate_name".to_string(), "my_project".to_string());

        let globals = build_liquid_globals(&vars).unwrap();
        assert!(globals.contains_key("project_name"));
        assert!(globals.contains_key("crate_name"));
    }
}
