//! Lightweight template expansion for Fluvio project scaffolding.
//!
//! Replaces `cargo-generate` without pulling in git2/libgit2/openssl.
//! Expands Liquid `{{ }}` template variables in all files, renames `.liquid` files.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, Context};
use globset::{Glob, GlobSet, GlobSetBuilder};
use include_dir::Dir;
use liquid_core::{Display_filter, Filter, FilterReflection, ParseFilter, Runtime, Value, ValueView};
use regex::Regex;
use tracing::{debug, info};

// Custom Liquid filter: kebab_case (not in stdlib, added by cargo-generate)
#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "kebab_case",
    description = "Convert a string to kebab-case",
    parsed(KebabCaseFilter)
)]
pub struct KebabCaseFilterParser;

#[derive(Debug, Default, Display_filter)]
#[name = "kebab_case"]
pub struct KebabCaseFilter;

impl Filter for KebabCaseFilter {
    fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> liquid_core::Result<Value> {
        let s = input.to_kstr();
        // Convert PascalCase/camelCase/snake_case to kebab-case
        let mut result = String::with_capacity(s.len() + 4);
        for (i, ch) in s.chars().enumerate() {
            if ch.is_uppercase() && i > 0 {
                result.push('-');
                result.push(ch.to_lowercase().next().unwrap());
            } else if ch == '_' || ch == ' ' {
                result.push('-');
            } else {
                result.push(ch.to_lowercase().next().unwrap());
            }
        }
        Ok(Value::scalar(result))
    }
}

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
    // Only insert the hyphenated version — build_liquid_globals creates the underscore key
    // with the SAME value. crate_name is a separate variable with underscored value.
    variables.insert("project-name".to_string(), name.clone());
    variables.insert("crate_name".to_string(), name.replace('-', "_"));

    // Collect hyphenated variable names for preprocessing
    let hyphenated_vars: Vec<String> = variables
        .keys()
        .filter(|k| k.contains('-'))
        .cloned()
        .collect();

    // Build Liquid template globals (underscore versions only — Liquid can't parse hyphens)
    let globals = build_liquid_globals(&variables)?;
    let parser = liquid::ParserBuilder::with_stdlib()
        .filter(KebabCaseFilterParser)
        .build()?;

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
            let genignore = load_genignore_from_embedded(dir);
            expand_embedded_dir(dir, &output_dir, &parser, &globals, &hyphenated_vars, &genignore, Path::new(""))?;
        }
        TemplateSource::LocalDir(path) => {
            let genignore = load_genignore(&path);
            expand_local_dir(&path, &output_dir, &parser, &globals, &hyphenated_vars, &genignore, Path::new(""))?;
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
///
/// Only underscore versions are inserted — Liquid's parser interprets hyphens
/// as subtraction operators, so `{{ project-name }}` would parse as
/// `project` minus `name`. Templates are preprocessed to convert hyphenated
/// variable references to underscored ones before Liquid parsing.
fn build_liquid_globals(
    variables: &HashMap<String, String>,
) -> Result<liquid::Object> {
    let mut globals = liquid::Object::new();
    for (key, value) in variables {
        let liquid_key = key.replace('-', "_");
        globals.insert(
            liquid_key.into(),
            liquid_core::Value::scalar(value.clone()),
        );
    }
    Ok(globals)
}

/// Preprocess template content to convert hyphenated variable names to
/// underscored versions inside Liquid tags.
///
/// Liquid's parser interprets `{{ project-name }}` as `project` minus `name`.
/// This function finds all Liquid tags (`{{ ... }}` and `{% ... %}`) and
/// replaces hyphenated variable names with their underscored equivalents
/// before the content is passed to the Liquid parser.
fn preprocess_template(content: &str, hyphenated_vars: &[String]) -> String {
    if hyphenated_vars.is_empty() {
        return content.to_string();
    }

    // Match all Liquid tags: {{ ... }} and {% ... %} (including {%- -%} variants)
    let tag_re = Regex::new(r"(\{\{-?.*?-?\}\}|\{%-?.*?-?%\})").unwrap();
    tag_re
        .replace_all(content, |caps: &regex::Captures| {
            let mut tag = caps[0].to_string();
            for var in hyphenated_vars {
                let underscore_var = var.replace('-', "_");
                tag = tag.replace(var.as_str(), &underscore_var);
            }
            tag
        })
        .to_string()
}

/// Load .genignore patterns from a local directory.
fn load_genignore(dir: &Path) -> Option<GlobSet> {
    let ignore_path = dir.join(".genignore");
    let content = fs::read_to_string(&ignore_path).ok()?;
    build_globset(&content)
}

/// Load .genignore patterns from an embedded directory.
fn load_genignore_from_embedded(dir: &Dir<'_>) -> Option<GlobSet> {
    let file = dir.get_file(".genignore")?;
    let content = file.contents_utf8()?;
    build_globset(content)
}

/// Build a GlobSet from .genignore-style content (one pattern per line).
fn build_globset(content: &str) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut has_patterns = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(glob) = Glob::new(line) {
            builder.add(glob);
            has_patterns = true;
        }
    }
    if !has_patterns {
        return None;
    }
    builder.build().ok()
}

/// Check if a relative path should be ignored by .genignore.
fn is_ignored(genignore: &Option<GlobSet>, relative_path: &Path) -> bool {
    if let Some(ignore) = genignore {
        ignore.is_match(relative_path)
    } else {
        false
    }
}

/// Recursively expand an embedded directory.
fn expand_embedded_dir(
    dir: &Dir<'_>,
    output_dir: &Path,
    parser: &liquid::Parser,
    globals: &liquid::Object,
    hyphenated_vars: &[String],
    genignore: &Option<GlobSet>,
    relative: &Path,
) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let dir_name = subdir.path().file_name().unwrap();
                let subdir_relative = relative.join(dir_name);
                if is_ignored(genignore, &subdir_relative) {
                    debug!(path = %subdir_relative.display(), "Skipped (genignore)");
                    continue;
                }
                let subdir_output = output_dir.join(dir_name);
                fs::create_dir_all(&subdir_output)?;
                expand_embedded_dir(subdir, &subdir_output, parser, globals, hyphenated_vars, genignore, &subdir_relative)?;
            }
            include_dir::DirEntry::File(file) => {
                let file_name = file
                    .path()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap_or_default();

                // Skip cargo-generate config files and .genignore
                if file_name == "cargo-generate.toml" || file_name == ".genignore" {
                    continue;
                }

                let file_relative = relative.join(file_name);
                if is_ignored(genignore, &file_relative) {
                    debug!(path = %file_relative.display(), "Skipped (genignore)");
                    continue;
                }

                let contents = file.contents_utf8().unwrap_or_default();
                expand_and_write(contents, file_name, output_dir, parser, globals, hyphenated_vars)?;
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
    hyphenated_vars: &[String],
    genignore: &Option<GlobSet>,
    relative: &Path,
) -> Result<()> {
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_str().unwrap_or_default();

        if path.is_dir() {
            let subdir_relative = relative.join(file_name);
            if is_ignored(genignore, &subdir_relative) {
                debug!(path = %subdir_relative.display(), "Skipped (genignore)");
                continue;
            }
            let subdir_output = output_dir.join(file_name);
            fs::create_dir_all(&subdir_output)?;
            expand_local_dir(&path, &subdir_output, parser, globals, hyphenated_vars, genignore, &subdir_relative)?;
        } else {
            if file_name == "cargo-generate.toml" || file_name == ".genignore" {
                continue;
            }
            let file_relative = relative.join(file_name);
            if is_ignored(genignore, &file_relative) {
                debug!(path = %file_relative.display(), "Skipped (genignore)");
                continue;
            }
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read template file: {}", path.display()))?;
            expand_and_write(&contents, file_name, output_dir, parser, globals, hyphenated_vars)?;
        }
    }
    Ok(())
}

/// Extract all variable names referenced in Liquid tags from template content.
/// Returns underscored versions (since preprocessing already converts hyphens).
fn extract_template_variables(content: &str) -> Vec<String> {
    let mut vars = Vec::new();
    // Match variable references in {{ var }}, {{ var | filter }}, {% if var %}, etc.
    let var_re = Regex::new(r"(?:\{\{-?\s*|\{%-?\s*(?:if|elsif|unless|assign|for)\s+)([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    for cap in var_re.captures_iter(content) {
        vars.push(cap[1].to_string());
    }
    vars
}

/// Expand Liquid templates in content and write to output.
fn expand_and_write(
    contents: &str,
    file_name: &str,
    output_dir: &Path,
    parser: &liquid::Parser,
    globals: &liquid::Object,
    hyphenated_vars: &[String],
) -> Result<()> {
    // Determine output filename (strip .liquid extension)
    let output_name = if file_name.ends_with(".liquid") {
        &file_name[..file_name.len() - 7]
    } else {
        file_name
    };

    // Expand template variables
    let expanded = if contents.contains("{{") || contents.contains("{%") {
        // Preprocess: convert hyphenated variable names to underscored inside Liquid tags
        let preprocessed = preprocess_template(contents, hyphenated_vars);

        // Ensure all referenced variables exist in globals (default to empty string)
        // This matches cargo-generate behavior where missing variables render as ""
        let mut full_globals = globals.clone();
        for var_name in extract_template_variables(&preprocessed) {
            if !full_globals.contains_key(var_name.as_str()) {
                full_globals.insert(
                    var_name.into(),
                    liquid_core::Value::scalar(""),
                );
            }
        }

        match parser.parse(&preprocessed) {
            Ok(template) => template.render(&full_globals).unwrap_or_else(|err| {
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

        expand_and_write("hello world", "test.txt", dir.path(), &parser, &globals, &[]).unwrap();

        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_expand_and_write_liquid() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let mut globals = liquid::Object::new();
        globals.insert("name".into(), liquid_core::Value::scalar("fluvio"));

        expand_and_write("hello {{ name }}", "test.txt", dir.path(), &parser, &globals, &[]).unwrap();

        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello fluvio");
    }

    #[test]
    fn test_expand_and_write_strips_liquid_extension() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let globals = liquid::Object::new();

        expand_and_write("[package]", "Cargo.toml.liquid", dir.path(), &parser, &globals, &[]).unwrap();

        assert!(dir.path().join("Cargo.toml").exists());
        assert!(!dir.path().join("Cargo.toml.liquid").exists());
    }

    #[test]
    fn test_build_liquid_globals_uses_underscores() {
        let mut vars = HashMap::new();
        vars.insert("project-name".to_string(), "my-project".to_string());
        vars.insert("crate_name".to_string(), "my_project".to_string());

        let globals = build_liquid_globals(&vars).unwrap();
        // Hyphenated keys are stored with underscores
        assert!(globals.contains_key("project_name"));
        assert!(globals.contains_key("crate_name"));
    }

    #[test]
    fn test_preprocess_output_tags() {
        let vars = vec!["project-name".to_string()];
        let input = "name = \"{{project-name}}\"";
        let result = preprocess_template(input, &vars);
        assert_eq!(result, "name = \"{{project_name}}\"");
    }

    #[test]
    fn test_preprocess_output_tags_with_spaces() {
        let vars = vec!["project-name".to_string()];
        let input = "name = \"{{ project-name }}\"";
        let result = preprocess_template(input, &vars);
        assert_eq!(result, "name = \"{{ project_name }}\"");
    }

    #[test]
    fn test_preprocess_output_tags_with_filter() {
        let vars = vec!["project-group".to_string()];
        let input = "group = \"{{project-group | kebab_case}}\"";
        let result = preprocess_template(input, &vars);
        assert_eq!(result, "group = \"{{project_group | kebab_case}}\"");
    }

    #[test]
    fn test_preprocess_if_conditional() {
        let vars = vec!["connector-type".to_string()];
        let input = "{% if connector-type == \"source\" %}source{% endif %}";
        let result = preprocess_template(input, &vars);
        assert_eq!(
            result,
            "{% if connector_type == \"source\" %}source{% endif %}"
        );
    }

    #[test]
    fn test_preprocess_elsif() {
        let vars = vec!["smartmodule-type".to_string()];
        let input = "{% if smartmodule-type == \"filter\" %}f{% elsif smartmodule-type == \"map\" %}m{% endif %}";
        let result = preprocess_template(input, &vars);
        assert_eq!(
            result,
            "{% if smartmodule_type == \"filter\" %}f{% elsif smartmodule_type == \"map\" %}m{% endif %}"
        );
    }

    #[test]
    fn test_preprocess_truthy_check() {
        let vars = vec!["smartmodule-params".to_string()];
        let input = "{% if smartmodule-params %}has params{% endif %}";
        let result = preprocess_template(input, &vars);
        assert_eq!(
            result,
            "{% if smartmodule_params %}has params{% endif %}"
        );
    }

    #[test]
    fn test_preprocess_whitespace_trimming_tags() {
        let vars = vec!["connector-type".to_string()];
        let input = "{%- if connector-type == \"sink\" -%}sink{%- endif -%}";
        let result = preprocess_template(input, &vars);
        assert_eq!(
            result,
            "{%- if connector_type == \"sink\" -%}sink{%- endif -%}"
        );
    }

    #[test]
    fn test_preprocess_leaves_non_tag_text_alone() {
        let vars = vec!["project-name".to_string()];
        let input = "The project-name is {{ project-name }}";
        let result = preprocess_template(input, &vars);
        // "project-name" in prose is NOT changed, only inside {{ }}
        assert_eq!(result, "The project-name is {{ project_name }}");
    }

    #[test]
    fn test_preprocess_multiple_vars() {
        let vars = vec![
            "project-name".to_string(),
            "connector-type".to_string(),
        ];
        let input = "{{ project-name }} {% if connector-type == \"source\" %}src{% endif %}";
        let result = preprocess_template(input, &vars);
        assert_eq!(
            result,
            "{{ project_name }} {% if connector_type == \"source\" %}src{% endif %}"
        );
    }

    #[test]
    fn test_end_to_end_hyphenated_variable() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let mut variables = HashMap::new();
        // Only insert hyphenated version — build_liquid_globals maps it to underscore key
        // with the SAME value (the actual project name with hyphens)
        variables.insert("project-name".to_string(), "my-connector".to_string());
        let globals = build_liquid_globals(&variables).unwrap();
        let hyphenated = vec!["project-name".to_string()];

        let template = "name = \"{{project-name}}\"\ncrate = \"{{ project-name }}\"";
        expand_and_write(template, "test.toml", dir.path(), &parser, &globals, &hyphenated).unwrap();

        let content = fs::read_to_string(dir.path().join("test.toml")).unwrap();
        assert_eq!(content, "name = \"my-connector\"\ncrate = \"my-connector\"");
        assert!(!content.contains("{{"));
    }

    #[test]
    fn test_end_to_end_conditional_with_hyphenated_var() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let mut variables = HashMap::new();
        variables.insert("connector-type".to_string(), "source".to_string());
        variables.insert("connector_type".to_string(), "source".to_string());
        let globals = build_liquid_globals(&variables).unwrap();
        let hyphenated = vec!["connector-type".to_string()];

        let template = "{% if connector-type == \"source\" %}SOURCE{% else %}SINK{% endif %}";
        expand_and_write(template, "test.txt", dir.path(), &parser, &globals, &hyphenated).unwrap();

        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "SOURCE");
    }

    #[test]
    fn test_end_to_end_filter_with_hyphenated_var() {
        let dir = tempfile::tempdir().unwrap();
        let parser = liquid::ParserBuilder::with_stdlib()
            .filter(KebabCaseFilterParser)
            .build()
            .unwrap();
        let mut variables = HashMap::new();
        variables.insert("project-group".to_string(), "MyGroup".to_string());
        variables.insert("project_group".to_string(), "MyGroup".to_string());
        let globals = build_liquid_globals(&variables).unwrap();
        let hyphenated = vec!["project-group".to_string()];

        let template = "group = \"{{project-group | kebab_case}}\"";
        expand_and_write(template, "test.toml", dir.path(), &parser, &globals, &hyphenated).unwrap();

        let content = fs::read_to_string(dir.path().join("test.toml")).unwrap();
        assert_eq!(content, "group = \"my-group\"");
        assert!(!content.contains("{{"));
    }

    #[test]
    fn test_genignore_skips_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("template");
        fs::create_dir_all(&src).unwrap();

        // Create template files
        fs::write(src.join("keep.txt"), "hello").unwrap();
        fs::write(src.join("skip.log"), "should not appear").unwrap();
        fs::write(src.join(".genignore"), "*.log\n").unwrap();

        let out = dir.path().join("output");
        let config = GenerateConfig {
            name: "test".to_string(),
            destination: Some(out.clone()),
            define: vec![],
            silent: true,
            verbose: false,
        };

        generate(TemplateSource::LocalDir(src), config).unwrap();

        assert!(out.join("test").join("keep.txt").exists());
        assert!(!out.join("test").join("skip.log").exists());
    }

    #[test]
    fn test_genignore_handles_comments_and_blanks() {
        let content = "# comment\n\n*.tmp\n  \n# another comment\n*.bak\n";
        let globset = build_globset(content).unwrap();
        assert!(globset.is_match("foo.tmp"));
        assert!(globset.is_match("bar.bak"));
        assert!(!globset.is_match("keep.txt"));
    }

    #[test]
    fn test_genignore_empty_returns_none() {
        let content = "# only comments\n\n";
        assert!(build_globset(content).is_none());
    }

    #[test]
    fn test_full_generate_local_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("template");
        fs::create_dir_all(src.join("src")).unwrap();

        fs::write(
            src.join("Cargo.toml.liquid"),
            "[package]\nname = \"{{project-name}}\"",
        )
        .unwrap();
        fs::write(src.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(src.join("cargo-generate.toml"), "[placeholders]").unwrap();

        let out = dir.path().join("output");
        let config = GenerateConfig {
            name: "my-app".to_string(),
            destination: Some(out.clone()),
            define: vec![],
            silent: true,
            verbose: false,
        };

        let result = generate(TemplateSource::LocalDir(src), config).unwrap();

        assert_eq!(result, out.join("my-app"));
        // Cargo.toml.liquid should be renamed to Cargo.toml
        assert!(result.join("Cargo.toml").exists());
        assert!(!result.join("Cargo.toml.liquid").exists());
        // cargo-generate.toml should be skipped
        assert!(!result.join("cargo-generate.toml").exists());
        // Variable should be expanded
        let cargo = fs::read_to_string(result.join("Cargo.toml")).unwrap();
        assert_eq!(cargo, "[package]\nname = \"my-app\"");
        assert!(!cargo.contains("{{"));
        // Subdirectory preserved
        assert!(result.join("src").join("main.rs").exists());
    }
}
