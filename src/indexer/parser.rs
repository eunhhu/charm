use crate::indexer::types::{Index, Symbol};
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub struct Indexer;

impl Indexer {
    pub fn index_workspace(root: &std::path::Path, index: &mut Index) -> anyhow::Result<()> {
        let mut python_parser = Parser::new();
        python_parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        let mut js_parser = Parser::new();
        js_parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;

        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| Self::should_visit(e))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_str = rel.to_string_lossy().to_string();

            if let Some(lang) = Self::detect_language(path) {
                if let Ok(metadata) = std::fs::metadata(path) {
                    if metadata.len() > 1_000_000 {
                        continue;
                    }
                }

                match lang.as_str() {
                    "python" => {
                        Self::index_python_with_parser(&mut python_parser, path, &rel_str, index)?
                    }
                    "javascript" | "typescript" => {
                        Self::index_js_with_parser(&mut js_parser, path, &rel_str, index)?
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn should_visit(entry: &walkdir::DirEntry) -> bool {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_dir() {
            match name.as_ref() {
                "node_modules" | ".git" | "target" | "dist" | "build" | "__pycache__" | ".venv"
                | "venv" => false,
                _ => true,
            }
        } else {
            true
        }
    }

    fn detect_language(path: &std::path::Path) -> Option<String> {
        match path.extension()?.to_str()? {
            "py" => Some("python".to_string()),
            "js" | "jsx" => Some("javascript".to_string()),
            "ts" | "tsx" => Some("typescript".to_string()),
            _ => None,
        }
    }

    fn index_python(
        path: &std::path::Path,
        rel_path: &str,
        index: &mut Index,
    ) -> anyhow::Result<()> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        Self::index_python_with_parser(&mut parser, path, rel_path, index)
    }

    fn index_python_with_parser(
        parser: &mut Parser,
        path: &std::path::Path,
        rel_path: &str,
        index: &mut Index,
    ) -> anyhow::Result<()> {
        let source = std::fs::read_to_string(path)?;
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
        let root = tree.root_node();

        Self::walk_python(&source, root, rel_path, index, None);
        Ok(())
    }

    fn walk_python(
        source: &str,
        node: Node,
        file_path: &str,
        index: &mut Index,
        class_name: Option<&str>,
    ) {
        for i in 0..node.child_count() {
            let child = node.child(i).unwrap();
            match child.kind() {
                "function_definition" => {
                    if let Some(sym) =
                        Self::extract_python_function(source, child, file_path, class_name)
                    {
                        index.add_symbol(sym);
                    }
                }
                "class_definition" => {
                    let cls_name = Self::node_text(source, child.child_by_field_name("name"));
                    if let Some(sym) = Self::extract_python_class(source, child, file_path) {
                        index.add_symbol(sym);
                    }
                    Self::walk_python(source, child, file_path, index, cls_name.as_deref());
                }
                _ => {
                    Self::walk_python(source, child, file_path, index, class_name);
                }
            }
        }
    }

    fn extract_python_function(
        source: &str,
        node: Node,
        file_path: &str,
        class_name: Option<&str>,
    ) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let params = node.child_by_field_name("parameters");
        let body = node.child_by_field_name("body");

        let full_name = class_name
            .map(|c| format!("{}.{}", c, name))
            .unwrap_or_else(|| name.clone());
        let signature = format!(
            "def {}({})",
            full_name,
            params
                .map(|p| Self::node_text_raw(source, p))
                .unwrap_or_default()
        );

        let (docstring, body_start, body_end) = body
            .map(|b| {
                let doc = Self::extract_docstring(source, b);
                (doc, b.start_position().row + 1, b.end_position().row + 1)
            })
            .unwrap_or((
                None,
                node.start_position().row + 1,
                node.end_position().row + 1,
            ));

        Some(Symbol {
            name: full_name,
            kind: if class_name.is_some() {
                "method".to_string()
            } else {
                "function".to_string()
            },
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature,
            docstring,
            body_start,
            body_end,
        })
    }

    fn extract_python_class(source: &str, node: Node, file_path: &str) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let body = node.child_by_field_name("body");

        let (docstring, body_start, body_end) = body
            .map(|b| {
                let doc = Self::extract_docstring(source, b);
                (doc, b.start_position().row + 1, b.end_position().row + 1)
            })
            .unwrap_or((
                None,
                node.start_position().row + 1,
                node.end_position().row + 1,
            ));

        Some(Symbol {
            name: name.clone(),
            kind: "class".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature: format!("class {}:", name),
            docstring,
            body_start,
            body_end,
        })
    }

    fn extract_docstring(source: &str, body: Node) -> Option<String> {
        let first = body.child(0)?;
        if first.kind() == "expression_statement" {
            let expr = first.child(0)?;
            if expr.kind() == "string" {
                return Some(
                    Self::node_text_raw(source, expr)
                        .trim_matches('"')
                        .trim_matches('\'')
                        .trim()
                        .to_string(),
                );
            }
        }
        None
    }

    fn index_js(path: &std::path::Path, rel_path: &str, index: &mut Index) -> anyhow::Result<()> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
        Self::index_js_with_parser(&mut parser, path, rel_path, index)
    }

    fn index_js_with_parser(
        parser: &mut Parser,
        path: &std::path::Path,
        rel_path: &str,
        index: &mut Index,
    ) -> anyhow::Result<()> {
        let source = std::fs::read_to_string(path)?;
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
        let root = tree.root_node();

        Self::walk_js(&source, root, rel_path, index);
        Ok(())
    }

    fn walk_js(source: &str, node: Node, file_path: &str, index: &mut Index) {
        for i in 0..node.child_count() {
            let child = node.child(i).unwrap();
            match child.kind() {
                "function_declaration" | "method_definition" => {
                    if let Some(sym) = Self::extract_js_function(source, child, file_path) {
                        index.add_symbol(sym);
                    }
                }
                "class_declaration" => {
                    if let Some(sym) = Self::extract_js_class(source, child, file_path) {
                        index.add_symbol(sym);
                    }
                    Self::walk_js(source, child, file_path, index);
                }
                _ => {
                    Self::walk_js(source, child, file_path, index);
                }
            }
        }
    }

    fn extract_js_function(source: &str, node: Node, file_path: &str) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let params = node.child_by_field_name("parameters");
        let body = node.child_by_field_name("body");

        let signature = format!(
            "function {}({})",
            name,
            params
                .map(|p| Self::node_text_raw(source, p))
                .unwrap_or_default()
        );

        Some(Symbol {
            name,
            kind: "function".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature,
            docstring: None,
            body_start: node.start_position().row + 1,
            body_end: body
                .map(|b| b.end_position().row + 1)
                .unwrap_or(node.end_position().row + 1),
        })
    }

    fn extract_js_class(source: &str, node: Node, file_path: &str) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        Some(Symbol {
            name: name.clone(),
            kind: "class".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature: format!("class {}", name),
            docstring: None,
            body_start: node.start_position().row + 1,
            body_end: node.end_position().row + 1,
        })
    }

    fn node_text(source: &str, node: Option<Node>) -> Option<String> {
        node.map(|n| source[n.byte_range()].to_string())
    }

    fn node_text_raw(source: &str, node: Node) -> String {
        source[node.byte_range()].to_string()
    }
}
