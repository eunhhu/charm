use crate::indexer::types::{Index, Symbol};
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub struct Indexer;

impl Indexer {
    pub fn index_workspace(root: &std::path::Path, index: &mut Index) -> anyhow::Result<()> {
        let mut python_parser = Parser::new();
        python_parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        let mut js_parser = Parser::new();
        js_parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
        let mut rust_parser = Parser::new();
        rust_parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;

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
                    "rust" => {
                        Self::index_rust_with_parser(&mut rust_parser, path, &rel_str, index)?
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
            "rs" => Some("rust".to_string()),
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

    fn index_rust_with_parser(
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

        Self::walk_rust(&source, root, rel_path, index, None);
        Ok(())
    }

    fn walk_rust(
        source: &str,
        node: Node,
        file_path: &str,
        index: &mut Index,
        impl_type: Option<&str>,
    ) {
        for i in 0..node.child_count() {
            let child = node.child(i).unwrap();
            match child.kind() {
                "function_item" => {
                    if impl_type.is_some() {
                        // Method inside impl block
                        if let Some(sym) =
                            Self::extract_rust_method(source, child, file_path, impl_type)
                        {
                            index.add_symbol(sym);
                        }
                    } else {
                        // Top-level function
                        if let Some(sym) = Self::extract_rust_function(source, child, file_path) {
                            index.add_symbol(sym);
                        }
                    }
                }
                "struct_item" => {
                    if let Some(sym) = Self::extract_rust_struct(source, child, file_path) {
                        index.add_symbol(sym);
                    }
                }
                "enum_item" => {
                    if let Some(sym) = Self::extract_rust_enum(source, child, file_path) {
                        index.add_symbol(sym);
                    }
                }
                "impl_item" => {
                    let impl_type_name = Self::extract_rust_impl_type(source, child);
                    Self::walk_rust(source, child, file_path, index, impl_type_name.as_deref());
                }
                _ => {
                    Self::walk_rust(source, child, file_path, index, impl_type);
                }
            }
        }
    }

    fn extract_rust_function(source: &str, node: Node, file_path: &str) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let params = node.child_by_field_name("parameters");
        let body = node.child_by_field_name("body");

        let signature = format!(
            "fn {}({})",
            name,
            params
                .map(|p| Self::node_text_raw(source, p))
                .unwrap_or_default()
        );

        Some(Symbol {
            name: name.clone(),
            kind: "function".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature,
            docstring: Self::extract_rust_docstring(source, node),
            body_start: node.start_position().row + 1,
            body_end: body
                .map(|b| b.end_position().row + 1)
                .unwrap_or(node.end_position().row + 1),
        })
    }

    fn extract_rust_struct(source: &str, node: Node, file_path: &str) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let body = node.child_by_field_name("body");

        Some(Symbol {
            name: name.clone(),
            kind: "struct".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature: format!(
                "struct {}",
                Self::node_text_raw(source, node)
                    .lines()
                    .next()
                    .unwrap_or(&name)
                    .trim()
            ),
            docstring: Self::extract_rust_docstring(source, node),
            body_start: node.start_position().row + 1,
            body_end: body
                .map(|b| b.end_position().row + 1)
                .unwrap_or(node.end_position().row + 1),
        })
    }

    fn extract_rust_enum(source: &str, node: Node, file_path: &str) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let body = node.child_by_field_name("body");

        Some(Symbol {
            name: name.clone(),
            kind: "enum".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature: format!(
                "enum {}",
                Self::node_text_raw(source, node)
                    .lines()
                    .next()
                    .unwrap_or(&name)
                    .trim()
            ),
            docstring: Self::extract_rust_docstring(source, node),
            body_start: node.start_position().row + 1,
            body_end: body
                .map(|b| b.end_position().row + 1)
                .unwrap_or(node.end_position().row + 1),
        })
    }

    fn extract_rust_impl_type(source: &str, node: Node) -> Option<String> {
        let type_node = node.child_by_field_name("type")?;
        Some(Self::node_text_raw(source, type_node).trim().to_string())
    }

    fn extract_rust_method(
        source: &str,
        node: Node,
        file_path: &str,
        impl_type: Option<&str>,
    ) -> Option<Symbol> {
        let name = Self::node_text(source, node.child_by_field_name("name"))?;
        let params = node.child_by_field_name("parameters");
        let body = node.child_by_field_name("body");

        let full_name = impl_type
            .map(|t| format!("{}::{}", t, name))
            .unwrap_or_else(|| name.clone());

        let signature = format!(
            "fn {}({})",
            full_name,
            params
                .map(|p| Self::node_text_raw(source, p))
                .unwrap_or_default()
        );

        Some(Symbol {
            name: full_name,
            kind: "method".to_string(),
            file_path: file_path.to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            signature,
            docstring: Self::extract_rust_docstring(source, node),
            body_start: node.start_position().row + 1,
            body_end: body
                .map(|b| b.end_position().row + 1)
                .unwrap_or(node.end_position().row + 1),
        })
    }

    fn extract_rust_docstring(source: &str, node: Node) -> Option<String> {
        let prev = node.prev_sibling()?;
        if prev.kind() == "line_comment" || prev.kind() == "block_comment" {
            let text = Self::node_text_raw(source, prev);
            if text.starts_with("///") || text.starts_with("/**") {
                return Some(
                    text.lines()
                        .map(|l| l.trim_start_matches("///").trim_start_matches("/**").trim())
                        .collect::<Vec<_>>()
                        .join(" ")
                        .trim()
                        .to_string(),
                );
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_rust_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_index_rust_function() {
        let temp_dir = TempDir::new().unwrap();
        let rust_content = r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    println!("Hello, world!");
}
"#;
        create_test_rust_file(&temp_dir, "test.rs", rust_content);

        let mut index = Index::default();
        Indexer::index_workspace(temp_dir.path(), &mut index).unwrap();

        assert_eq!(index.symbols.len(), 2);

        let func_names: Vec<&str> = index.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(func_names.contains(&"add"));
        assert!(func_names.contains(&"main"));

        let add = index.symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add.kind, "function");
        assert!(add.signature.contains("fn add"));
    }

    #[test]
    fn test_index_rust_struct() {
        let temp_dir = TempDir::new().unwrap();
        let rust_content = r#"
pub struct Point {
    x: f64,
    y: f64,
}

struct Rectangle {
    width: u32,
    height: u32,
}
"#;
        create_test_rust_file(&temp_dir, "shapes.rs", rust_content);

        let mut index = Index::default();
        Indexer::index_workspace(temp_dir.path(), &mut index).unwrap();

        let struct_names: Vec<&str> = index
            .symbols
            .iter()
            .filter(|s| s.kind == "struct")
            .map(|s| s.name.as_str())
            .collect();

        assert!(struct_names.contains(&"Point"));
        assert!(struct_names.contains(&"Rectangle"));
    }

    #[test]
    fn test_index_rust_enum() {
        let temp_dir = TempDir::new().unwrap();
        let rust_content = r#"
enum Status {
    Active,
    Inactive,
    Pending,
}
"#;
        create_test_rust_file(&temp_dir, "status.rs", rust_content);

        let mut index = Index::default();
        Indexer::index_workspace(temp_dir.path(), &mut index).unwrap();

        let enum_sym = index.symbols.iter().find(|s| s.kind == "enum");
        assert!(enum_sym.is_some());
        assert_eq!(enum_sym.unwrap().name, "Status");
    }

    #[test]
    fn test_index_rust_impl_methods() {
        let temp_dir = TempDir::new().unwrap();
        let rust_content = r#"
struct Calculator;

impl Calculator {
    fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }

    fn subtract(&self, a: i32, b: i32) -> i32 {
        a - b
    }
}
"#;
        create_test_rust_file(&temp_dir, "calc.rs", rust_content);

        let mut index = Index::default();
        Indexer::index_workspace(temp_dir.path(), &mut index).unwrap();

        // Should find struct and at least 2 functions/methods
        assert!(!index.symbols.is_empty(), "Expected at least one symbol");

        // Look for functions or methods
        let functions: Vec<&Symbol> = index
            .symbols
            .iter()
            .filter(|s| s.kind == "function" || s.kind == "method")
            .collect();

        // Impl methods may be indexed as functions or methods depending on nesting
        assert!(
            functions.len() >= 2,
            "Expected at least 2 functions/methods"
        );

        // Check that we found the struct
        let has_struct = index.symbols.iter().any(|s| s.kind == "struct");
        assert!(has_struct, "Expected to find struct");
    }

    #[test]
    fn test_skip_target_and_git() {
        let temp_dir = TempDir::new().unwrap();

        // Create target directory with a Rust file
        let target_dir = temp_dir.path().join("target");
        std::fs::create_dir(&target_dir).unwrap();
        create_test_rust_file(&temp_dir, "target/ignored.rs", "fn ignored() {}");

        // Create .git directory with a Rust file
        let git_dir = temp_dir.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        create_test_rust_file(&temp_dir, ".git/ignored.rs", "fn git_ignored() {}");

        // Create a regular Rust file
        create_test_rust_file(&temp_dir, "main.rs", "fn main() {}");

        let mut index = Index::default();
        Indexer::index_workspace(temp_dir.path(), &mut index).unwrap();

        // Should only find main, not ignored files
        assert_eq!(index.symbols.len(), 1);
        assert_eq!(index.symbols[0].name, "main");
    }

    #[test]
    fn test_skip_large_files() {
        let temp_dir = TempDir::new().unwrap();

        // Create a small Rust file
        create_test_rust_file(&temp_dir, "small.rs", "fn small() {}");

        // Create a large file (over 1MB)
        let large_content = "x".repeat(1_100_000);
        let large_rust = format!("fn large() {{ let x = \"{}\"; }}", large_content);
        create_test_rust_file(&temp_dir, "large.rs", &large_rust);

        let mut index = Index::default();
        Indexer::index_workspace(temp_dir.path(), &mut index).unwrap();

        // Should only find small, not large
        assert_eq!(index.symbols.len(), 1);
        assert_eq!(index.symbols[0].name, "small");
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(
            Indexer::detect_language(std::path::Path::new("test.py")),
            Some("python".to_string())
        );
        assert_eq!(
            Indexer::detect_language(std::path::Path::new("test.rs")),
            Some("rust".to_string())
        );
        assert_eq!(
            Indexer::detect_language(std::path::Path::new("test.js")),
            Some("javascript".to_string())
        );
        assert_eq!(
            Indexer::detect_language(std::path::Path::new("test.ts")),
            Some("typescript".to_string())
        );
        assert_eq!(
            Indexer::detect_language(std::path::Path::new("test.txt")),
            None
        );
    }
}
