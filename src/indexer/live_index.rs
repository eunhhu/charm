use crate::indexer::types::{Index, Symbol};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Windsurf-style in-memory live index
/// - 파일 변경 시 즉시 AST 재파싱
/// - 메모리 상주 (디스크 I/O 없음)
/// - Fuzzy 검색 지원
pub struct LiveIndex {
    /// Path -> Symbol list mapping for O(1) file lookup
    file_symbols: RwLock<HashMap<PathBuf, Vec<Symbol>>>,
    /// All symbols for global search
    all_symbols: RwLock<Vec<Symbol>>,
    /// Fuzzy index: name -> symbols
    name_index: RwLock<HashMap<String, Vec<usize>>>, // usize = index in all_symbols
    workspace_root: PathBuf,
}

impl LiveIndex {
    pub fn new(workspace_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            file_symbols: RwLock::new(HashMap::new()),
            all_symbols: RwLock::new(Vec::new()),
            name_index: RwLock::new(HashMap::new()),
            workspace_root,
        })
    }

    /// 파일 변경 시 즉시 업데이트 (Windsurf 실시간 방식)
    pub async fn update_file(&self, path: &Path, source: &str) -> anyhow::Result<()> {
        let rel_path = path.strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // 1. 기존 심볼 제거
        self.remove_file_internal(&rel_path).await;

        // 2. 새로 파싱
        let new_symbols = self.parse_symbols(source, &rel_path).await?;

        // 3. 인덱스 업데이트
        self.add_symbols_internal(&rel_path, new_symbols).await;

        Ok(())
    }

    /// 파일 삭제 시 인덱스에서 제거
    pub async fn remove_file(&self, path: &Path) {
        let rel_path = path.strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        self.remove_file_internal(&rel_path).await;
    }

    /// Fuzzy 심볼 검색 (Windsurf 스타일)
    pub async fn fuzzy_search(&self, query: &str, limit: usize) -> Vec<SymbolMatch> {
        let query_lower = query.to_lowercase();
        let all = self.all_symbols.read().await;
        let name_idx = self.name_index.read().await;

        let mut matches: Vec<SymbolMatch> = Vec::new();

        // 1. 정확한 prefix 매칭 (가장 높은 점수)
        for (name, indices) in name_idx.iter() {
            let name_lower = name.to_lowercase();
            let score = if name_lower == query_lower {
                100.0 // 완전 일치
            } else if name_lower.starts_with(&query_lower) {
                90.0 - (name.len() - query.len()) as f32 * 0.5 // prefix 일치
            } else if name_lower.contains(&query_lower) {
                50.0 - (name_lower.find(&query_lower).unwrap_or(0) as f32) * 0.3 // 포함
            } else {
                let dist = levenshtein_distance(&name_lower, &query_lower);
                if dist <= 3 {
                    30.0 - dist as f32 * 5.0 // 유사한 철자
                } else {
                    0.0
                }
            };

            if score > 0.0 {
                for &idx in indices {
                    if let Some(sym) = all.get(idx) {
                        matches.push(SymbolMatch {
                            symbol: sym.clone(),
                            score,
                            match_type: if score >= 90.0 {
                                MatchType::Prefix
                            } else if score >= 50.0 {
                                MatchType::Substring
                            } else {
                                MatchType::Fuzzy
                            },
                        });
                    }
                }
            }
        }

        // 점수순 정렬
        matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        matches.truncate(limit);
        matches
    }

    /// 정확한 심볼 조회 (정의로 이동용)
    pub async fn find_exact(&self, name: &str, file_hint: Option<&Path>) -> Option<Symbol> {
        let all = self.all_symbols.read().await;
        
        // 파일 힌트가 있으면 해당 파일 우선 검색
        if let Some(file) = file_hint {
            let file_str = file.to_string_lossy().to_string();
            if let Some(symbols) = self.file_symbols.read().await.get(&file_str) {
                for sym in symbols {
                    if sym.name == name {
                        return Some(sym.clone());
                    }
                }
            }
        }

        // 전역 검색
        all.iter().find(|s| s.name == name).cloned()
    }

    /// 파일 내 심볼 목록 (빠른 조회)
    pub async fn file_symbols(&self, path: &Path) -> Vec<Symbol> {
        let rel_path = path.strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        
        self.file_symbols.read().await
            .get(&rel_path)
            .cloned()
            .unwrap_or_default()
    }

    /// 병렬 검색 지원 (여러 쿼리 동시 처리)
    pub async fn parallel_search(&self, queries: &[&str], limit_per_query: usize) -> Vec<Vec<SymbolMatch>> {
        let futures = queries.iter().map(|q| self.fuzzy_search(q, limit_per_query));
        futures::future::join_all(futures).await
    }

    // Private helpers

    async fn remove_file_internal(&self, rel_path: &str) {
        let mut file_syms = self.file_symbols.write().await;
        let mut all = self.all_symbols.write().await;
        let mut name_idx = self.name_index.write().await;

        if let Some(symbols) = file_syms.remove(rel_path) {
            for sym in symbols {
                // name_index에서 제거
                if let Some(indices) = name_idx.get_mut(&sym.name) {
                    indices.retain(|&i| {
                        if let Some(s) = all.get(i) {
                            s.file_path != rel_path
                        } else {
                            false
                        }
                    });
                }
                // all_symbols에서 제거 (성능을 위해 lazy deletion 고려)
                // 실제 구현에서는 UUID 기반으로 개선 필요
            }
        }
    }

    async fn add_symbols_internal(&self, rel_path: &str, symbols: Vec<Symbol>) {
        let mut file_syms = self.file_symbols.write().await;
        let mut all = self.all_symbols.write().await;
        let mut name_idx = self.name_index.write().await;

        for sym in symbols {
            let idx = all.len();
            all.push(sym.clone());
            
            name_idx.entry(sym.name.clone())
                .or_insert_with(Vec::new)
                .push(idx);
        }

        file_syms.insert(rel_path.to_string(), symbols);
    }

    async fn parse_symbols(&self, source: &str, rel_path: &str) -> anyhow::Result<Vec<Symbol>> {
        // 언어 감지 및 파싱
        let ext = Path::new(rel_path).extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        match ext {
            "py" => self.parse_python(source, rel_path).await,
            "js" | "jsx" | "ts" | "tsx" => self.parse_js(source, rel_path).await,
            "rs" => self.parse_rust(source, rel_path).await,
            _ => Ok(Vec::new()),
        }
    }

    async fn parse_python(&self, source: &str, rel_path: &str) -> anyhow::Result<Vec<Symbol>> {
        use tree_sitter::Parser;
        
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        
        let tree = parser.parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Parse failed"))?;
        
        let mut symbols = Vec::new();
        Self::walk_python_tree(source, tree.root_node(), rel_path, &mut symbols, None);
        Ok(symbols)
    }

    fn walk_python_tree(
        source: &str,
        node: tree_sitter::Node,
        file_path: &str,
        symbols: &mut Vec<Symbol>,
        class_name: Option<&str>,
    ) {
        for i in 0..node.child_count() {
            let child = node.child(i).unwrap();
            match child.kind() {
                "function_definition" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let func_name = &source[name.byte_range()];
                        let full_name = class_name
                            .map(|c| format!("{}.{}", c, func_name))
                            .unwrap_or_else(|| func_name.to_string());
                        
                        symbols.push(Symbol {
                            name: full_name,
                            kind: if class_name.is_some() { "method".to_string() } else { "function".to_string() },
                            file_path: file_path.to_string(),
                            line: child.start_position().row + 1,
                            col: child.start_position().column,
                            signature: format!("def {}(...)", func_name),
                            docstring: None,
                            body_start: child.start_position().row + 1,
                            body_end: child.end_position().row + 1,
                        });
                    }
                }
                "class_definition" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let cls_name = &source[name.byte_range()];
                        symbols.push(Symbol {
                            name: cls_name.to_string(),
                            kind: "class".to_string(),
                            file_path: file_path.to_string(),
                            line: child.start_position().row + 1,
                            col: child.start_position().column,
                            signature: format!("class {}:", cls_name),
                            docstring: None,
                            body_start: child.start_position().row + 1,
                            body_end: child.end_position().row + 1,
                        });
                        
                        Self::walk_python_tree(source, child, file_path, symbols, Some(cls_name));
                    }
                }
                _ => {
                    Self::walk_python_tree(source, child, file_path, symbols, class_name);
                }
            }
        }
    }

    // JS/TS 파싱은 기존 parser.rs와 유사하게
    async fn parse_js(&self, _source: &str, _rel_path: &str) -> anyhow::Result<Vec<Symbol>> {
        // 기존 parser.rs 로직 재사용
        Ok(Vec::new())
    }

    async fn parse_rust(&self, source: &str, rel_path: &str) -> anyhow::Result<Vec<Symbol>> {
        use tree_sitter::Parser;

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Parse failed"))?;

        let mut symbols = Vec::new();
        Self::walk_rust_tree(source, tree.root_node(), rel_path, &mut symbols, None);
        Ok(symbols)
    }

    fn walk_rust_tree(
        source: &str,
        node: tree_sitter::Node,
        file_path: &str,
        symbols: &mut Vec<Symbol>,
        impl_type: Option<&str>,
    ) {
        for i in 0..node.child_count() {
            let child = node.child(i).unwrap();
            match child.kind() {
                "function_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let func_name = &source[name.byte_range()];
                        let full_name = if impl_type.is_some() {
                            format!("{}::{}", impl_type.unwrap(), func_name)
                        } else {
                            func_name.to_string()
                        };

                        symbols.push(Symbol {
                            name: full_name,
                            kind: if impl_type.is_some() { "method" } else { "function" }.to_string(),
                            file_path: file_path.to_string(),
                            line: child.start_position().row + 1,
                            col: child.start_position().column,
                            signature: format!("fn {}", func_name),
                            docstring: None,
                            body_start: child.start_position().row + 1,
                            body_end: child.end_position().row + 1,
                        });
                    }
                }
                "struct_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let struct_name = &source[name.byte_range()];
                        symbols.push(Symbol {
                            name: struct_name.to_string(),
                            kind: "struct".to_string(),
                            file_path: file_path.to_string(),
                            line: child.start_position().row + 1,
                            col: child.start_position().column,
                            signature: format!("struct {}", struct_name),
                            docstring: None,
                            body_start: child.start_position().row + 1,
                            body_end: child.end_position().row + 1,
                        });
                    }
                }
                "enum_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        let enum_name = &source[name.byte_range()];
                        symbols.push(Symbol {
                            name: enum_name.to_string(),
                            kind: "enum".to_string(),
                            file_path: file_path.to_string(),
                            line: child.start_position().row + 1,
                            col: child.start_position().column,
                            signature: format!("enum {}", enum_name),
                            docstring: None,
                            body_start: child.start_position().row + 1,
                            body_end: child.end_position().row + 1,
                        });
                    }
                }
                "impl_item" => {
                    let impl_name = child
                        .child_by_field_name("type")
                        .map(|n| &source[n.byte_range()])
                        .map(|s| s.to_string());
                    Self::walk_rust_tree(source, child, file_path, symbols, impl_name.as_deref());
                }
                _ => {
                    Self::walk_rust_tree(source, child, file_path, symbols, impl_type);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SymbolMatch {
    pub symbol: Symbol,
    pub score: f32,
    pub match_type: MatchType,
}

#[derive(Debug, Clone)]
pub enum MatchType {
    Exact,     // 완전 일치
    Prefix,    // 접두사 일치
    Substring, // 부분 문자열
    Fuzzy,     // 유사 철자
}

/// Levenshtein distance for fuzzy matching
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let len_a = a.chars().count();
    let len_b = b.chars().count();
    
    if len_a == 0 { return len_b; }
    if len_b == 0 { return len_a; }
    
    let mut matrix = vec![vec![0; len_b + 1]; len_a + 1];
    
    for i in 0..=len_a {
        matrix[i][0] = i;
    }
    for j in 0..=len_b {
        matrix[0][j] = j;
    }
    
    for (i, ca) in a.chars().enumerate() {
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            matrix[i + 1][j + 1] = std::cmp::min(
                std::cmp::min(matrix[i][j + 1] + 1, matrix[i + 1][j] + 1),
                matrix[i][j] + cost,
            );
        }
    }
    
    matrix[len_a][len_b]
}

/// File watcher integration for real-time updates
pub struct IndexWatcher {
    index: Arc<LiveIndex>,
    _watcher: notify::RecommendedWatcher,
}

impl IndexWatcher {
    pub fn new(workspace_root: PathBuf, index: Arc<LiveIndex>) -> anyhow::Result<Self> {
        use notify::{Config, Event, RecursiveMode, Watcher};
        
        let index_clone = Arc::clone(&index);
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    tokio::spawn(async move {
                        for path in event.paths {
                            match event.kind {
                                notify::EventKind::Create(_) | notify::EventKind::Modify(_) => {
                                    if let Ok(content) = tokio::fs::read_to_string(&path).await {
                                        let _ = index_clone.update_file(&path, &content).await;
                                    }
                                }
                                notify::EventKind::Remove(_) => {
                                    index_clone.remove_file(&path).await;
                                }
                                _ => {}
                            }
                        }
                    });
                }
                Err(e) => eprintln!("Watch error: {:?}", e),
            }
        })?;
        
        watcher.watch(&workspace_root, RecursiveMode::Recursive)?;
        
        Ok(Self {
            index,
            _watcher: watcher,
        })
    }
}
