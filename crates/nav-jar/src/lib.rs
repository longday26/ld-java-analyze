//! Library (JAR) navigation: source-first, multiple candidates, offline fallback.
//!
//! Hard constraints from the design: no source downloads, no Maven invocation and
//! no network access. Only jars that already exist locally are considered. When
//! several jars contain the same class, every candidate is returned so the editor
//! can show a picker. Without a `*-sources.jar`, a bytecode structure view is
//! generated under `<workspace>/.ld-java-analyze/cache` as a navigable text file.

pub mod classfile;

use nav_core::{file_uri, ProjectContext};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::ZipArchive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// A `*-sources.jar` entry: the preferred destination.
    Source,
    /// Only a `.class` entry exists; a structure view is generated.
    Structure,
}

#[derive(Debug, Clone)]
pub struct JarCandidate {
    pub jar: PathBuf,
    pub kind: CandidateKind,
    pub entry: String,
    pub fqn: String,
    pub member: Option<String>,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct MaterializedSource {
    /// A real file below `<workspace>/.ld-java-analyze/cache`, so any editor can open it.
    pub path: PathBuf,
    pub uri: String,
    pub line: u32,
    pub label: String,
}

/// Extension point for a real decompiler (e.g. `jcdc`) in a later phase.
pub trait Decompiler: Send + Sync {
    fn id(&self) -> &'static str;
    fn decompile(&self, class_bytes: &[u8]) -> Option<String>;
}

/// Default fallback: readable outline built from the class file tables.
pub struct StructureViewDecompiler;

impl Decompiler for StructureViewDecompiler {
    fn id(&self) -> &'static str {
        "structure-view"
    }

    fn decompile(&self, class_bytes: &[u8]) -> Option<String> {
        classfile::outline(class_bytes).map(|outline| classfile::render(&outline, "bytecode"))
    }
}

#[derive(Debug, Default)]
pub struct JarIndex {
    sources: Vec<PathBuf>,
    compiled: Vec<PathBuf>,
    /// Jar entry names loaded once at discovery time, so candidate lookups never
    /// reopen every zip on the classpath on each Ctrl+Click.
    entries: HashMap<PathBuf, HashSet<String>>,
}

impl JarIndex {
    /// Discovers jars from the user-generated classpath and the workspace scan.
    /// Also checks for sibling `-sources.jar` files next to compiled jars.
    pub fn scan(ctx: &ProjectContext) -> Self {
        let mut sources = Vec::new();
        let mut compiled = Vec::new();
        for jar in &ctx.classpath {
            if !jar.is_file() {
                continue;
            }
            let name = jar
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if name.ends_with("-sources.jar") {
                push_unique(&mut sources, jar);
                continue;
            }
            push_unique(&mut compiled, jar);
            if let Some(parent) = jar.parent() {
                let stem = name.strip_suffix(".jar").unwrap_or(name);
                let sibling = parent.join(format!("{stem}-sources.jar"));
                if sibling.is_file() {
                    push_unique(&mut sources, &sibling);
                }
            }
        }
        for jar in &ctx.source_jars {
            if jar.is_file() {
                push_unique(&mut sources, jar);
            }
        }
        let mut index = Self {
            sources,
            compiled,
            entries: HashMap::new(),
        };
        let jars: Vec<PathBuf> = index
            .sources
            .iter()
            .chain(&index.compiled)
            .cloned()
            .collect();
        for jar in &jars {
            let names = entry_names(jar);
            index.entries.insert(jar.clone(), names);
        }
        index
    }

    fn has_entry(&self, jar: &Path, entry: &str) -> bool {
        self.entries
            .get(jar)
            .is_some_and(|names| names.contains(entry))
    }

    pub fn source_jars(&self) -> &[PathBuf] {
        &self.sources
    }

    pub fn compiled_jars(&self) -> &[PathBuf] {
        &self.compiled
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.compiled.is_empty()
    }

    /// All jars containing `fqn`, sources first. `member` is used to place the
    /// caret on the requested method in the generated view.
    pub fn candidates(&self, fqn: &str, member: Option<&str>) -> Vec<JarCandidate> {
        let path = fqn.replace('.', "/");
        let java_entry = format!("{path}.java");
        let class_entry = format!("{path}.class");
        let mut out = Vec::new();
        for jar in &self.sources {
            if self.has_entry(jar, &java_entry) {
                out.push(candidate(
                    jar,
                    CandidateKind::Source,
                    java_entry.clone(),
                    fqn,
                    member,
                ));
            }
        }
        for jar in &self.compiled {
            if self.has_entry(jar, &class_entry) {
                out.push(candidate(
                    jar,
                    CandidateKind::Structure,
                    class_entry.clone(),
                    fqn,
                    member,
                ));
            }
        }
        out
    }

    /// Writes (or reuses) a real file under `<state_dir>/cache` and returns its URI.
    pub fn materialize(
        &self,
        candidate: &JarCandidate,
        state_dir: &Path,
    ) -> Option<MaterializedSource> {
        self.materialize_with(candidate, state_dir, &StructureViewDecompiler)
    }

    pub fn materialize_with(
        &self,
        candidate: &JarCandidate,
        state_dir: &Path,
        decompiler: &dyn Decompiler,
    ) -> Option<MaterializedSource> {
        let bytes = read_entry(&candidate.jar, &candidate.entry)?;
        let safe = cache_key(&candidate.jar);
        let relative = candidate.entry.replace('/', std::path::MAIN_SEPARATOR_STR);
        let (subdir, filename, content) = match candidate.kind {
            CandidateKind::Source => (
                "sources",
                relative,
                String::from_utf8_lossy(&bytes).into_owned(),
            ),
            CandidateKind::Structure => {
                let rendered = decompiler.decompile(&bytes)?;
                let filename = format!(
                    "{}.{}.txt",
                    candidate.fqn.replace('.', "_"),
                    decompiler.id()
                );
                ("classes", filename, rendered)
            }
        };
        let path = state_dir
            .join("cache")
            .join(subdir)
            .join(safe)
            .join(filename);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok()?;
        }
        if !path.exists() {
            fs::write(&path, &content).ok()?;
        }
        let line = match candidate.member.as_deref() {
            Some(member) => member_line(&content, member),
            None if candidate.kind == CandidateKind::Source => {
                let simple = candidate.fqn.rsplit('.').next().unwrap_or(&candidate.fqn);
                type_declaration_line(&content, simple)
            }
            None => 0,
        };
        Some(MaterializedSource {
            uri: file_uri(&path),
            path,
            line,
            label: candidate.label.clone(),
        })
    }
}

fn candidate(
    jar: &Path,
    kind: CandidateKind,
    entry: String,
    fqn: &str,
    member: Option<&str>,
) -> JarCandidate {
    let jar_name = jar
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("?")
        .to_string();
    let label = match kind {
        CandidateKind::Source => format!("{fqn} — 源码 — {jar_name}"),
        CandidateKind::Structure => format!("{fqn} — 无源码/结构视图 — {jar_name}"),
    };
    JarCandidate {
        jar: jar.to_path_buf(),
        kind,
        entry,
        fqn: fqn.to_string(),
        member: member.map(str::to_string),
        label,
    }
}

fn entry_names(jar: &Path) -> HashSet<String> {
    let Ok(file) = fs::File::open(jar) else {
        return HashSet::new();
    };
    let Ok(archive) = ZipArchive::new(file) else {
        return HashSet::new();
    };
    archive
        .file_names()
        .filter(|name| name.ends_with(".java") || name.ends_with(".class"))
        .map(str::to_string)
        .collect()
}

fn read_entry(jar: &Path, entry: &str) -> Option<Vec<u8>> {
    let file = fs::File::open(jar).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;
    let mut zip_file = archive.by_name(entry).ok()?;
    let mut bytes = Vec::new();
    zip_file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn push_unique(list: &mut Vec<PathBuf>, path: &Path) {
    if !list.iter().any(|existing| existing == path) {
        list.push(path.to_path_buf());
    }
}

fn cache_key(jar: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    jar.hash(&mut hasher);
    let stem = jar
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("jar");
    let safe: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}-{:x}", hasher.finish() & 0xffff_ffff)
}

/// Finds the `class` / `interface` / `enum` / `record` / `@interface` declaration
/// line for a type name, skipping license comments and imports.
fn type_declaration_line(text: &str, simple: &str) -> u32 {
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("import ")
        {
            continue;
        }
        for keyword in ["@interface", "class", "interface", "enum", "record"] {
            let needle = format!("{keyword} {simple}");
            let Some(at) = trimmed.find(&needle) else {
                continue;
            };
            let rest = &trimmed[at + needle.len()..];
            if rest.is_empty()
                || rest
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '{' | '<' | ':' | '(' | ';'))
            {
                return index as u32;
            }
        }
    }
    0
}

fn member_line(text: &str, member: &str) -> u32 {
    let call = format!("{member}(");
    let field = format!("{member} :");
    for (index, line) in text.lines().enumerate() {
        if line.contains(&call) || line.contains(&field) {
            return index as u32;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nav-jar-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn zip(&self, name: &str, entry: &str, content: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            let file = fs::File::create(&path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(entry, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
            writer.finish().unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn type_declaration_line_skips_license_and_imports() {
        let text = "/*\n * license\n */\npackage demo;\n\nimport java.util.List;\n\npublic @interface PostMapping {\n}\n";
        assert_eq!(type_declaration_line(text, "PostMapping"), 7);
        let enum_text = "package demo;\npublic enum HttpStatus {\n    OK,\n}\n";
        assert_eq!(type_declaration_line(enum_text, "HttpStatus"), 1);
        assert_eq!(type_declaration_line(text, "Missing"), 0);
        assert_eq!(
            member_line("    String summary() default \"\";\n", "summary"),
            0
        );
    }

    #[test]
    fn source_jar_wins_and_all_duplicate_jars_are_listed() {
        let temp = TempDir::new("candidates");
        let source = temp.zip(
            "lib-1.0-sources.jar",
            "demo/Lib.java",
            b"package demo; public class Lib { public String find(){ return \"x\"; } }",
        );
        let compiled_a = temp.zip("lib-1.0.jar", "demo/Lib.class", b"not-a-real-class");
        let compiled_b = temp.zip("lib-1.1.jar", "demo/Lib.class", b"not-a-real-class");

        let mut ctx = ProjectContext::new(&temp.0);
        ctx.classpath = vec![compiled_a.clone(), compiled_b.clone()];
        ctx.source_jars = vec![source.clone()];
        let index = JarIndex::scan(&ctx);
        let candidates = index.candidates("demo.Lib", Some("find"));

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].kind, CandidateKind::Source);
        assert_eq!(candidates[1].kind, CandidateKind::Structure);
        assert!(candidates[0].label.contains("源码"));
    }

    #[test]
    fn materialized_source_lives_inside_state_dir() {
        let temp = TempDir::new("materialize");
        let source = temp.zip(
            "lib-sources.jar",
            "demo/Lib.java",
            b"package demo;\npublic class Lib {\n    public void find() {}\n}\n",
        );
        let mut ctx = ProjectContext::new(&temp.0);
        ctx.source_jars = vec![source];
        let index = JarIndex::scan(&ctx);
        let candidate = index.candidates("demo.Lib", Some("find")).remove(0);
        let state_dir = temp.0.join(".ld-java-analyze");
        let materialized = index.materialize(&candidate, &state_dir).unwrap();

        assert!(materialized.path.starts_with(&state_dir));
        assert!(materialized.uri.starts_with("file:///"));
        assert_eq!(materialized.line, 2);
    }
}
