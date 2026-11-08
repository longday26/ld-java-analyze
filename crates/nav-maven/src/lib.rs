//! Read-only Maven project discovery.
//!
//! This crate never starts Maven, never downloads anything and never touches the
//! network. `classpath.txt` is produced by the user, e.g.
//! `mvn -q dependency:build-classpath -Dmdep.outputFile=.ld-java-analyze/classpath.txt`.

use nav_core::{ProjectContext, ScanRules};
use roxmltree::Document;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Whether a walk entry is excluded by the configured name rules.
pub fn is_ignored_by(entry: &DirEntry, rules: &ScanRules) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    if entry.file_type().is_dir() {
        rules.is_excluded_dir(name)
    } else {
        rules.is_excluded_file(name)
    }
}

/// Convenience wrapper using the built-in rule set.
pub fn is_ignored(entry: &DirEntry) -> bool {
    is_ignored_by(entry, &ScanRules::default())
}

/// Fills reactor modules, the user-generated classpath and locally present source jars.
pub fn discover(ctx: &mut ProjectContext) {
    ctx.modules = find_modules(&ctx.root, &ctx.scan_rules);
    ctx.classpath = read_classpath(&ctx.state_dir.join("classpath.txt"));
    ctx.source_jars = find_source_jars(&ctx.root, &ctx.scan_rules);
}

fn find_modules(root: &Path, rules: &ScanRules) -> Vec<PathBuf> {
    let mut modules = vec![root.to_path_buf()];
    for entry in WalkDir::new(root)
        .max_depth(4)
        .into_iter()
        .filter_entry(|entry| entry.path() == root || !is_ignored_by(entry, rules))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() == "pom.xml")
    {
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(document) = Document::parse(&text) else {
            continue;
        };
        for node in document
            .descendants()
            .filter(|node| node.has_tag_name("module"))
        {
            if let Some(name) = node.text() {
                let module = parent.join(name.trim());
                // A declared module must obey the same name rules as the walk.
                let relative = module.strip_prefix(root).unwrap_or(&module);
                if relative.components().any(|part| {
                    part.as_os_str()
                        .to_str()
                        .is_some_and(|component| rules.is_excluded_dir(component))
                }) {
                    continue;
                }
                if !modules.contains(&module) {
                    modules.push(module);
                }
            }
        }
    }
    modules
}

fn read_classpath(path: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    // A classpath file contains one `:` / `;` separated list on a single line.
    let separator = if cfg!(windows) { ';' } else { ':' };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .flat_map(|line| line.split(separator))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn find_source_jars(root: &Path, rules: &ScanRules) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| entry.path() == root || !is_ignored_by(entry, rules))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(DirEntry::into_path)
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("-sources.jar"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rules_cover_state_and_build_directories() {
        let rules = ScanRules::default();
        for name in [".ld-java-analyze", "target", "node_modules", ".git"] {
            assert!(
                rules.is_excluded_dir(name),
                "missing builtin rule for {name}"
            );
        }
        assert!(!rules.is_excluded_dir("src"));
    }

    #[test]
    fn custom_rules_are_honoured_by_discovery() {
        let root = std::env::temp_dir().join(format!("nav-maven-rules-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("generated")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("pom.xml"),
            "<project><modules><module>generated</module><module>src</module></modules></project>",
        )
        .unwrap();

        let mut ctx = ProjectContext::new(&root);
        discover(&mut ctx);
        assert!(ctx.modules.iter().any(|module| module == &root.join("src")));
        assert!(ctx
            .modules
            .iter()
            .any(|module| module == &root.join("generated")));

        ctx.scan_rules = ScanRules::new(&["generated/".to_string()], true);
        discover(&mut ctx);
        assert!(ctx.modules.iter().any(|module| module == &root.join("src")));
        assert!(!ctx
            .modules
            .iter()
            .any(|module| module == &root.join("generated")));

        let _ = fs::remove_dir_all(&root);
    }
}
