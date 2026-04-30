use super::types::{Evidence, RetrievalResult};
use crate::indexer::{store::IndexStore, types::Index};
use std::path::Path;

pub struct RetrievalWorker {
    index: Option<Index>,
    cwd: std::path::PathBuf,
}

impl RetrievalWorker {
    pub fn new(cwd: &Path) -> Self {
        let store = IndexStore::new(cwd);
        let index = store.load().ok().filter(|i| !i.symbols.is_empty());
        Self {
            index,
            cwd: cwd.to_path_buf(),
        }
    }

    pub async fn retrieve(&self, query: &str, top_k: usize) -> anyhow::Result<RetrievalResult> {
        let queries = Self::decompose_query(query);
        let mut all_evidence = Vec::new();

        for q in &queries {
            let grep_result = crate::tools::search::grep_search(
                serde_json::json!({"pattern": q, "output_mode": "content"}),
                &self.cwd,
            )
            .await;
            if let Ok(result) = grep_result {
                if result.success {
                    all_evidence.extend(Self::parse_grep_output(&result.output, q));
                }
            }

            if let Some(ref index) = self.index {
                let sem_results = index.search(q, top_k);
                for sym in sem_results {
                    all_evidence.push(Evidence {
                        source: "semantic".to_string(),
                        rank: Self::score(q, &sym.signature),
                        file_path: sym.file_path.clone(),
                        line: sym.line,
                        snippet: sym.signature.clone(),
                        context: sym.docstring.clone(),
                    });
                }
            }
        }

        all_evidence.sort_by(|a, b| b.rank.partial_cmp(&a.rank).unwrap());
        all_evidence.dedup_by(|a, b| a.file_path == b.file_path && a.line == b.line);

        let top = all_evidence.into_iter().take(top_k).collect::<Vec<_>>();
        let summary = Self::summarize(&top);

        Ok(RetrievalResult {
            query: query.to_string(),
            evidence: top,
            summary,
        })
    }

    fn decompose_query(query: &str) -> Vec<String> {
        let q = query.trim();
        if q.contains(" and ") || q.contains(" + ") {
            q.split(" and ").map(|s| s.trim().to_string()).collect()
        } else {
            vec![q.to_string()]
        }
    }

    fn parse_grep_output(output: &str, query: &str) -> Vec<Evidence> {
        let mut results = Vec::new();
        for line in output.lines().take(30) {
            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() < 2 {
                continue;
            }
            let file_path = parts[0].to_string();
            let content = parts[1].to_string();
            let rank = Self::score(query, &content);
            results.push(Evidence {
                source: "grep".to_string(),
                rank,
                file_path,
                line: 0,
                snippet: content,
                context: None,
            });
        }
        results
    }

    fn score(query: &str, text: &str) -> f32 {
        let q = query.to_lowercase();
        let t = text.to_lowercase();
        let mut score = 0.0;
        if t.contains(&q) {
            score += 10.0;
        }
        for word in q.split_whitespace() {
            if t.contains(word) {
                score += 3.0;
            }
        }
        score
    }

    fn summarize(evidence: &[Evidence]) -> String {
        if evidence.is_empty() {
            return "No evidence found.".to_string();
        }
        let files: Vec<String> = evidence
            .iter()
            .map(|e| format!("{} ({})", e.file_path, e.source))
            .collect();
        format!(
            "Found {} matches across {} files: {}",
            evidence.len(),
            evidence
                .iter()
                .map(|e| &e.file_path)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            files.join(", ")
        )
    }
}
