use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: usize,
    pub col: usize,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    pub body_start: usize,
    pub body_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Index {
    pub files: HashMap<String, FileEntry>,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub language: String,
    pub symbols: Vec<String>,
}

impl Index {
    pub fn add_symbol(&mut self, symbol: Symbol) {
        let file_path = symbol.file_path.clone();
        self.symbols.push(symbol);
        let entry = self.files.entry(file_path).or_insert_with(|| FileEntry {
            language: String::new(),
            symbols: Vec::new(),
        });
        entry
            .symbols
            .push(self.symbols.last().unwrap().name.clone());
    }

    pub fn search(&self, query: &str, top_k: usize) -> Vec<&Symbol> {
        let q = query.to_lowercase();
        let mut scored: Vec<(f32, &Symbol)> = self
            .symbols
            .iter()
            .map(|s| {
                let name_score = if s.name.to_lowercase().contains(&q) {
                    10.0
                } else {
                    0.0
                };
                let sig_score = if s.signature.to_lowercase().contains(&q) {
                    5.0
                } else {
                    0.0
                };
                let doc_score = s
                    .docstring
                    .as_ref()
                    .map(|d| {
                        if d.to_lowercase().contains(&q) {
                            3.0
                        } else {
                            0.0
                        }
                    })
                    .unwrap_or(0.0);
                (name_score + sig_score + doc_score, s)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        scored.into_iter().take(top_k).map(|(_, s)| s).collect()
    }
}
