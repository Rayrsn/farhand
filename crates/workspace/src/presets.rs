use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub markers: &'static [&'static str],
    pub outputs: &'static [&'static str],
}

pub const DEFAULT_PRESETS: &[Preset] = &[
    Preset {
        name: "npm",
        markers: &["package.json"],
        outputs: &["dist", "build", ".next", "out"],
    },
    Preset {
        name: "rust",
        markers: &["Cargo.toml"],
        outputs: &["target/release", "target/debug"],
    },
    Preset {
        name: "go",
        markers: &["go.mod"],
        outputs: &["bin", "dist"],
    },
    Preset {
        name: "python",
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        outputs: &["dist", "build"],
    },
];

/// Detect candidate output paths based on project marker files present in `workspace_root`.
pub fn detect_preset_outputs(workspace_root: &Path) -> Vec<String> {
    let mut candidates = Vec::new();
    for preset in DEFAULT_PRESETS {
        let matched = preset
            .markers
            .iter()
            .any(|marker| workspace_root.join(marker).exists());
        if matched {
            for output in preset.outputs {
                candidates.push((*output).to_string());
            }
        }
    }
    candidates
}

/// Resolve artifact paths to be retrieved from the remote workspace.
///
/// 1. If explicit `requested_outputs` is provided and non-empty, use those.
/// 2. Otherwise, detect candidate outputs using project marker presets.
/// 3. Filter and expand candidates (directories, files, or glob patterns) to existing paths
///    relative to `workspace_root`.
/// 4. All paths use wire-normalized forward slashes and prevent directory traversal.
pub fn resolve_artifact_paths(
    workspace_root: &Path,
    requested_outputs: Option<&[String]>,
    explicit_template: Option<&str>,
) -> Vec<String> {
    let (candidates, output_ignores): (Vec<String>, Vec<String>) = match requested_outputs {
        Some(outs) if !outs.is_empty() => (outs.to_vec(), Vec::new()),
        _ => {
            let tmpl_candidates =
                templates::resolve_template_outputs(workspace_root, explicit_template);
            let tmpl_ignores =
                templates::resolve_template_outputs_ignores(workspace_root, explicit_template);
            if !tmpl_candidates.is_empty() {
                (tmpl_candidates, tmpl_ignores)
            } else {
                (detect_preset_outputs(workspace_root), Vec::new())
            }
        }
    };

    let canonical_root = match workspace_root.canonicalize() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut resolved_paths = Vec::new();

    for candidate in candidates {
        let clean = candidate.trim().trim_matches('/');
        // Security check: reject empty or path traversal sequences
        if clean.is_empty()
            || clean.starts_with("..")
            || clean.contains("/../")
            || clean.contains("\\..\\")
            || clean.contains('\\')
        {
            continue;
        }

        // Check if candidate is a glob pattern
        if clean.contains('*') || clean.contains('?') || clean.contains('[') {
            let pattern_str = format!("{}/{}", canonical_root.to_string_lossy(), clean);
            if let Ok(entries) = glob::glob(&pattern_str) {
                for entry in entries.flatten() {
                    add_path_or_dir(
                        &canonical_root,
                        &entry,
                        &output_ignores,
                        &mut resolved_paths,
                    );
                }
            }
        } else {
            let candidate_path = canonical_root.join(clean);
            if candidate_path.exists() {
                add_path_or_dir(
                    &canonical_root,
                    &candidate_path,
                    &output_ignores,
                    &mut resolved_paths,
                );
            }
        }
    }

    resolved_paths.sort();
    resolved_paths.dedup();
    resolved_paths
}

fn is_artifact_ignored(rel_str: &str, output_ignores: &[String]) -> bool {
    let clean = rel_str.trim().trim_matches('/');
    if clean.is_empty() {
        return false;
    }

    // Check path segments against built-in intermediate compiler caches
    let segments: Vec<&str> = clean.split('/').collect();
    for seg in &segments {
        if matches!(
            *seg,
            "deps" | "incremental" | ".fingerprint" | "__pycache__" | ".git" | "node_modules"
        ) {
            return true;
        }
    }

    // Check against template outputsIgnore patterns
    for pattern in output_ignores {
        let pat_clean = pattern.trim().trim_matches('/');
        if pat_clean.contains('*') || pat_clean.contains('?') || pat_clean.contains('[') {
            if let Ok(glob_pat) = glob::Pattern::new(pat_clean) {
                if glob_pat.matches(clean) {
                    return true;
                }
                if let Some(file_name) = clean.rsplit('/').next() {
                    if glob_pat.matches(file_name) {
                        return true;
                    }
                }
            }
        } else if clean == pat_clean
            || clean.starts_with(&format!("{}/", pat_clean))
            || clean.ends_with(&format!("/{}", pat_clean))
        {
            return true;
        }
    }

    false
}

fn add_path_or_dir(
    canonical_root: &Path,
    target: &Path,
    output_ignores: &[String],
    out: &mut Vec<String>,
) {
    let canonical_target = match target.canonicalize() {
        Ok(c) => c,
        Err(_) => return,
    };

    // Prevent Zip-Slip: ensure target stays strictly within canonical_root
    if !canonical_target.starts_with(canonical_root) || canonical_target == canonical_root {
        return;
    }

    if target.is_dir() {
        let mut it = WalkDir::new(target).into_iter();
        loop {
            let entry = match it.next() {
                Some(Ok(e)) => e,
                Some(Err(_)) => continue,
                None => break,
            };

            // Verify entry canonical path also stays within root
            if let Ok(canon_entry) = entry.path().canonicalize() {
                if !canon_entry.starts_with(canonical_root) {
                    continue;
                }
            }

            if let Ok(rel) = entry.path().strip_prefix(canonical_root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if rel_str.is_empty() {
                    continue;
                }

                if is_artifact_ignored(&rel_str, output_ignores) {
                    if entry.file_type().is_dir() {
                        it.skip_current_dir();
                    }
                    continue;
                }

                out.push(rel_str);
            }
        }
    } else if let Ok(rel) = canonical_target.strip_prefix(canonical_root) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !rel_str.is_empty() && !is_artifact_ignored(&rel_str, output_ignores) {
            out.push(rel_str);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_detect_preset_outputs_rust_and_npm() {
        let dir = tempdir().unwrap();
        // Initially empty: no candidates
        assert!(detect_preset_outputs(dir.path()).is_empty());

        // Add Cargo.toml
        File::create(dir.path().join("Cargo.toml")).unwrap();
        let detected = detect_preset_outputs(dir.path());
        assert!(detected.contains(&"target/release".to_string()));
        assert!(detected.contains(&"target/debug".to_string()));

        // Add package.json
        File::create(dir.path().join("package.json")).unwrap();
        let detected = detect_preset_outputs(dir.path());
        assert!(detected.contains(&"dist".to_string()));
        assert!(detected.contains(&"target/release".to_string()));
    }

    #[test]
    fn test_resolve_artifact_paths_explicit_files_and_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create dist/bundle.js and dist/sub/style.css
        let dist = root.join("dist");
        let sub = dist.join("sub");
        fs::create_dir_all(&sub).unwrap();
        File::create(dist.join("bundle.js"))
            .unwrap()
            .write_all(b"console.log('hi');")
            .unwrap();
        File::create(sub.join("style.css"))
            .unwrap()
            .write_all(b"body { margin: 0; }")
            .unwrap();

        // Explicitly request "dist"
        let paths = resolve_artifact_paths(root, Some(&["dist".to_string()]), None);
        assert!(paths.contains(&"dist".to_string()));
        assert!(paths.contains(&"dist/bundle.js".to_string()));
        assert!(paths.contains(&"dist/sub".to_string()));
        assert!(paths.contains(&"dist/sub/style.css".to_string()));
    }

    #[test]
    fn test_resolve_artifact_paths_preset_fallback() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create marker Cargo.toml and output target/release/my-bin
        File::create(root.join("Cargo.toml")).unwrap();
        let release_dir = root.join("target").join("release");
        fs::create_dir_all(&release_dir).unwrap();
        File::create(release_dir.join("my-bin")).unwrap();

        // Pass None for requested_outputs; should detect target/release
        let paths = resolve_artifact_paths(root, None, None);
        assert!(paths.contains(&"target/release".to_string()));
        assert!(paths.contains(&"target/release/my-bin".to_string()));
        // target/debug was not created, so it should not be in paths
        assert!(!paths.contains(&"target/debug".to_string()));
    }

    #[test]
    fn test_resolve_artifact_paths_glob() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let out = root.join("out");
        fs::create_dir_all(&out).unwrap();
        File::create(out.join("test1.log")).unwrap();
        File::create(out.join("test2.log")).unwrap();
        File::create(out.join("other.txt")).unwrap();

        let paths = resolve_artifact_paths(root, Some(&["out/*.log".to_string()]), None);
        assert_eq!(paths, vec!["out/test1.log", "out/test2.log"]);
    }

    #[test]
    fn test_resolve_artifact_paths_traversal_rejection() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Pass malicious paths
        let paths = resolve_artifact_paths(
            root,
            Some(&[
                "../outside".to_string(),
                "../../etc/passwd".to_string(),
                "out/../../escaped".to_string(),
            ]),
            None,
        );
        assert!(paths.is_empty());
    }

    #[test]
    fn test_resolve_artifact_paths_nonexistent() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let paths = resolve_artifact_paths(root, Some(&["does_not_exist".to_string()]), None);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_resolve_artifact_paths_template_integration() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create package.json and dist/app.js
        File::create(root.join("package.json")).unwrap();
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();
        File::create(dist.join("app.js")).unwrap();

        let paths = resolve_artifact_paths(root, None, None);
        assert!(paths.contains(&"dist".to_string()));
        assert!(paths.contains(&"dist/app.js".to_string()));
    }

    #[test]
    fn test_resolve_artifact_paths_excludes_intermediate_compiler_caches() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create Cargo.toml to trigger rust template
        File::create(root.join("Cargo.toml")).unwrap();
        let release_dir = root.join("target").join("release");
        let deps_dir = release_dir.join("deps");
        let build_dir = release_dir.join("build");
        let inc_dir = release_dir.join("incremental");
        fs::create_dir_all(&deps_dir).unwrap();
        fs::create_dir_all(&build_dir).unwrap();
        fs::create_dir_all(&inc_dir).unwrap();

        // Output binary (should be retrieved)
        File::create(release_dir.join("app-bin")).unwrap();
        // Intermediate caches (should be EXCLUDED)
        File::create(deps_dir.join("libfoo.rlib")).unwrap();
        File::create(build_dir.join("build-script.o")).unwrap();
        File::create(inc_dir.join("inc.bin")).unwrap();
        File::create(release_dir.join("app-bin.d")).unwrap();

        let paths = resolve_artifact_paths(root, None, None);
        assert!(paths.contains(&"target/release/app-bin".to_string()));
        assert!(!paths.contains(&"target/release/deps/libfoo.rlib".to_string()));
        assert!(!paths.contains(&"target/release/build/build-script.o".to_string()));
        assert!(!paths.contains(&"target/release/incremental/inc.bin".to_string()));
        assert!(!paths.contains(&"target/release/app-bin.d".to_string()));
    }
}
