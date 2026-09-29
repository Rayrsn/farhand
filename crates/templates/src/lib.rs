//! Templates management crate for farhand.
//!
//! Provides declarative YAML template parsing, embedded defaults via `include_str!`,
//! multi-tier resolution hierarchy (Project > User > Built-in), and monorepo matching.
#![forbid(unsafe_code)]

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

    #[error(
        "Invalid template name '{0}': must be 1-64 characters of letters, digits, '.', '_' or '-'"
    )]
    InvalidName(String),

    #[error("Workspace directory is required for project-scoped template operations")]
    MissingWorkspace,
}

/// Longest template name accepted by [`save_template`].
const MAX_NAME_LEN: usize = 64;

/// Whether `name` is safe to use as a filename component.
///
/// The name ends up in `templates/<name>.yaml`, and the caller may be a remote
/// client uploading YAML whose `name` field it chose. Without a character
/// whitelist a name like `../../../../etc/cron.d/evil` walks out of the
/// templates directory entirely, so the check is on the *characters* rather
/// than on specific bad patterns: no separator, no `..`, no absolute path and
/// no NUL can be expressed at all in this set.
///
/// This is deliberately stricter than the sanitiser in
/// `workspace::resolve_workspace_dir`, which maps offending characters to `_`
/// because a workspace name is hashed into a directory name and only needs to
/// be unique. Here the name is user-visible and doubles as a filename, so
/// silently rewriting it would write somewhere the caller did not ask for.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
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
        yaml: include_str!("../builtin/npm.yaml"),
    },
    BuiltinTemplate {
        name: "rust",
        yaml: include_str!("../builtin/rust.yaml"),
    },
    BuiltinTemplate {
        name: "go",
        yaml: include_str!("../builtin/go.yaml"),
    },
    BuiltinTemplate {
        name: "python",
        yaml: include_str!("../builtin/python.yaml"),
    },
    BuiltinTemplate {
        name: "maven",
        yaml: include_str!("../builtin/maven.yaml"),
    },
    BuiltinTemplate {
        name: "gradle",
        yaml: include_str!("../builtin/gradle.yaml"),
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

    // The filename comes from the *YAML*, not from the `name` argument, so a
    // caller uploading YAML with a hostile `name:` field controls the path
    // that gets written. It is validated rather than sanitised: silently
    // rewriting it would write somewhere the caller did not name.
    if !is_valid_name(&parsed.name) {
        return Err(TemplateError::InvalidName(parsed.name));
    }

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

    // Defence in depth. The character whitelist above already makes escape
    // impossible, but this is the invariant the project actually cares about
    // (a write must stay inside the templates directory), and a future change
    // to the whitelist should not be able to violate it silently.
    if target_file.parent() != Some(target_dir.as_path()) {
        return Err(TemplateError::InvalidName(parsed.name));
    }

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

    /// The filename is built from the `name` field *inside the uploaded YAML*,
    /// not from the name the caller passed, and that YAML can arrive from a
    /// remote client via `PUT_TEMPLATE`. Before this was validated, a name
    /// like `../../../../etc/whatever` wrote straight out of the templates
    /// directory — confirmed to escape the workdir entirely.
    ///
    /// Asserts the *specific* rejection rather than merely "it errored":
    /// without the check, a traversal whose parent directory happens to exist
    /// would succeed outright, and one whose parent does not would fail with a
    /// generic I/O error, so a naive `is_err()` assertion would pass for
    /// entirely the wrong reason.
    #[test]
    fn test_save_template_rejects_names_that_escape_the_templates_dir() {
        let ws = tempdir().unwrap();
        for hostile in [
            "../escaped",
            "../../escaped",
            "../../../../../tmp/ESCAPED",
            "sub/dir",
            "..",
            ".",
            "",
            "/etc/passwd",
        ] {
            // Single-quoted so the name reaches the validator verbatim rather
            // than being mangled by YAML's own escaping rules first.
            let yaml = format!("name: '{hostile}'\ndescription: x\n");
            match save_template(Some(ws.path()), "ignored", &yaml, "project") {
                Ok(p) => panic!("name {hostile:?} was accepted, wrote {}", p.display()),
                Err(TemplateError::InvalidName(n)) => assert_eq!(n, hostile),
                Err(other) => panic!(
                    "name {hostile:?} failed for the wrong reason: {other} \
                     (an I/O error means the write was merely blocked by a \
                     missing parent directory, not validated)"
                ),
            }
        }

        // Nothing at all may have been written under the templates directory.
        let mut written: Vec<_> = std::fs::read_dir(ws.path().join(".farhand/templates"))
            .map(|d| d.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        written.sort();
        assert!(
            written.is_empty(),
            "a template file was written: {written:?}"
        );
    }

    /// Character-level coverage for the validator itself, kept separate so
    /// YAML escaping cannot mask a case.
    #[test]
    fn test_is_valid_name_rejects_unsafe_characters() {
        for bad in [
            "",
            ".",
            "..",
            "/",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "a b",
            "a\0b",
            "a:b",
            "a*b",
            "a?b",
            "a|b",
            "a\nb",
            "héllo",
            "日本語",
        ] {
            assert!(!is_valid_name(bad), "{bad:?} should be rejected");
        }
        for good in [
            "a",
            "zig",
            "rust2",
            "my-template",
            "my_template",
            "a.b.c",
            ".hidden",
        ] {
            assert!(is_valid_name(good), "{good:?} should be accepted");
        }
        assert!(is_valid_name(&"a".repeat(MAX_NAME_LEN)));
        assert!(!is_valid_name(&"a".repeat(MAX_NAME_LEN + 1)));
    }

    /// The negative control: ordinary names, including ones with dots,
    /// hyphens, and underscores, must still work.
    #[test]
    fn test_save_template_accepts_ordinary_names() {
        let ws = tempdir().unwrap();
        for name in ["zig", "my-template", "my_template", "rust2", "a.b.c"] {
            let yaml = format!("name: {name}\ndescription: x\n");
            let saved = save_template(Some(ws.path()), name, &yaml, "project")
                .unwrap_or_else(|e| panic!("name {name:?} rejected: {e}"));
            assert!(saved.exists());
            assert_eq!(
                saved.parent().unwrap(),
                ws.path().join(".farhand/templates")
            );
        }
    }

    /// A name that is too long is rejected rather than truncated, so the file
    /// written always matches the name the caller asked for.
    #[test]
    fn test_save_template_rejects_overlong_names() {
        let ws = tempdir().unwrap();
        let name = "a".repeat(MAX_NAME_LEN + 1);
        let yaml = format!("name: {name}\ndescription: x\n");
        assert!(save_template(Some(ws.path()), &name, &yaml, "project").is_err());
    }
}

#[cfg(test)]
mod packaging_tests {
    /// The builtin templates must live inside this crate's own directory.
    ///
    /// `cargo publish` packages only the files under the crate directory, then
    /// *verifies* the package by building it. Templates read from a
    /// repository-level `templates/` directory therefore compile fine in-tree,
    /// pass the whole test suite, and break `cargo install farhand-cli` for
    /// everyone on crates.io — and they break it *mid-chain*, after the crates
    /// below this one are already published and can never be removed.
    ///
    /// Both halves are checked: the files are inside the crate, and the
    /// repository-level directory they used to live in does not exist, so
    /// nobody is tempted to move them back.
    #[test]
    fn builtin_templates_live_inside_the_crate() {
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let builtin = crate_root.join("builtin");
        assert!(
            builtin.is_dir(),
            "expected the builtin templates at {}",
            builtin.display()
        );

        for name in ["rust", "npm", "go", "python", "maven", "gradle"] {
            let file = builtin.join(format!("{name}.yaml"));
            assert!(
                file.is_file(),
                "missing builtin template: {}",
                file.display()
            );
        }

        // A crate cannot read above itself once packaged, so the old
        // repository-level location must be gone for good.
        let repo_root = crate_root
            .parent()
            .and_then(|p| p.parent())
            .expect("crate is two levels below the repository root");
        let old_location = repo_root.join("templates");
        assert!(
            !old_location.exists(),
            "{} still exists. If these are duplicates of the crate's own \
             builtin/ directory, delete them — otherwise a future edit to one \
             copy will silently not be the one that ships.",
            old_location.display()
        );
    }
}
