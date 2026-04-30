use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub scope: String,
    pub category: String,
    pub content: String,
    pub approved: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryStore {
    pub entries: Vec<MemoryEntry>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stage(&mut self, scope: &str, category: &str, content: &str) -> String {
        let threshold = 0.65;
        let new_words: HashSet<String> = tokenize(content);

        if let Some((idx, score)) = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.approved && e.scope == scope && e.category == category)
            .map(|(i, e)| {
                let existing_words: HashSet<String> = tokenize(&e.content);
                let intersection: HashSet<_> =
                    new_words.intersection(&existing_words).cloned().collect();
                let union: HashSet<_> = new_words.union(&existing_words).cloned().collect();
                let similarity = if union.is_empty() {
                    0.0
                } else {
                    intersection.len() as f64 / union.len() as f64
                };
                (i, similarity)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            && score >= threshold
        {
            let existing = &self.entries[idx];
            let merged =
                if existing.content.contains(content) || content.contains(&existing.content) {
                    if existing.content.len() > content.len() {
                        existing.content.clone()
                    } else {
                        content.to_string()
                    }
                } else {
                    format!("{}\n\n--- updated ---\n\n{}", existing.content, content)
                };
            let id = existing.id.clone();
            self.entries[idx].content = merged;
            self.entries[idx].created_at = chrono::Utc::now().to_rfc3339();
            return id;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let entry = MemoryEntry {
            id: id.clone(),
            scope: scope.to_string(),
            category: category.to_string(),
            content: content.to_string(),
            approved: scope == "session",
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.entries.push(entry);
        id
    }

    pub fn commit(&mut self, ids: &[String]) -> usize {
        let mut count = 0;
        for entry in &mut self.entries {
            if ids.contains(&entry.id) && !entry.approved {
                entry.approved = true;
                count += 1;
            }
        }
        count
    }

    pub fn get_approved(&self, scope: Option<&str>) -> Vec<&MemoryEntry> {
        self.entries
            .iter()
            .filter(|e| e.approved && scope.is_none_or(|s| e.scope == s))
            .collect()
    }

    pub fn get_pending(&self) -> Vec<&MemoryEntry> {
        self.entries.iter().filter(|e| !e.approved).collect()
    }
}

pub struct MemoryManager {
    path: std::path::PathBuf,
    store: MemoryStore,
}

impl MemoryManager {
    pub fn new(workspace_root: &Path) -> Self {
        let path = workspace_root.join(".charm").join("memory.json");
        let store = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            MemoryStore::new()
        };
        Self { path, store }
    }

    pub fn stage(&mut self, scope: &str, category: &str, content: &str) -> String {
        self.store.stage(scope, category, content)
    }

    pub fn commit(&mut self, ids: &[String]) -> usize {
        self.store.commit(ids)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.path.parent().unwrap())?;
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.store)?)?;
        Ok(())
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut MemoryStore {
        &mut self.store
    }
}

fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_string())
        .collect()
}
