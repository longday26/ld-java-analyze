//! MyBatis provider: bidirectional links between Mapper interface methods and XML statements.
//!
//! This is the only framework provider in P0. It demonstrates the extension point:
//! the core index knows nothing about XML or MyBatis, it just stores `Link`s.

use nav_core::{
    file_uri, position_at, FrameworkProvider, Link, LinkEndpoint, ProjectContext, Range, Symbol,
    SymbolLookup,
};
use roxmltree::{Document, ParsingOptions};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub struct MybatisProvider;

impl FrameworkProvider for MybatisProvider {
    fn id(&self) -> &'static str {
        "mybatis"
    }

    /// Enabled by configuration; detection is deliberately cheap and local.
    fn detect(&self, _ctx: &ProjectContext) -> bool {
        true
    }

    fn contribute(
        &self,
        _ctx: &ProjectContext,
        files: &[PathBuf],
        lookup: &dyn SymbolLookup,
    ) -> Vec<Link> {
        let mut links = Vec::new();
        for path in files
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "xml"))
        {
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            // MyBatis mapper files declare a DOCTYPE. roxmltree disables DTD by
            // default; allowing it locally does not resolve external entities.
            let Ok(document) = Document::parse_with_options(
                &text,
                ParsingOptions {
                    allow_dtd: true,
                    ..Default::default()
                },
            ) else {
                continue;
            };
            // DOCTYPE nodes can share the tag name `mapper`; only real elements qualify.
            let Some(mapper) = document.descendants().find(|node| {
                node.is_element()
                    && node.has_tag_name("mapper")
                    && node.attribute("namespace").is_some()
            }) else {
                continue;
            };
            let Some(namespace) = mapper.attribute("namespace") else {
                continue;
            };
            let uri = file_uri(path);

            for statement in mapper.children().filter(|node| {
                node.is_element()
                    && matches!(
                        node.tag_name().name(),
                        "select" | "insert" | "update" | "delete"
                    )
            }) {
                let Some(id) = statement.attribute("id") else {
                    continue;
                };
                let Some(range) = locate_id(&text, id) else {
                    continue;
                };
                let xml_endpoint = LinkEndpoint {
                    uri: uri.clone(),
                    range,
                    label: format!("{namespace}.{id} (XML)"),
                };
                for method in lookup.methods_named(namespace, id) {
                    links.push(Link {
                        from: xml_endpoint.clone(),
                        to: LinkEndpoint {
                            uri: method.uri,
                            range: method.selection_range,
                            label: format!("{namespace}.{id} (Java)"),
                        },
                        kind: "mybatis.mapper-xml".to_string(),
                        bidirectional: true,
                    });
                }
            }
        }
        links
    }
}

/// Locates the `id="..."` attribute so the XML side of a link is clickable.
/// A byte search is enough here because statement ids are unique inside a mapper file.
fn locate_id(text: &str, id: &str) -> Option<Range> {
    for needle in [format!("id=\"{id}\""), format!("id='{id}'")] {
        if let Some(start) = text.find(&needle) {
            return Some(Range::new(
                position_at(text, start),
                position_at(text, start + needle.len()),
            ));
        }
    }
    None
}

/// Resolves links touching a given position, honouring `bidirectional`.
pub fn targets_at(links: &[Link], uri: &str, position: nav_core::Position) -> Vec<LinkEndpoint> {
    let mut out = Vec::new();
    for link in links {
        if link.from.uri == uri && link.from.range.contains(position) {
            out.push(link.to.clone());
        }
        if link.bidirectional && link.to.uri == uri && link.to.range.contains(position) {
            out.push(link.from.clone());
        }
    }
    out
}

/// Utility for callers that want a label -> symbol map for a mapper namespace.
pub fn methods_by_name(methods: &[Symbol]) -> HashMap<String, Symbol> {
    methods
        .iter()
        .map(|symbol| (symbol.name.clone(), symbol.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapper_xml_with_doctype_is_parsed() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata/demo1/src/main/resources/mapper/UserMapper.xml");
        let text = fs::read_to_string(&path).unwrap();
        let document = Document::parse_with_options(
            &text,
            ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mapper = document
            .descendants()
            .find(|node| node.is_element() && node.has_tag_name("mapper"))
            .unwrap();
        assert_eq!(mapper.attribute("namespace"), Some("demo.UserMapper"));
        assert_eq!(
            mapper
                .children()
                .filter(|node| node.is_element() && node.has_tag_name("select"))
                .count(),
            1
        );
    }

    #[test]
    fn locates_single_and_double_quoted_ids() {
        let text = "<mapper namespace=\"a.B\"><select id=\"findById\">SELECT 1</select></mapper>";
        assert!(locate_id(text, "findById").is_some());
        let single = "<mapper><update id='save'>x</update></mapper>";
        assert!(locate_id(single, "save").is_some());
        assert!(locate_id(text, "missing").is_none());
    }
}
