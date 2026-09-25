use std::fs;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum InitError {
    #[error("Configuration file already exists in '{0}'. Use --force to overwrite.")]
    AlreadyExists(String),

    #[error("I/O error at '{0}': {1}")]
    Io(String, #[source] std::io::Error),

    #[error("Failed to save template: {0}")]
    Template(String),
}

#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub path: PathBuf,
    pub host: Option<String>,
    pub token: Option<String>,
    pub name: Option<String>,
    pub template: Option<String>,
    pub with_template: bool,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct InitResult {
    pub config_path: PathBuf,
    pub project_name: String,
    pub detected_template: Option<String>,
    pub template_path: Option<PathBuf>,
}

pub fn init_project(opts: &InitOptions) -> Result<InitResult, InitError> {
    let target_dir = if opts.path.exists() {
        opts.path
            .canonicalize()
            .map_err(|e| InitError::Io(format!("canonicalizing '{}'", opts.path.display()), e))?
    } else {
        fs::create_dir_all(&opts.path).map_err(|e| {
            InitError::Io(format!("creating directory '{}'", opts.path.display()), e)
        })?;
        opts.path
            .canonicalize()
            .unwrap_or_else(|_| opts.path.clone())
    };

    let config_file = target_dir.join(".farhand.yaml");
    let yml_file = target_dir.join(".farhand.yml");
    if (config_file.exists() || yml_file.exists()) && !opts.force {
        return Err(InitError::AlreadyExists(target_dir.display().to_string()));
    }

    // Determine project name
    let project_name = opts
        .name
        .clone()
        .or_else(|| {
            target_dir
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "my-project".to_string());

    // Detect template
    let detected_template = if let Some(t) = &opts.template {
        Some(t.clone())
    } else {
        let matched = templates::match_templates(&target_dir, None);
        matched.first().map(|t| t.name.clone())
    };

    // Determine default outputs based on template
    let outputs_yaml = match detected_template.as_deref() {
        Some("rust") => "outputs:\n  - target/release".to_string(),
        Some("npm") => "outputs:\n  - dist".to_string(),
        Some("go") => "outputs:\n  - bin".to_string(),
        Some("python") => "outputs:\n  - dist".to_string(),
        Some("maven") => "outputs:\n  - target".to_string(),
        Some("gradle") => "outputs:\n  - build/libs".to_string(),
        _ => "outputs:\n  - dist".to_string(),
    };

    let template_line = if let Some(t) = &detected_template {
        format!("template: {}", t)
    } else {
        "# template: rust  # (rust, npm, go, python, maven, gradle)".to_string()
    };

    let host_line = match &opts.host {
        Some(h) => format!("host: \"{}\"", h),
        None => "host: \"${FARHAND_HOST:-127.0.0.1:9876}\"".to_string(),
    };

    let token_line = match &opts.token {
        Some(tok) => {
            // Plaintext tokens in committed files are a leak risk; nudge
            // users toward environment interpolation (the default).
            eprintln!(
                "Warning: writing the token in plaintext to .farhand.yaml. \
                 Prefer `fh init --token-env` (token: \"${{FARHAND_TOKEN}}\") or \
                 `fh init` without flags and export FARHAND_TOKEN."
            );
            format!("token: \"{}\"", tok)
        }
        None => "token: \"${FARHAND_TOKEN}\"".to_string(),
    };

    let content = format!(
        r#"# ==============================================================================
# Farhand Project Configuration (.farhand.yaml)
# ==============================================================================
# Offload heavy compilation, bundling, and testing to a remote build agent.
#
# Quickstart:
#   export FARHAND_HOST="192.168.1.50:9876"
#   export FARHAND_TOKEN="your-secret-token"
#   fh cargo build --release
# ==============================================================================

# Remote agent TCP address (host:port)
{host_line}

# Shared authentication token (can be injected via environment variable)
{token_line}

# Project identifier and persistent workspace folder key on the remote agent
name: {project_name}

# Build environment preset (provides automatic dependency caching & ignores)
{template_line}

# Build artifacts to automatically retrieve upon exit code 0
{outputs_yaml}

# Local directory where retrieved artifacts will be extracted
outDir: "."

# Wire compression algorithm ('zstd', 'gzip', 'none')
compression: zstd

# Print detailed file scan, delta sync, and execution telemetry
verbose: false
"#
    );

    fs::write(&config_file, content)
        .map_err(|e| InitError::Io(config_file.display().to_string(), e))?;

    let mut template_path = None;
    if opts.with_template {
        let t_name = detected_template.as_deref().unwrap_or("custom");
        let available = templates::load_templates(Some(&target_dir));
        let yaml_content = if let Some(t) = available.get(t_name) {
            t.raw_yaml.clone()
        } else {
            format!(
                "name: {}\ndescription: Custom {} build toolchain\nmatch:\n  anyFile:\n    - {}.json\noutputs:\n  - dist\nignoreExtra: []\n",
                t_name, t_name, t_name
            )
        };
        let p = templates::save_template(Some(&target_dir), t_name, &yaml_content, "project")
            .map_err(|e| InitError::Template(e.to_string()))?;
        template_path = Some(p);
    }

    Ok(InitResult {
        config_path: config_file,
        project_name,
        detected_template,
        template_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_init_in_empty_dir() {
        let dir = tempdir().unwrap();
        let opts = InitOptions {
            path: dir.path().to_path_buf(),
            name: Some("test-project".to_string()),
            host: Some("10.0.0.1:9876".to_string()),
            token: Some("secret123".to_string()),
            template: None,
            with_template: false,
            force: false,
        };

        let res = init_project(&opts).unwrap();
        assert_eq!(res.project_name, "test-project");
        assert!(res.config_path.exists());

        let content = fs::read_to_string(&res.config_path).unwrap();
        assert!(content.contains("name: test-project"));
        assert!(content.contains("host: \"10.0.0.1:9876\""));
        assert!(content.contains("token: \"secret123\""));
        assert!(content.contains("compression: zstd"));
        assert!(content.contains("outDir: \".\""));
    }

    #[test]
    fn test_init_auto_detects_rust() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\n",
        )
        .unwrap();

        let opts = InitOptions {
            path: dir.path().to_path_buf(),
            name: None,
            host: None,
            token: None,
            template: None,
            with_template: false,
            force: false,
        };

        let res = init_project(&opts).unwrap();
        assert_eq!(res.detected_template, Some("rust".to_string()));

        let content = fs::read_to_string(&res.config_path).unwrap();
        assert!(content.contains("template: rust"));
        assert!(content.contains("target/release"));
    }

    #[test]
    fn test_init_auto_detects_npm() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\": \"my-app\"}").unwrap();

        let opts = InitOptions {
            path: dir.path().to_path_buf(),
            name: None,
            host: None,
            token: None,
            template: None,
            with_template: false,
            force: false,
        };

        let res = init_project(&opts).unwrap();
        assert_eq!(res.detected_template, Some("npm".to_string()));

        let content = fs::read_to_string(&res.config_path).unwrap();
        assert!(content.contains("template: npm"));
        assert!(content.contains("outputs:\n  - dist"));
    }

    #[test]
    fn test_init_already_exists_error_without_force() {
        let dir = tempdir().unwrap();
        let opts = InitOptions {
            path: dir.path().to_path_buf(),
            name: None,
            host: None,
            token: None,
            template: None,
            with_template: false,
            force: false,
        };

        init_project(&opts).unwrap();

        // Second init without force must fail
        let err = init_project(&opts).unwrap_err();
        match err {
            InitError::AlreadyExists(p) => {
                let p_canon = fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(&p));
                let dir_canon =
                    fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
                assert_eq!(p_canon, dir_canon);
            }
            _ => panic!("Expected AlreadyExists error, got {:?}", err),
        }

        // With force it should succeed
        let mut force_opts = opts.clone();
        force_opts.force = true;
        assert!(init_project(&force_opts).is_ok());
    }

    #[test]
    fn test_init_with_template_generates_template_file() {
        let dir = tempdir().unwrap();
        let opts = InitOptions {
            path: dir.path().to_path_buf(),
            name: Some("go-service".to_string()),
            host: None,
            token: None,
            template: Some("go".to_string()),
            with_template: true,
            force: false,
        };

        let res = init_project(&opts).unwrap();
        assert!(res.template_path.is_some());
        let t_path = res.template_path.unwrap();
        assert!(t_path.exists());
        assert!(t_path.ends_with(".farhand/templates/go.yaml"));

        let template_content = fs::read_to_string(&t_path).unwrap();
        assert!(template_content.contains("name: go"));
    }
}
