//! Configuration management crate for farhand.
//!
//! Handles loading `.farhand.yaml`, environment variable interpolation (`${VAR}`),
//! and configuration parsing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("I/O error reading configuration file '{0}': {1}")]
    Io(String, #[source] std::io::Error),

    #[error("YAML syntax error in configuration file '{0}': {1}")]
    Yaml(String, #[source] serde_yaml::Error),
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentConfig {
    pub host: String,
    pub token: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A build output target which can be either a simple path or a structured entry with conditions.
#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
#[serde(untagged)]
pub enum OutputItem {
    Simple(String),
    Detailed {
        path: String,
        #[serde(default)]
        condition: Option<String>,
    },
}

impl OutputItem {
    pub fn is_active(&self) -> bool {
        match self {
            Self::Simple(_) => true,
            Self::Detailed { condition, .. } => {
                if let Some(cond) = condition {
                    let trimmed = cond.trim().to_lowercase();
                    !trimmed.is_empty()
                        && trimmed != "0"
                        && trimmed != "false"
                        && trimmed != "no"
                        && trimmed != "off"
                } else {
                    true
                }
            }
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::Simple(p) => p.as_str(),
            Self::Detailed { path, .. } => path.as_str(),
        }
    }
}

impl PartialEq for OutputItem {
    fn eq(&self, other: &Self) -> bool {
        self.path() == other.path()
    }
}

impl PartialEq<&str> for OutputItem {
    fn eq(&self, other: &&str) -> bool {
        self.path() == *other
    }
}

impl PartialEq<String> for OutputItem {
    fn eq(&self, other: &String) -> bool {
        self.path() == other.as_str()
    }
}

impl From<&str> for OutputItem {
    fn from(s: &str) -> Self {
        OutputItem::Simple(s.to_string())
    }
}

impl From<String> for OutputItem {
    fn from(s: String) -> Self {
        OutputItem::Simple(s)
    }
}

/// Project-local configuration parsed from `.farhand.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub host: Option<String>,
    pub token: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub outputs: Vec<OutputItem>,
    #[serde(alias = "out_dir")]
    pub out_dir: Option<String>,
    #[serde(alias = "insecure_skip_token", default)]
    pub insecure_skip_token: bool,
    #[serde(default)]
    pub verbose: bool,
    pub template: Option<String>,
    #[serde(alias = "agent_tag")]
    pub agent_tag: Option<String>,
    #[serde(alias = "no_cache", default)]
    pub no_cache: bool,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(alias = "forward_env", default = "default_true")]
    pub forward_env: bool,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub forward: Vec<String>,
    #[serde(default)]
    pub compression: Option<String>,
}

impl Config {
    /// Returns the active output paths, filtering out items whose condition evaluates to false.
    pub fn resolved_outputs(&self) -> Vec<String> {
        self.outputs
            .iter()
            .filter(|o| o.is_active())
            .map(|o| o.path().to_string())
            .collect()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: None,
            token: None,
            name: None,
            outputs: Vec::new(),
            out_dir: None,
            insecure_skip_token: false,
            verbose: false,
            template: None,
            agent_tag: None,
            no_cache: false,
            agents: Vec::new(),
            forward_env: true,
            env: HashMap::new(),
            tty: false,
            forward: Vec::new(),
            compression: None,
        }
    }
}

/// Expands environment variable expressions in a string.
///
/// Supported syntax:
/// - `${VAR}`: Value of `VAR`, or empty string if unset.
/// - `${VAR:-default}`: Value of `VAR`, or `default` if unset or empty.
/// - `$VAR`: Value of `VAR` (alphanumeric + underscore identifier), or empty string if unset.
/// - `$$`: Escaped literal `$`.
pub fn interpolate_env(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&next_c) = chars.peek() {
                if next_c == '$' {
                    chars.next();
                    result.push('$');
                    continue;
                }
                if next_c == '{' {
                    chars.next(); // consume '{'
                    let mut var_expr = String::new();
                    let mut closed = false;
                    for inner in chars.by_ref() {
                        if inner == '}' {
                            closed = true;
                            break;
                        }
                        var_expr.push(inner);
                    }
                    if closed {
                        if let Some((var_name, default_val)) = var_expr.split_once(":-") {
                            match std::env::var(var_name) {
                                Ok(val) if !val.is_empty() => result.push_str(&val),
                                _ => result.push_str(default_val),
                            }
                        } else if let Ok(val) = std::env::var(&var_expr) {
                            result.push_str(&val);
                        }
                    } else {
                        // Unclosed brace, keep literal prefix
                        result.push('$');
                        result.push('{');
                        result.push_str(&var_expr);
                    }
                    continue;
                }
                if next_c.is_ascii_alphabetic() || next_c == '_' {
                    let mut var_name = String::new();
                    while let Some(&ident_c) = chars.peek() {
                        if ident_c.is_ascii_alphanumeric() || ident_c == '_' {
                            var_name.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                    if let Ok(val) = std::env::var(&var_name) {
                        result.push_str(&val);
                    }
                    continue;
                }
            }
            result.push('$');
        } else {
            result.push(c);
        }
    }

    result
}

/// Load and parse a `.farhand.yaml` file from the given path.
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let raw_content =
        fs::read_to_string(path).map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
    let interpolated = interpolate_env(&raw_content);
    let config: Config = serde_yaml::from_str(&interpolated)
        .map_err(|e| ConfigError::Yaml(path.display().to_string(), e))?;
    Ok(config)
}

/// Load configuration if it exists.
///
/// If `is_explicit` is true and the file does not exist, returns an `Err(ConfigError::Io)`.
/// If `is_explicit` is false and the file does not exist, returns `Ok(None)`.
pub fn load_config_optional(path: &Path, is_explicit: bool) -> Result<Option<Config>, ConfigError> {
    if !path.exists() {
        if is_explicit {
            return Err(ConfigError::Io(
                path.display().to_string(),
                std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            ));
        }
        return Ok(None);
    }
    load_config(path).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_interpolate_env_vars() {
        std::env::set_var("TEST_FARHAND_VAR", "my_secret_token");
        std::env::set_var("TEST_FARHAND_PORT", "9876");

        assert_eq!(
            interpolate_env("token: ${TEST_FARHAND_VAR}"),
            "token: my_secret_token"
        );
        assert_eq!(
            interpolate_env("host: 127.0.0.1:$TEST_FARHAND_PORT"),
            "host: 127.0.0.1:9876"
        );
        assert_eq!(
            interpolate_env("host: ${TEST_UNDEFINED_VAR:-192.168.1.100:9876}"),
            "host: 192.168.1.100:9876"
        );
        assert_eq!(interpolate_env("token: ${TEST_UNDEFINED_VAR}"), "token: ");
        assert_eq!(interpolate_env("escaped: $$100"), "escaped: $100");
    }

    #[test]
    fn test_parse_full_config() {
        std::env::set_var("TEST_CONFIG_TOKEN", "interpolated-secret");

        let yaml = r#"
host: 192.168.1.50:9876
token: ${TEST_CONFIG_TOKEN}
name: custom-project
outputs:
  - dist/
  - coverage.lcov
outDir: ./custom-out
insecureSkipToken: true
verbose: true
template: rust-wasm
agentTag: gpu
noCache: true
"#;
        let dir = tempdir().unwrap();
        let config_path = dir.path().join(".farhand.yaml");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let cfg = load_config(&config_path).unwrap();
        assert_eq!(cfg.host.as_deref(), Some("192.168.1.50:9876"));
        assert_eq!(cfg.token.as_deref(), Some("interpolated-secret"));
        assert_eq!(cfg.name.as_deref(), Some("custom-project"));
        assert_eq!(cfg.outputs, vec!["dist/", "coverage.lcov"]);
        assert_eq!(cfg.out_dir.as_deref(), Some("./custom-out"));
        assert!(cfg.insecure_skip_token);
        assert!(cfg.verbose);
        assert_eq!(cfg.template.as_deref(), Some("rust-wasm"));
        assert_eq!(cfg.agent_tag.as_deref(), Some("gpu"));
        assert!(cfg.no_cache);
    }

    #[test]
    fn test_parse_snake_case_aliases() {
        let yaml = r#"
out_dir: ./snake-out
insecure_skip_token: false
agent_tag: build-box
no_cache: false
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.out_dir.as_deref(), Some("./snake-out"));
        assert!(!cfg.insecure_skip_token);
        assert_eq!(cfg.agent_tag.as_deref(), Some("build-box"));
        assert!(!cfg.no_cache);
    }

    #[test]
    fn test_load_config_optional_missing_default() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join(".farhand.yaml");
        let res = load_config_optional(&missing, false).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn test_load_config_optional_missing_explicit() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nonexistent.yaml");
        let res = load_config_optional(&missing, true);
        assert!(res.is_err());
    }

    #[test]
    fn test_parse_agents_list() {
        std::env::set_var("AGENT_ONE_HOST", "10.0.0.1:9876");
        std::env::set_var("AGENT_ONE_TOKEN", "secret-one");

        let yaml = r#"
name: multi-agent-app
agents:
  - host: ${AGENT_ONE_HOST}
    token: ${AGENT_ONE_TOKEN}
    tags: [lan, fast, x86_64]
  - host: 10.0.0.2:9876
    token: secret-two
    tags: [cloud, gpu]
"#;
        let dir = tempdir().unwrap();
        let config_path = dir.path().join(".farhand.yaml");
        fs::write(&config_path, yaml).unwrap();

        let cfg = load_config(&config_path).unwrap();
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].host, "10.0.0.1:9876");
        assert_eq!(cfg.agents[0].token.as_deref(), Some("secret-one"));
        assert_eq!(cfg.agents[0].tags, vec!["lan", "fast", "x86_64"]);

        assert_eq!(cfg.agents[1].host, "10.0.0.2:9876");
        assert_eq!(cfg.agents[1].token.as_deref(), Some("secret-two"));
        assert_eq!(cfg.agents[1].tags, vec!["cloud", "gpu"]);
    }

    #[test]
    fn test_parse_env_forwarding_options() {
        // Default: forward_env is true, env is empty
        let cfg_default: Config = serde_yaml::from_str("name: test-app\n").unwrap();
        assert!(cfg_default.forward_env);
        assert!(cfg_default.env.is_empty());

        // Explicitly disabled
        let yaml_disabled = r#"
forwardEnv: false
env:
  DATABASE_URL: postgres://localhost/db
  API_KEY: secret-123
"#;
        let cfg_disabled: Config = serde_yaml::from_str(yaml_disabled).unwrap();
        assert!(!cfg_disabled.forward_env);
        assert_eq!(
            cfg_disabled.env.get("DATABASE_URL").map(|s| s.as_str()),
            Some("postgres://localhost/db")
        );
        assert_eq!(
            cfg_disabled.env.get("API_KEY").map(|s| s.as_str()),
            Some("secret-123")
        );

        // Snake case alias
        let yaml_snake = "forward_env: false\n";
        let cfg_snake: Config = serde_yaml::from_str(yaml_snake).unwrap();
        assert!(!cfg_snake.forward_env);
    }

    #[test]
    fn test_parse_structured_outputs() {
        let yaml = r#"
name: structured-app
outputs:
  - "dist/bundle.js"
  - path: "build/debug-symbols.pdb"
    condition: "false"
  - path: "build/docs"
    condition: "true"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.outputs.len(), 3);
        let active = cfg.resolved_outputs();
        assert_eq!(active, vec!["dist/bundle.js", "build/docs"]);
    }
}
