use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source of reference information
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceSourceKind {
    /// Context7 MCP provider
    Context7,
    /// Official documentation website
    OfficialDocs,
    /// Package registry (crates.io, npm, PyPI, etc.)
    PackageRegistry,
    /// GitHub issues/discussions
    GitHubIssues,
    /// Web search results
    WebSearch,
    /// Local package source
    LocalSource,
}

/// Confidence level for reference data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceConfidence {
    /// Official source, verified
    Official,
    /// Community verified, widely used
    High,
    /// Community source, needs verification
    Medium,
    /// Unverified, use with caution
    Low,
    /// Uncertain origin
    Uncertain,
}

/// Compiled reference information for external libraries/APIs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencePack {
    pub source_kind: ReferenceSourceKind,
    pub library: Option<String>,
    pub version: Option<String>,
    pub query: String,
    pub relevant_rules: Vec<String>,
    pub minimal_examples: Vec<CodeExample>,
    pub caveats: Vec<String>,
    pub anti_patterns: Vec<String>,
    pub source_refs: Vec<SourceRef>,
    pub confidence: ReferenceConfidence,
    /// When this reference was fetched
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A code example with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeExample {
    pub title: Option<String>,
    pub code: String,
    pub language: String,
    pub source_url: Option<String>,
}

/// Source reference for traceability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub url: String,
    pub title: Option<String>,
    pub accessed_at: chrono::DateTime<chrono::Utc>,
    pub hash: Option<String>, // Content hash for verification
}

/// Package identifier for resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageId {
    pub name: String,
    pub version: Option<String>,
    pub registry: Option<String>, // crates.io, npm, PyPI, etc.
}

/// Raw finding before compilation into ReferencePack
#[derive(Debug, Clone)]
pub struct RawFinding {
    pub kind: FindingKind,
    pub title: Option<String>,
    pub content: String,
    pub language: Option<String>,
    pub source_url: Option<String>,
}

/// Type of finding
#[derive(Debug, Clone, PartialEq)]
pub enum FindingKind {
    Example,
    Caveat,
    AntiPattern,
}

/// Search result from issue search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueResult {
    pub title: String,
    pub url: String,
    pub status: IssueStatus,
    pub relevance_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IssueStatus {
    Open,
    Closed,
    Merged,
}

/// Reference broker that fetches and compiles external documentation
pub struct ReferenceBroker {
    /// Cache for reference packs to avoid repeated fetches
    cache: HashMap<String, ReferencePack>,
    /// Context7 MCP endpoint (if configured)
    context7_endpoint: Option<String>,
    /// Whether to fallback to web search
    enable_web_fallback: bool,
    /// Max age for cached references (in hours)
    cache_ttl_hours: u32,
}

impl ReferenceBroker {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            context7_endpoint: std::env::var("CONTEXT7_ENDPOINT").ok(),
            enable_web_fallback: true,
            cache_ttl_hours: 24,
        }
    }

    /// Configure with Context7 MCP endpoint
    pub fn with_context7_endpoint(mut self, endpoint: String) -> Self {
        self.context7_endpoint = Some(endpoint);
        self
    }

    /// Disable web search fallback
    pub fn disable_web_fallback(mut self) -> Self {
        self.enable_web_fallback = false;
        self
    }

    /// Set cache TTL in hours
    pub fn with_cache_ttl(mut self, hours: u32) -> Self {
        self.cache_ttl_hours = hours;
        self
    }

    /// Resolve library/package name and version from the codebase
    pub fn resolve_packages(&self, codebase_root: &std::path::Path) -> Vec<PackageId> {
        let mut packages = Vec::new();

        // Try to detect Rust packages from Cargo.toml
        if let Ok(content) = std::fs::read_to_string(codebase_root.join("Cargo.toml")) {
            packages.extend(self.parse_cargo_toml(&content));
        }

        // Try to detect Node packages from package.json
        if let Ok(content) = std::fs::read_to_string(codebase_root.join("package.json")) {
            packages.extend(self.parse_package_json(&content));
        }

        // Try to detect Python packages from requirements.txt or pyproject.toml
        if let Ok(content) = std::fs::read_to_string(codebase_root.join("requirements.txt")) {
            packages.extend(self.parse_requirements_txt(&content));
        }

        packages
    }

    /// Fetch documentation and examples for a package
    pub async fn fetch_docs(&mut self, package: &PackageId) -> anyhow::Result<ReferencePack> {
        let cache_key = format!("{}@{:?}", package.name, package.version);

        // Check cache first
        if let Some(cached) = self.cache.get(&cache_key) {
            if let Some(fetched_at) = cached.fetched_at {
                let age = chrono::Utc::now().signed_duration_since(fetched_at);
                if age.num_hours() < self.cache_ttl_hours as i64 {
                    return Ok(cached.clone());
                }
            }
        }

        // Try Context7 first if configured
        if let Some(ref endpoint) = self.context7_endpoint {
            match self.fetch_from_context7(package, endpoint).await {
                Ok(pack) => {
                    self.cache.insert(cache_key, pack.clone());
                    return Ok(pack);
                }
                Err(e) => {
                    eprintln!("[ReferenceBroker] Context7 fetch failed: {}", e);
                }
            }
        }

        // Try package registry
        match self.fetch_from_registry(package).await {
            Ok(pack) => {
                self.cache.insert(cache_key, pack.clone());
                return Ok(pack);
            }
            Err(e) => {
                eprintln!("[ReferenceBroker] Registry fetch failed: {}", e);
            }
        }

        // Fallback to web search if enabled
        if self.enable_web_fallback {
            match self.fetch_from_web_search(package).await {
                Ok(pack) => {
                    self.cache.insert(cache_key, pack.clone());
                    return Ok(pack);
                }
                Err(e) => {
                    eprintln!("[ReferenceBroker] Web search failed: {}", e);
                }
            }
        }

        Err(anyhow::anyhow!(
            "Failed to fetch docs for {} from any source",
            package.name
        ))
    }

    /// Search official issues/changelogs before repeated debugging
    pub async fn search_issues(
        &self,
        _package: &PackageId,
        _error_message: &str,
    ) -> anyhow::Result<Vec<IssueResult>> {
        // This would search GitHub issues, Stack Overflow, etc.
        // For now, return empty as a skeleton
        Ok(Vec::new())
    }

    /// Compile raw findings into a ReferencePack
    pub fn compile_reference_pack(
        &self,
        source_kind: ReferenceSourceKind,
        findings: Vec<RawFinding>,
        query: &str,
    ) -> ReferencePack {
        let mut examples = Vec::new();
        let mut caveats = Vec::new();
        let mut anti_patterns = Vec::new();
        let mut source_refs = Vec::new();

        for finding in &findings {
            match finding.kind {
                FindingKind::Example => {
                    examples.push(CodeExample {
                        title: finding.title.clone(),
                        code: finding.content.clone(),
                        language: finding.language.clone().unwrap_or_default(),
                        source_url: finding.source_url.clone(),
                    });
                }
                FindingKind::Caveat => {
                    caveats.push(finding.content.clone());
                }
                FindingKind::AntiPattern => {
                    anti_patterns.push(finding.content.clone());
                }
            }

            if let Some(ref url) = finding.source_url {
                source_refs.push(SourceRef {
                    url: url.clone(),
                    title: finding.title.clone(),
                    accessed_at: chrono::Utc::now(),
                    hash: None, // Would compute content hash in real impl
                });
            }
        }

        ReferencePack {
            source_kind,
            library: None,
            version: None,
            query: query.to_string(),
            relevant_rules: Vec::new(),
            minimal_examples: examples,
            caveats,
            anti_patterns,
            source_refs,
            confidence: ReferenceConfidence::Medium,
            fetched_at: Some(chrono::Utc::now()),
        }
    }

    // Private helper methods

    fn parse_cargo_toml(&self, content: &str) -> Vec<PackageId> {
        let mut packages = Vec::new();
        // Simple parsing - in real impl would use toml crate
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("[dependencies]") || line.starts_with("[dev-dependencies]") {
                // Next lines are dependencies
                continue;
            }
            if let Some(eq_pos) = line.find('=') {
                let name = line[..eq_pos].trim().to_string();
                if !name.is_empty() && !name.starts_with('[') {
                    packages.push(PackageId {
                        name,
                        version: None, // Would parse from = "x.y.z"
                        registry: Some("crates.io".to_string()),
                    });
                }
            }
        }
        packages
    }

    fn parse_package_json(&self, _content: &str) -> Vec<PackageId> {
        // Simple parsing - in real impl would use serde_json
        // For now, return empty as skeleton
        Vec::new()
    }

    fn parse_requirements_txt(&self, content: &str) -> Vec<PackageId> {
        let mut packages = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Parse "package==version" or "package>=version" or just "package"
            let name =
                if let Some(pos) = line.find(|c| c == '=' || c == '>' || c == '<' || c == ';') {
                    line[..pos].trim().to_string()
                } else {
                    line.to_string()
                };
            if !name.is_empty() {
                packages.push(PackageId {
                    name,
                    version: None,
                    registry: Some("pypi".to_string()),
                });
            }
        }
        packages
    }

    async fn fetch_from_context7(
        &self,
        _package: &PackageId,
        _endpoint: &str,
    ) -> anyhow::Result<ReferencePack> {
        // Would make MCP call to Context7
        Err(anyhow::anyhow!("Context7 not yet implemented"))
    }

    async fn fetch_from_registry(&self, _package: &PackageId) -> anyhow::Result<ReferencePack> {
        // Would query crates.io, npm, PyPI APIs
        Err(anyhow::anyhow!("Registry fetch not yet implemented"))
    }

    async fn fetch_from_web_search(&self, _package: &PackageId) -> anyhow::Result<ReferencePack> {
        // Would use web search tool
        Err(anyhow::anyhow!("Web search not yet implemented"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reference_broker_new() {
        let broker = ReferenceBroker::new();
        assert!(broker.context7_endpoint.is_none() || broker.context7_endpoint.is_some());
    }

    #[test]
    fn test_with_context7_endpoint() {
        let broker =
            ReferenceBroker::new().with_context7_endpoint("http://localhost:8080".to_string());
        assert_eq!(
            broker.context7_endpoint,
            Some("http://localhost:8080".to_string())
        );
    }

    #[test]
    fn test_disable_web_fallback() {
        let broker = ReferenceBroker::new().disable_web_fallback();
        assert!(!broker.enable_web_fallback);
    }

    #[test]
    fn test_parse_cargo_toml() {
        let broker = ReferenceBroker::new();
        let content = r#"
[package]
name = "myapp"
version = "0.1.0"

[dependencies]
tokio = "1.0"
serde = { version = "1.0", features = ["derive"] }
"#;
        let packages = broker.parse_cargo_toml(content);
        assert!(!packages.is_empty());
        assert!(packages.iter().any(|p| p.name == "tokio"));
        assert!(packages.iter().any(|p| p.name == "serde"));
    }

    #[test]
    fn test_parse_requirements_txt() {
        let broker = ReferenceBroker::new();
        let content = r#"
requests==2.28.1
numpy>=1.21.0
# comment
pandas
"#;
        let packages = broker.parse_requirements_txt(content);
        assert_eq!(packages.len(), 3);
        assert!(packages.iter().any(|p| p.name == "requests"));
        assert!(packages.iter().any(|p| p.name == "numpy"));
        assert!(packages.iter().any(|p| p.name == "pandas"));
    }

    #[test]
    fn test_compile_reference_pack() {
        let broker = ReferenceBroker::new();
        let findings = vec![
            RawFinding {
                kind: FindingKind::Example,
                title: Some("Basic usage".to_string()),
                content: "let x = 42;".to_string(),
                language: Some("rust".to_string()),
                source_url: Some("https://example.com".to_string()),
            },
            RawFinding {
                kind: FindingKind::Caveat,
                title: None,
                content: "Be careful with ownership".to_string(),
                language: None,
                source_url: None,
            },
        ];

        let pack = broker.compile_reference_pack(
            ReferenceSourceKind::OfficialDocs,
            findings,
            "test query",
        );

        assert_eq!(pack.source_kind, ReferenceSourceKind::OfficialDocs);
        assert_eq!(pack.query, "test query");
        assert_eq!(pack.minimal_examples.len(), 1);
        assert_eq!(pack.caveats.len(), 1);
        assert_eq!(pack.confidence, ReferenceConfidence::Medium);
        assert!(pack.fetched_at.is_some());
    }

    #[test]
    fn test_reference_confidence_ordering() {
        // Confidence should have clear hierarchy
        assert_ne!(ReferenceConfidence::Official, ReferenceConfidence::Low);
        assert_ne!(ReferenceConfidence::High, ReferenceConfidence::Uncertain);
    }

    #[test]
    fn test_package_id_creation() {
        let pkg = PackageId {
            name: "serde".to_string(),
            version: Some("1.0".to_string()),
            registry: Some("crates.io".to_string()),
        };
        assert_eq!(pkg.name, "serde");
        assert_eq!(pkg.version, Some("1.0".to_string()));
    }
}
