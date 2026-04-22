use crate::indexer::types::Index;
use std::path::Path;

pub struct IndexStore {
    path: std::path::PathBuf,
}

impl IndexStore {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            path: workspace_root.join(".charm").join("index.json"),
        }
    }

    pub fn save(&self, index: &Index) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(index)?;
        std::fs::create_dir_all(self.path.parent().unwrap())?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    pub fn load(&self) -> anyhow::Result<Index> {
        if !self.path.exists() {
            return Ok(Index::default());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        let index: Index = serde_json::from_str(&raw)?;
        Ok(index)
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}
