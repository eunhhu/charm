use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DependencyGraph {
    nodes: HashMap<String, FileNode>,
    edges: Vec<DependencyEdge>,
}

#[derive(Debug, Clone)]
pub struct FileNode {
    pub path: String,
    pub imports: Vec<String>,
    pub exports: Vec<String>,
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub kind: DependencyKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DependencyKind {
    Import,
    Call,
    Extend,
    Reference,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn analyze_workspace(root: &Path) -> anyhow::Result<Self> {
        let mut graph = Self::new();

        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| Self::should_visit(e))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if let Some(rel) = path.strip_prefix(root).ok() {
                let rel_str = rel.to_string_lossy().to_string();
                if let Ok(content) = std::fs::read_to_string(path) {
                    let node = Self::analyze_file(&rel_str, &content);
                    graph.nodes.insert(rel_str.clone(), node);
                }
            }
        }

        graph.build_edges(root);
        Ok(graph)
    }

    fn should_visit(entry: &walkdir::DirEntry) -> bool {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_dir() {
            match name.as_ref() {
                "node_modules" | ".git" | "target" | "dist" | "build" | "__pycache__" | ".venv"
                | "venv" | ".charm" => false,
                _ => true,
            }
        } else {
            true
        }
    }

    fn analyze_file(path: &str, content: &str) -> FileNode {
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut symbols = Vec::new();

        if path.ends_with(".py") {
            Self::analyze_python(content, &mut imports, &mut exports, &mut symbols);
        } else if path.ends_with(".js")
            || path.ends_with(".ts")
            || path.ends_with(".jsx")
            || path.ends_with(".tsx")
        {
            Self::analyze_javascript(content, &mut imports, &mut exports, &mut symbols);
        } else if path.ends_with(".go") {
            Self::analyze_go(content, &mut imports, &mut exports, &mut symbols);
        } else if path.ends_with(".rs") {
            Self::analyze_rust(content, &mut imports, &mut exports, &mut symbols);
        }

        FileNode {
            path: path.to_string(),
            imports,
            exports,
            symbols,
        }
    }

    fn analyze_python(
        content: &str,
        imports: &mut Vec<String>,
        exports: &mut Vec<String>,
        symbols: &mut Vec<String>,
    ) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("import ") || line.starts_with("from ") {
                imports.push(line.to_string());
            } else if line.starts_with("def ") {
                if let Some(name) = line.split_whitespace().nth(1) {
                    if let Some(end) = name.find('(') {
                        symbols.push(name[..end].to_string());
                    } else {
                        symbols.push(name.to_string());
                    }
                }
            } else if line.starts_with("class ") {
                if let Some(name) = line.split_whitespace().nth(1) {
                    if let Some(end) = name.find('(') {
                        symbols.push(name[..end].to_string());
                    } else if let Some(end) = name.find(':') {
                        symbols.push(name[..end].to_string());
                    } else {
                        symbols.push(name.to_string());
                    }
                }
            }
        }
    }

    fn analyze_javascript(
        content: &str,
        imports: &mut Vec<String>,
        exports: &mut Vec<String>,
        symbols: &mut Vec<String>,
    ) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("import ") || line.starts_with("require(") {
                imports.push(line.to_string());
            } else if line.starts_with("export ") {
                exports.push(line.to_string());
                if line.contains("function ") || line.contains("class ") {
                    if let Some(name) = line.split_whitespace().last() {
                        if let Some(end) = name.find('(') {
                            symbols.push(name[..end].to_string());
                        } else {
                            symbols.push(name.to_string());
                        }
                    }
                }
            } else if line.starts_with("function ") || line.starts_with("class ") {
                if let Some(name) = line.split_whitespace().nth(1) {
                    if let Some(end) = name.find('(') {
                        symbols.push(name[..end].to_string());
                    } else {
                        symbols.push(name.to_string());
                    }
                }
            }
        }
    }

    fn analyze_go(
        content: &str,
        imports: &mut Vec<String>,
        _exports: &mut Vec<String>,
        symbols: &mut Vec<String>,
    ) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("import ") {
                imports.push(line.to_string());
            } else if line.starts_with("func ") {
                if let Some(name) = line.split_whitespace().nth(1) {
                    if let Some(end) = name.find('(') {
                        symbols.push(name[..end].to_string());
                    } else {
                        symbols.push(name.to_string());
                    }
                }
            }
        }
    }

    fn analyze_rust(
        content: &str,
        imports: &mut Vec<String>,
        _exports: &mut Vec<String>,
        symbols: &mut Vec<String>,
    ) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("use ") || line.starts_with("extern crate ") {
                imports.push(line.to_string());
            } else if line.starts_with("fn ") || line.starts_with("pub fn ") {
                if let Some(name) = line.split_whitespace().nth(1) {
                    if let Some(end) = name.find('(') {
                        symbols.push(name[..end].to_string());
                    } else {
                        symbols.push(name.to_string());
                    }
                }
            } else if line.starts_with("struct ") || line.starts_with("pub struct ") {
                if let Some(name) = line.split_whitespace().nth(1) {
                    symbols.push(name.to_string());
                }
            }
        }
    }

    fn build_edges(&mut self, _root: &Path) {
        let paths: Vec<String> = self.nodes.keys().cloned().collect();

        for path in &paths {
            if let Some(node) = self.nodes.get(path) {
                for import in &node.imports {
                    if let Some(target) = Self::resolve_import(path, import, &paths) {
                        self.edges.push(DependencyEdge {
                            from: path.clone(),
                            to: target,
                            kind: DependencyKind::Import,
                        });
                    }
                }
            }
        }
    }

    fn resolve_import(_from: &str, import: &str, paths: &[String]) -> Option<String> {
        let import_clean = import
            .replace("import ", "")
            .replace("from ", "")
            .replace("use ", "")
            .replace("require(", "")
            .replace("'", "")
            .replace("\"", "");

        for path in paths {
            let file_name = path.split('/').last().unwrap_or(path);
            let file_stem = file_name.split('.').next().unwrap_or(file_name);

            if import_clean.contains(file_stem) {
                return Some(path.clone());
            }
        }
        None
    }

    pub fn get_related_files(&self, file_path: &str, depth: usize) -> Vec<String> {
        let mut related = HashSet::new();
        let mut current = HashSet::new();
        current.insert(file_path.to_string());

        for _ in 0..depth {
            let mut next = HashSet::new();
            for path in &current {
                for edge in &self.edges {
                    if edge.from == *path {
                        next.insert(edge.to.clone());
                        related.insert(edge.to.clone());
                    }
                    if edge.to == *path {
                        next.insert(edge.from.clone());
                        related.insert(edge.from.clone());
                    }
                }
            }
            current = next;
        }

        related.remove(file_path);
        related.into_iter().collect()
    }

    pub fn get_relevance_score(&self, file_path: &str, target_path: &str) -> f64 {
        let mut score = 0.0;

        if let Some(node) = self.nodes.get(file_path) {
            if node.imports.iter().any(|i| i.contains(target_path)) {
                score += 1.0;
            }
            if node.exports.iter().any(|e| e.contains(target_path)) {
                score += 0.8;
            }
        }

        for edge in &self.edges {
            if (edge.from == file_path && edge.to == target_path)
                || (edge.from == target_path && edge.to == file_path)
            {
                score += match edge.kind {
                    DependencyKind::Import => 0.9,
                    DependencyKind::Call => 0.7,
                    DependencyKind::Extend => 0.6,
                    DependencyKind::Reference => 0.4,
                };
            }
        }

        score
    }
}
