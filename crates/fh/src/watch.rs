use std::path::Path;

/// Determines whether a filesystem event path within a project should be ignored during watch mode.
///
/// Typical build outputs, cache directories, version control, and farhand internal outputs
/// are ignored to prevent feedback loops.
pub fn should_ignore_path(path: &Path, base_dir: &Path) -> bool {
    let rel = match path.strip_prefix(base_dir) {
        Ok(r) => r,
        Err(_) => return false,
    };
    for comp in rel.components() {
        if let std::path::Component::Normal(name) = comp {
            let s = name.to_string_lossy();
            if s == ".git"
                || s == "target"
                || s == "node_modules"
                || s == ".venv"
                || s == "farhand-out"
                || s == "dist"
                || s.starts_with(".farhand")
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_should_ignore_path() {
        let base = PathBuf::from("/home/user/project");

        // Normal source files should not be ignored
        assert!(!should_ignore_path(&base.join("src/main.rs"), &base));
        assert!(!should_ignore_path(&base.join("Cargo.toml"), &base));
        assert!(!should_ignore_path(&base.join("README.md"), &base));

        // Ignored directories
        assert!(should_ignore_path(&base.join(".git/index"), &base));
        assert!(should_ignore_path(&base.join("target/debug/app"), &base));
        assert!(should_ignore_path(
            &base.join("node_modules/express/index.js"),
            &base
        ));
        assert!(should_ignore_path(&base.join(".venv/bin/python"), &base));
        assert!(should_ignore_path(&base.join("farhand-out/app"), &base));
        assert!(should_ignore_path(&base.join("dist/bundle.js"), &base));
        assert!(should_ignore_path(&base.join(".farhand/cache.db"), &base));
        assert!(should_ignore_path(&base.join(".farhand.yaml"), &base));

        // Outside base dir
        assert!(!should_ignore_path(&PathBuf::from("/tmp/other.txt"), &base));
    }
}
