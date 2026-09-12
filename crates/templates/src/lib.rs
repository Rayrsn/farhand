//! Templates management crate for farhand.
//!
//! Provides declarative YAML template parsing, embedded defaults via `include_str!`,
//! multi-tier resolution hierarchy (Project > User > Built-in), and monorepo matching.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TemplateError {
    #[error("I/O error reading template from '{0}': {1}")]
    Io(String, #[source] std::io::Error),

    #[error("YAML syntax error in template '{0}': {1}")]
    Yaml(String, #[source] serde_yaml::Error),

    #[error("Template '{0}' was not found")]
    NotFound(String),

    #[error("Invalid template scope '{0}', must be 'user' or 'project'")]
    InvalidScope(String),

    #[error("Workspace directory is required for project-scoped template operations")]
    MissingWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MatchConditions {
    #[serde(rename = "anyFile", default)]
    pub any_file: Vec<String>,
    #[serde(rename = "allFiles", default)]
    pub all_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Hints {
    #[serde(rename = "installCommand")]
    pub install_command: Option<String>,
    #[serde(default)]
    pub lockfiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub r#match: MatchConditions,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(rename = "outputsIgnore", default)]
    pub outputs_ignore: Vec<String>,
    #[serde(rename = "ignoreExtra", default)]
    pub ignore_extra: Vec<String>,
    #[serde(default)]
    pub hints: Hints,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateSource {
    Builtin,
    User,
    Project,
}

impl std::fmt::Display for TemplateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateSource::Builtin => write!(f, "builtin"),
            TemplateSource::User => write!(f, "user"),
            TemplateSource::Project => write!(f, "project"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedTemplate {
    pub template: Template,
    pub source: TemplateSource,
    pub raw_yaml: String,
}

pub struct BuiltinTemplate {
    pub name: &'static str,
    pub yaml: &'static str,
}

pub const BUILTIN_TEMPLATES: &[BuiltinTemplate] = &[
    BuiltinTemplate {
        name: "npm",
        yaml: include_str!("../../../templates/builtin/npm.yaml"),
    },
    BuiltinTemplate {
        name: "rust",
        yaml: include_str!("../../../templates/builtin/rust.yaml"),
    },
    BuiltinTemplate {
        name: "go",
        yaml: include_str!("../../../templates/builtin/go.yaml"),
    },
    BuiltinTemplate {
        name: "python",
        yaml: include_str!("../../../templates/builtin/python.yaml"),
    },
    BuiltinTemplate {
        name: "maven",
        yaml: include_str!("../../../templates/builtin/maven.yaml"),
    },
    BuiltinTemplate {
        name: "gradle",
        yaml: include_str!("../../../templates/builtin/gradle.yaml"),
    },
];

/// Returns the user-level template directory (`~/.farhand/templates`).
pub fn default_user_templates_dir() -> Option<PathBuf> {
    if let Ok(override_dir) = std::env::var("FARHAND_TEMPLATES_DIR") {
        return Some(PathBuf::from(override_dir));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".farhand").join("templates"))
}

/// Load all available templates according to the resolution hierarchy:
/// Project (.farhand/templates/*.yaml) > User (~/.farhand/templates/*.yaml) > Built-in.
pub fn load_templates(workspace_root: Option<&Path>) -> HashMap<String, LoadedTemplate> {
    let mut map = HashMap::new();

    // 1. Builtin templates
    for b in BUILTIN_TEMPLATES {
        if let Ok(t) = serde_yaml::from_str::<Template>(b.yaml) {
            map.insert(
                b.name.to_string(),
                LoadedTemplate {
                    template: t,
                    source: TemplateSource::Builtin,
                    raw_yaml: b.yaml.to_string(),
                },
            );
        }
    }

    // 2. User templates (~/.farhand/templates)
    if let Some(user_dir) = default_user_templates_dir() {
        load_dir_templates(&user_dir, TemplateSource::User, &mut map);
    }

    // 3. Project templates (<workspace>/.farhand/templates)
    if let Some(ws) = workspace_root {
        let project_dir = ws.join(".farhand").join("templates");
        load_dir_templates(&project_dir, TemplateSource::Project, &mut map);
    }

    map
}

fn load_dir_templates(
    dir: &Path,
    source: TemplateSource,
    out: &mut HashMap<String, LoadedTemplate>,
) {
    if !dir.is_dir() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry_res in entries.flatten() {
        let path = entry_res.path();
        if let Some(ext) = path.extension() {
            if ext == "yaml" || ext == "yml" {
                if let Ok(raw_yaml) = fs::read_to_string(&path) {
                    if let Ok(template) = serde_yaml::from_str::<Template>(&raw_yaml) {
                        out.insert(
                            template.name.clone(),
                            LoadedTemplate {
                                template,
                                source,
                                raw_yaml,
                            },
                        );
                    }
                }
            }
        }
    }
}

/// Check if a template matches files present in `workspace_root`.
pub fn matches_workspace(template: &Template, workspace_root: &Path) -> bool {
    let has_any_condition = !template.r#match.any_file.is_empty();
    let has_all_condition = !template.r#match.all_files.is_empty();

    if !has_any_condition && !has_all_condition {
        return false;
    }

    // 1. If allFiles specified, EVERY pattern must match at least one file
    if has_all_condition {
        for pattern in &template.r#match.all_files {
            if !pattern_matches(workspace_root, pattern) {
                return false;
            }
        }
    }

    // 2. If anyFile specified, AT LEAST ONE pattern must match
    if has_any_condition {
        let any_matched = template
            .r#match
            .any_file
            .iter()
            .any(|pattern| pattern_matches(workspace_root, pattern));
        if !any_matched {
            return false;
        }
    }

    true
}

fn pattern_matches(workspace_root: &Path, pattern: &str) -> bool {
    let clean = pattern.trim().trim_matches('/');
    let target = workspace_root.join(clean);

    if clean.contains('*') || clean.contains('?') || clean.contains('[') {
        if let Ok(mut entries) = glob::glob(&target.to_string_lossy()) {
            return entries.next().is_some();
        }
    } else if target.exists() {
        return true;
    }
    false
}

/// Match all applicable templates for a workspace, respecting explicit template overrides.
pub fn match_templates(workspace_root: &Path, explicit_template: Option<&str>) -> Vec<Template> {
    let templates = load_templates(Some(workspace_root));
    if let Some(target_name) = explicit_template {
        templates
            .get(target_name)
            .map(|lt| vec![lt.template.clone()])
            .unwrap_or_default()
    } else {
        let mut matched: Vec<_> = templates
            .into_values()
            .filter(|lt| matches_workspace(&lt.template, workspace_root))
            .map(|lt| lt.template)
            .collect();
        matched.sort_by(|a, b| a.name.cmp(&b.name));
        matched
    }
}

/// Resolve candidate output paths using templates (union of all matching templates).
pub fn resolve_template_outputs(
    workspace_root: &Path,
    explicit_template: Option<&str>,
) -> Vec<String> {
    let matched = match_templates(workspace_root, explicit_template);
    let mut outputs = Vec::new();
    for t in matched {
        outputs.extend(t.outputs);
    }
    outputs.sort();
    outputs.dedup();
    outputs
}

/// Resolve extra ignore rules declared by matching templates.
pub fn resolve_template_extra_ignores(
    workspace_root: &Path,
    explicit_template: Option<&str>,
) -> Vec<String> {
    let matched = match_templates(workspace_root, explicit_template);
    let mut extra_ignores = Vec::new();
    for t in matched {
        extra_ignores.extend(t.ignore_extra);
    }
    extra_ignores.sort();
    extra_ignores.dedup();
    extra_ignores
}

/// Resolve candidate output ignore patterns declared by matching templates.
pub fn resolve_template_outputs_ignores(
    workspace_root: &Path,
    explicit_template: Option<&str>,
) -> Vec<String> {
    let matched = match_templates(workspace_root, explicit_template);
    let mut outputs_ignores = Vec::new();
    for t in matched {
        outputs_ignores.extend(t.outputs_ignore);
    }
    outputs_ignores.sort();
    outputs_ignores.dedup();
    outputs_ignores
}

/// Save a template into the user or project directory.
pub fn save_template(
    workspace_root: Option<&Path>,
    name: &str,
    yaml: &str,
    scope: &str,
) -> Result<PathBuf, TemplateError> {
    // Validate that YAML is a valid template
    let parsed: Template =
        serde_yaml::from_str(yaml).map_err(|e| TemplateError::Yaml(name.to_string(), e))?;

    let target_dir = match scope {
        "project" => {
            let ws = workspace_root.ok_or(TemplateError::MissingWorkspace)?;
            ws.join(".farhand").join("templates")
        }
        "user" => default_user_templates_dir().ok_or_else(|| {
            TemplateError::Io(
                "user home directory".into(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory found"),
            )
        })?,
        _ => return Err(TemplateError::InvalidScope(scope.to_string())),
    };

    fs::create_dir_all(&target_dir)
        .map_err(|e| TemplateError::Io(target_dir.display().to_string(), e))?;

    let target_file = target_dir.join(format!("{}.yaml", parsed.name));
    fs::write(&target_file, yaml)
        .map_err(|e| TemplateError::Io(target_file.display().to_string(), e))?;

    Ok(target_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_builtin_templates_parsed() {
        let templates = load_templates(None);
        assert!(templates.contains_key("npm"));
        assert!(templates.contains_key("rust"));
        assert!(templates.contains_key("go"));
        assert!(templates.contains_key("python"));
        assert!(templates.contains_key("maven"));
        assert!(templates.contains_key("gradle"));

        let rust = &templates["rust"].template;
        assert_eq!(rust.name, "rust");
        assert!(rust.outputs.contains(&"target/release".to_string()));
        assert!(rust.hints.lockfiles.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn test_template_resolution_override_order() {
        let ws = tempdir().unwrap();
        let project_dir = ws.path().join(".farhand").join("templates");
        fs::create_dir_all(&project_dir).unwrap();

        // Project override for rust template with custom outputs
        let custom_rust = r#"
name: rust
description: Custom project rust template
match:
  anyFile:
    - Cargo.toml
outputs:
  - target/custom-bin
"#;
        fs::write(project_dir.join("rust.yaml"), custom_rust).unwrap();

        let templates = load_templates(Some(ws.path()));
        let loaded = &templates["rust"];
        assert_eq!(loaded.source, TemplateSource::Project);
        assert_eq!(loaded.template.outputs, vec!["target/custom-bin"]);
    }

    #[test]
    fn test_matches_workspace() {
        let ws = tempdir().unwrap();
        fs::write(ws.path().join("Cargo.toml"), "[package]\nname=\"foo\"\n").unwrap();

        let templates = load_templates(Some(ws.path()));
        assert!(matches_workspace(&templates["rust"].template, ws.path()));
        assert!(!matches_workspace(&templates["npm"].template, ws.path()));
    }

    #[test]
    fn test_monorepo_outputs_union() {
        let ws = tempdir().unwrap();
        fs::write(ws.path().join("Cargo.toml"), "").unwrap();
        fs::write(ws.path().join("package.json"), "").unwrap();

        let outputs = resolve_template_outputs(ws.path(), None);
        // Union of npm and rust outputs
        assert!(outputs.contains(&"dist".to_string()));
        assert!(outputs.contains(&"target/release".to_string()));
    }

    #[test]
    fn test_explicit_template_selection() {
        let ws = tempdir().unwrap();
        fs::write(ws.path().join("Cargo.toml"), "").unwrap();
        fs::write(ws.path().join("package.json"), "").unwrap();

        // Explicitly requesting npm should ignore Cargo.toml outputs
        let outputs = resolve_template_outputs(ws.path(), Some("npm"));
        assert!(outputs.contains(&"dist".to_string()));
        assert!(!outputs.contains(&"target/release".to_string()));
    }

    #[test]
    fn test_save_template_project_scope() {
        let ws = tempdir().unwrap();
        let yaml = r#"
name: zig
description: Zig build toolchain
match:
  anyFile:
    - build.zig
outputs:
  - zig-out
"#;
        let saved_path = save_template(Some(ws.path()), "zig", yaml, "project").unwrap();
        assert!(saved_path.exists());
        assert!(saved_path.ends_with(".farhand/templates/zig.yaml"));

        let loaded = load_templates(Some(ws.path()));
        assert!(loaded.contains_key("zig"));
        assert_eq!(loaded["zig"].source, TemplateSource::Project);
    }
}
