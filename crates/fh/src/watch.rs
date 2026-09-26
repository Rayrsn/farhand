use std::path::Path;

use fileset::IgnoreMatcher;

/// The paths watch mode must never react to, regardless of configuration.
///
/// These are the feedback-loop guards: watching our own build output would
/// rebuild forever. They are deliberately *not* configurable — a template that
/// could un-ignore `target/` would be a footgun, not a feature. Anything a
/// project wants to add goes through its template's `ignoreExtra` instead,
/// which is layered on top of these.
fn is_feedback_loop_path(rel: &Path) -> bool {
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
                || s.ends_with(".tmp")
                || s.ends_with('~')
                || s.starts_with(".#")
                || s == ".DS_Store"
            {
                return true;
            }
        }
    }
    false
}

/// Determines whether a filesystem event path within a project should be ignored.
///
/// `extra` carries the project's own ignore patterns (the `ignoreExtra` of
/// every matched template). It is passed in already-built rather than a slice
/// so the glob matcher is compiled once, not per filesystem event — watch mode
/// calls this for every event the OS reports.
pub fn should_ignore_path(path: &Path, base_dir: &Path, extra: &IgnoreMatcher) -> bool {
    let rel = match path.strip_prefix(base_dir) {
        Ok(r) => r,
        Err(_) => path,
    };
    if rel.as_os_str().is_empty() {
        return true;
    }

    let rel_wire = protocol::to_wire_path(rel);
    if is_feedback_loop_path(rel) {
        return true;
    }
    extra.should_ignore(&rel_wire, rel.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn matcher(patterns: &[&str]) -> IgnoreMatcher {
        let mut m = IgnoreMatcher::new();
        m.add_patterns(
            &patterns
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>(),
        );
        m
    }

    #[test]
    fn test_should_ignore_path() {
        let base = PathBuf::from("/home/user/project");
        let none = matcher(&[]);

        // Normal source files should not be ignored
        assert!(!should_ignore_path(&base.join("src/main.rs"), &base, &none));
        assert!(!should_ignore_path(&base.join("Cargo.toml"), &base, &none));
        assert!(!should_ignore_path(&base.join("README.md"), &base, &none));

        // Ignored directories
        assert!(should_ignore_path(&base.join(".git/index"), &base, &none));
        assert!(should_ignore_path(
            &base.join("target/debug/app"),
            &base,
            &none
        ));
        assert!(should_ignore_path(
            &base.join("node_modules/express/index.js"),
            &base,
            &none
        ));
        assert!(should_ignore_path(
            &base.join(".venv/bin/python"),
            &base,
            &none
        ));
        assert!(should_ignore_path(
            &base.join("farhand-out/app"),
            &base,
            &none
        ));
        assert!(should_ignore_path(
            &base.join("dist/bundle.js"),
            &base,
            &none
        ));
        assert!(should_ignore_path(
            &base.join(".farhand/cache.db"),
            &base,
            &none
        ));
        assert!(should_ignore_path(
            &base.join(".farhand.yaml"),
            &base,
            &none
        ));

        // Outside base dir
        assert!(!should_ignore_path(
            &PathBuf::from("/tmp/other.txt"),
            &base,
            &none
        ));

        // Base dir itself should be ignored
        assert!(should_ignore_path(&base, &base, &none));

        // Temporary editor files should be ignored
        assert!(should_ignore_path(
            &base.join("src/main.rs.tmp"),
            &base,
            &none
        ));
        assert!(should_ignore_path(&base.join("src/main.rs~"), &base, &none));
        assert!(should_ignore_path(&base.join(".DS_Store"), &base, &none));
    }

    #[test]
    fn template_ignore_extra_is_layered_on_top() {
        let base = PathBuf::from("/home/user/project");
        // A monorepo template that says "don't watch the vendored tree".
        let extra = matcher(&["vendor/**", "*.snap"]);

        assert!(
            should_ignore_path(&base.join("vendor/lib/x.js"), &base, &extra),
            "a template's ignoreExtra should suppress that path"
        );
        assert!(should_ignore_path(
            &base.join("tests/a.snap"),
            &base,
            &extra
        ));
        assert!(
            !should_ignore_path(&base.join("src/main.rs"), &base, &extra),
            "unrelated source must still trigger a rebuild"
        );
    }

    #[test]
    fn template_cannot_unignore_a_feedback_loop_path() {
        let base = PathBuf::from("/home/user/project");
        // Even a negated pattern in a template must not un-ignore `target`,
        // or watch mode would rebuild on its own output forever.
        let extra = matcher(&["!target"]);
        assert!(should_ignore_path(
            &base.join("target/debug/app"),
            &base,
            &extra
        ));
    }
}
