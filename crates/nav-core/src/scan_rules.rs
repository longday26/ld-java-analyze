//! Lightweight `.gitignore`-style rules for workspace scanning.
//!
//! Rules match **names**, not full paths: a directory component or a file name.
//! This keeps configuration predictable for multi-module Maven layouts where
//! the same `target/`, `generated/` or `*-generated.java` appears many times.
//!
//! Syntax (one rule per line):
//!
//! ```text
//! # comment
//! generated/            directory named `generated` (trailing slash)
//! dir:build*            directories matching `build*`
//! file:*.generated.java files matching `*.generated.java`
//! *Test.java            both directories and files named `*Test.java`
//! !UserServiceTest.java re-include (last matching rule wins)
//! ```
//!
//! `*` and `?` are supported; matching is case-insensitive so the same config
//! works on Windows and Linux. Path separators are deliberately not supported.

use serde::{Deserialize, Serialize};

/// Cannot be re-included by `!`: indexing our own cache would feed generated
/// library sources back into the workspace index.
pub const ALWAYS_EXCLUDED_DIRS: &[&str] = &[".ld-java-analyze"];

/// Applied unless `use_builtin_excludes` is disabled.
pub const BUILTIN_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".idea",
    ".vscode",
    ".gradle",
    "target",
    "build",
    "out",
    "node_modules",
    "dist",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleTarget {
    Any,
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludeRule {
    pub pattern: String,
    pub target: RuleTarget,
    pub negated: bool,
}

/// Effective, parsed rule set used by every directory walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRules {
    pub rules: Vec<ExcludeRule>,
    pub use_builtin: bool,
}

impl Default for ScanRules {
    fn default() -> Self {
        Self::new(&[], true)
    }
}

impl ScanRules {
    pub fn new(lines: &[String], use_builtin: bool) -> Self {
        let rules = lines.iter().filter_map(|line| parse_line(line)).collect();
        Self { rules, use_builtin }
    }

    pub fn from_text(text: &str, use_builtin: bool) -> Self {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        Self::new(&lines, use_builtin)
    }

    /// Rules from configuration, then rules from a `.ld-java-analyze/ignore` file.
    pub fn combined(config_lines: &[String], file_text: Option<&str>, use_builtin: bool) -> Self {
        let mut lines = config_lines.to_vec();
        if let Some(text) = file_text {
            lines.extend(text.lines().map(str::to_string));
        }
        Self::new(&lines, use_builtin)
    }

    pub fn is_excluded_dir(&self, name: &str) -> bool {
        self.is_excluded(name, RuleTarget::Directory)
    }

    pub fn is_excluded_file(&self, name: &str) -> bool {
        self.is_excluded(name, RuleTarget::File)
    }

    pub fn is_excluded(&self, name: &str, target: RuleTarget) -> bool {
        if target == RuleTarget::Directory
            && ALWAYS_EXCLUDED_DIRS
                .iter()
                .any(|blocked| blocked.eq_ignore_ascii_case(name))
        {
            return true;
        }
        let mut excluded = self.use_builtin
            && target == RuleTarget::Directory
            && BUILTIN_EXCLUDED_DIRS
                .iter()
                .any(|builtin| builtin.eq_ignore_ascii_case(name));
        for rule in &self.rules {
            if rule.target != RuleTarget::Any && rule.target != target {
                continue;
            }
            if wildcard_match(&rule.pattern, name) {
                excluded = !rule.negated;
            }
        }
        excluded
    }

    pub fn patterns(&self) -> Vec<String> {
        self.rules
            .iter()
            .map(|rule| {
                let prefix = match rule.target {
                    RuleTarget::Any => "",
                    RuleTarget::Directory => "dir:",
                    RuleTarget::File => "file:",
                };
                format!(
                    "{}{}{}",
                    if rule.negated { "!" } else { "" },
                    prefix,
                    rule.pattern
                )
            })
            .collect()
    }
}

fn parse_line(line: &str) -> Option<ExcludeRule> {
    let mut value = line.trim();
    if value.is_empty() || value.starts_with('#') {
        return None;
    }
    let negated = value.starts_with('!');
    if negated {
        value = value[1..].trim_start();
    }
    let lower = value.to_ascii_lowercase();
    let (target, mut pattern) = if lower.starts_with("dir:") {
        (RuleTarget::Directory, &value[4..])
    } else if lower.starts_with("file:") {
        (RuleTarget::File, &value[5..])
    } else if let Some(rest) = value.strip_suffix('/') {
        (RuleTarget::Directory, rest)
    } else {
        (RuleTarget::Any, value)
    };
    pattern = pattern.trim().trim_start_matches('/');
    if pattern.is_empty() {
        return None;
    }
    Some(ExcludeRule {
        pattern: pattern.to_string(),
        target,
        negated,
    })
}

/// Case-insensitive glob supporting `*`, `?` with a small dynamic program.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let name: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let mut previous = vec![false; name.len() + 1];
    previous[0] = true;
    for &token in &pattern {
        let mut current = vec![false; name.len() + 1];
        if token == '*' {
            current[0] = previous[0];
        }
        for index in 1..=name.len() {
            current[index] = match token {
                '*' => current[index - 1] || previous[index],
                '?' => previous[index - 1],
                literal => previous[index - 1] && literal == name[index - 1],
            };
        }
        previous = current;
    }
    previous[name.len()]
}

fn default_true() -> bool {
    true
}

/// Everything the LSP client can configure about workspace scanning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanConfig {
    /// Restrict `references` results; empty means the whole workspace.
    pub reference_roots: Vec<String>,
    /// Raw `.gitignore`-style lines.
    pub exclude_rules: Vec<String>,
    /// Keep the built-in directory excludes (`.git`, `target`, ...).
    pub use_builtin_excludes: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            reference_roots: Vec::new(),
            exclude_rules: Vec::new(),
            use_builtin_excludes: default_true(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn trailing_slash_targets_directories_only() {
        let rules = ScanRules::new(&lines(&["generated/", "file:*.generated.java"]), false);
        assert!(rules.is_excluded_dir("generated"));
        assert!(!rules.is_excluded_file("generated"));
        assert!(rules.is_excluded_file("User.generated.java"));
        assert!(!rules.is_excluded_dir("User.generated.java"));

        // Unprefixed rules apply to both directory and file names.
        let both = ScanRules::new(&lines(&["*Test.java"]), false);
        assert!(both.is_excluded_file("UserTest.java"));
        assert!(both.is_excluded_dir("UserTest.java"));
    }

    #[test]
    fn explicit_dir_and_file_prefixes() {
        let rules = ScanRules::new(&lines(&["dir:build*", "file:*Test.java"]), false);
        assert!(rules.is_excluded_dir("build-output"));
        assert!(!rules.is_excluded_file("build-output"));
        assert!(rules.is_excluded_file("UserServiceTest.java"));
        assert!(!rules.is_excluded_dir("UserServiceTest.java"));
    }

    #[test]
    fn last_matching_rule_wins_and_negation_reincludes() {
        let rules = ScanRules::new(
            &lines(&["file:*Test.java", "!file:UserServiceTest.java"]),
            false,
        );
        assert!(rules.is_excluded_file("OtherTest.java"));
        assert!(!rules.is_excluded_file("UserServiceTest.java"));
    }

    #[test]
    fn builtin_excludes_can_be_disabled_but_state_dir_cannot() {
        let with_builtin = ScanRules::new(&[], true);
        assert!(with_builtin.is_excluded_dir("target"));
        let without_builtin = ScanRules::new(&[], false);
        assert!(!without_builtin.is_excluded_dir("target"));
        assert!(without_builtin.is_excluded_dir(".ld-java-analyze"));
        let negated = ScanRules::new(&lines(&["!target", "!.ld-java-analyze"]), false);
        assert!(negated.is_excluded_dir(".ld-java-analyze"));
    }

    #[test]
    fn matching_is_case_insensitive_and_wildcards_work() {
        let rules = ScanRules::new(&lines(&["Dir:GEN?RATED"]), false);
        assert!(rules.is_excluded_dir("Generated"));
        assert!(!rules.is_excluded_dir("GeneratedExtra"));
        let rules = ScanRules::new(&lines(&["file:*.generated.java"]), false);
        assert!(rules.is_excluded_file("USER.Generated.JAVA"));
        assert!(!rules.is_excluded_file("User.java"));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let rules = ScanRules::from_text("# comment\n\n  \nfile:*.tmp\n", false);
        assert_eq!(rules.rules.len(), 1);
        assert_eq!(rules.patterns(), vec!["file:*.tmp".to_string()]);
    }

    #[test]
    fn combined_rules_let_file_override_configuration() {
        let config = lines(&["file:*Test.java"]);
        let rules = ScanRules::combined(&config, Some("!file:KeepTest.java\n"), false);
        assert!(rules.is_excluded_file("OtherTest.java"));
        assert!(!rules.is_excluded_file("KeepTest.java"));
    }
}
