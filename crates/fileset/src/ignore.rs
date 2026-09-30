use glob::Pattern;
use std::path::Path;

pub const DEFAULT_IGNORES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    "out",
    "__pycache__",
    ".venv",
    "venv",
    "vendor",
    ".DS_Store",
    ".farhand-state.json",
    ".farhand-runs",
    ".farhand-last-used",
];

/// Returns true if any component of the forward-slash relative path matches a default ignore.
pub fn is_default_ignored(rel_path: &str) -> bool {
    let clean = rel_path.trim_matches('/');
    for part in clean.split('/') {
        if part.is_empty() {
            continue;
        }
        for &ignored in DEFAULT_IGNORES {
            if part == ignored {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct IgnoreRule {
    pub pattern: Pattern,
    pub dir_only: bool,
    pub negated: bool,
    pub match_full_path: bool,
}

#[derive(Debug, Clone, Default)]
pub struct IgnoreMatcher {
    rules: Vec<IgnoreRule>,
}

impl IgnoreMatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load ignore rules from a file path if it exists.
    pub fn load_file(&mut self, path: &Path) {
        if let Ok(content) = std::fs::read_to_string(path) {
            self.parse_and_add_lines(&content);
        }
    }

    /// Add a slice of pattern strings (e.g. from templates or CLI).
    pub fn add_patterns(&mut self, patterns: &[String]) {
        for pat in patterns {
            self.parse_and_add_line(pat);
        }
    }

    pub fn parse_and_add_lines(&mut self, content: &str) {
        for line in content.lines() {
            self.parse_and_add_line(line);
        }
    }

    pub fn parse_and_add_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return;
        }

        let (negated, rule_str) = if let Some(stripped) = trimmed.strip_prefix('!') {
            (true, stripped.trim())
        } else {
            (false, trimmed)
        };

        let dir_only = rule_str.ends_with('/');
        let clean_rule = rule_str.trim_end_matches('/');

        // If it starts with '/', it anchors to root
        let clean_rule = clean_rule.strip_prefix('/').unwrap_or(clean_rule);

        let match_full_path = clean_rule.contains('/');

        if let Ok(pattern) = Pattern::new(clean_rule) {
            self.rules.push(IgnoreRule {
                pattern,
                dir_only,
                negated,
                match_full_path,
            });
        }
    }

    /// Evaluates if a relative path (using forward slashes) should be ignored.
    pub fn should_ignore(&self, rel_path: &str, is_dir: bool) -> bool {
        let clean_path = rel_path.trim_matches('/');

        // 1. Default ignores always take priority
        if is_default_ignored(clean_path) {
            return true;
        }

        let basename = clean_path.split('/').next_back().unwrap_or(clean_path);

        let mut ignored = false;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }

            let matches = if rule.match_full_path {
                rule.pattern.matches(clean_path)
            } else {
                rule.pattern.matches(basename) || rule.pattern.matches(clean_path)
            };

            if matches {
                ignored = !rule.negated;
            }
        }

        ignored
    }

    /// The pattern that decided the outcome for `rel_path`, if any.
    ///
    /// Mirrors [`IgnoreMatcher::should_ignore`] rule for rule — same ordering,
    /// same negation handling — so the pattern reported is the one that
    /// actually set the result, not merely the first that matched. Returns
    /// `None` when no rule applies, or when only the built-in ignore list
    /// matched (callers report that case separately).
    pub fn matching_rule(&self, rel_path: &str, is_dir: bool) -> Option<String> {
        let clean_path = rel_path.trim_matches('/');
        if is_default_ignored(clean_path) {
            return None;
        }
        let basename = clean_path.split('/').next_back().unwrap_or(clean_path);

        let mut decided: Option<String> = None;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            let matches = if rule.match_full_path {
                rule.pattern.matches(clean_path)
            } else {
                rule.pattern.matches(basename) || rule.pattern.matches(clean_path)
            };
            if matches {
                // A negation re-includes rather than excludes, so it must
                // *clear* any earlier decision rather than be skipped: the
                // last matching rule is the one that decides. Leaving the
                // prior rule in place made `fh why` blame `*.log` for
                // excluding a file `!important.log` had re-included.
                decided = if rule.negated {
                    None
                } else {
                    Some(rule.pattern.as_str().to_string())
                };
            }
        }
        decided
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_ignores() {
        assert!(is_default_ignored("node_modules"));
        assert!(is_default_ignored("node_modules/express/index.js"));
        assert!(is_default_ignored("foo/bar/target/release/app"));
        assert!(is_default_ignored(".git/HEAD"));
        assert!(is_default_ignored(".venv/bin/python"));
        assert!(is_default_ignored(".DS_Store"));

        assert!(!is_default_ignored("src/main.rs"));
        assert!(!is_default_ignored("tests/fixtures.rs"));
        assert!(!is_default_ignored("target_info.txt"));
    }

    #[test]
    fn test_custom_patterns() {
        let mut matcher = IgnoreMatcher::new();
        matcher.parse_and_add_lines(
            r#"
            # Comment
            *.log
            temp/
            secret.txt
            !important.log
            /root_only.txt
            build/**/*.tmp
            "#,
        );

        // *.log
        assert!(matcher.should_ignore("error.log", false));
        assert!(matcher.should_ignore("deep/nested/debug.log", false));

        // !important.log (negation)
        assert!(!matcher.should_ignore("important.log", false));

        // temp/ (dir only)
        assert!(matcher.should_ignore("temp", true));
        assert!(matcher.should_ignore("sub/temp", true));
        assert!(!matcher.should_ignore("temp", false)); // File named temp is not ignored

        // secret.txt
        assert!(matcher.should_ignore("secret.txt", false));
        assert!(matcher.should_ignore("nested/secret.txt", false));

        // root_only.txt
        assert!(matcher.should_ignore("root_only.txt", false));

        // Non-ignored files
        assert!(!matcher.should_ignore("src/lib.rs", false));
    }
}

#[cfg(test)]
mod negation_explanation_tests {
    use super::*;

    /// `should_ignore` handles negation; `matching_rule` did not, so a
    /// re-included path could be explained as excluded. The common path in
    /// `fh why` masked it by checking the scan first, which is exactly why it
    /// survived — but the primitive was a trap for the next caller.
    #[test]
    fn a_negated_rule_is_never_reported_as_the_reason_for_excluding() {
        let mut m = IgnoreMatcher::new();
        m.add_patterns(&["*.log".to_string(), "!important.log".to_string()]);

        assert!(m.should_ignore("debug.log", false));
        assert!(!m.should_ignore("important.log", false));

        // The excluded file names the rule that excluded it.
        assert_eq!(
            m.matching_rule("debug.log", false).as_deref(),
            Some("*.log"),
            "the wrong rule was blamed"
        );
        // The re-included file is not excluded, so no rule may claim to exclude it.
        assert_eq!(
            m.matching_rule("important.log", false),
            None,
            "a negated rule was reported as an exclusion"
        );
    }

    #[test]
    fn should_ignore_and_the_explanation_agree_on_negation() {
        let mut m = IgnoreMatcher::new();
        // Not a built-in default: `dist` would short-circuit in
        // is_default_ignored before the rule machinery is ever consulted.
        m.add_patterns(&["scratch".to_string(), "!scratch/keep.txt".to_string()]);

        for path in ["scratch/app.js", "scratch/keep.txt", "src/main.rs"] {
            assert_eq!(
                m.should_ignore(path, false),
                m.matching_rule(path, false).is_some(),
                "should_ignore and matching_rule disagree about {path:?}"
            );
        }
    }
}
