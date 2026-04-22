use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub rank: f32,
    pub file_path: String,
    pub line: usize,
    pub snippet: String,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalResult {
    pub query: String,
    pub evidence: Vec<Evidence>,
    pub summary: String,
}
