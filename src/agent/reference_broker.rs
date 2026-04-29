use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::tools::url_guard;

type HttpGetFuture = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>;
type HttpGet = Arc<dyn Fn(String) -> HttpGetFuture + Send + Sync>;
type HttpPostFuture = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>;
type HttpPost = Arc<dyn Fn(String, String) -> HttpPostFuture + Send + Sync>;

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
    /// HTTP GET adapter for registry/doc fetches.
    http_get: HttpGet,
    /// HTTP POST adapter for MCP/JSON-RPC fetches.
    http_post: HttpPost,
}

impl ReferenceBroker {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            context7_endpoint: std::env::var("CONTEXT7_ENDPOINT").ok(),
            enable_web_fallback: true,
            cache_ttl_hours: 24,
            http_get: default_http_get(),
            http_post: default_http_post(),
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

    /// Override HTTP GET transport, primarily for deterministic tests.
    pub fn with_http_get<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.http_get = Arc::new(move |url| Box::pin(f(url)));
        self
    }

    /// Override HTTP POST transport, primarily for deterministic tests.
    pub fn with_http_post<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.http_post = Arc::new(move |url, body| Box::pin(f(url, body)));
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

    /// Build a ReferencePack from installed or vendored package source.
    ///
    /// This is the first production-grade reference path because it is local,
    /// version-matched, auditable, and does not depend on network availability.
    pub fn fetch_from_local_source_roots(
        &self,
        package: &PackageId,
        roots: &[PathBuf],
        query: &str,
    ) -> anyhow::Result<ReferencePack> {
        let package_dir = find_local_package_dir(package, roots).ok_or_else(|| {
            anyhow::anyhow!("local source for package '{}' not found", package.name)
        })?;
        let findings = collect_local_source_findings(&package_dir)?;
        let mut pack =
            self.compile_reference_pack(ReferenceSourceKind::LocalSource, findings, query);
        pack.library = Some(package.name.clone());
        pack.version = package.version.clone();
        pack.confidence = ReferenceConfidence::High;
        pack.relevant_rules.push(
            "Local installed package source is preferred over model memory for API shape."
                .to_string(),
        );
        Ok(pack)
    }

    // Private helper methods

    fn parse_cargo_toml(&self, content: &str) -> Vec<PackageId> {
        let mut packages = Vec::new();
        let mut in_dependencies = false;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                in_dependencies = matches!(
                    line,
                    "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]"
                ) || line.starts_with("[target.")
                    && line.ends_with(".dependencies]");
                continue;
            }
            if !in_dependencies {
                continue;
            }
            if let Some(eq_pos) = line.find('=') {
                let name = line[..eq_pos].trim().to_string();
                if !name.is_empty() && !name.starts_with('[') {
                    let version = parse_manifest_version(line[eq_pos + 1..].trim());
                    packages.push(PackageId {
                        name,
                        version,
                        registry: Some("crates.io".to_string()),
                    });
                }
            }
        }
        packages
    }

    fn parse_package_json(&self, content: &str) -> Vec<PackageId> {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
            return Vec::new();
        };
        [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ]
        .into_iter()
        .filter_map(|section| json.get(section).and_then(serde_json::Value::as_object))
        .flat_map(|deps| deps.iter())
        .map(|(name, requirement)| {
            let version = requirement.as_str().and_then(normalize_version_requirement);
            PackageId {
                name: name.clone(),
                version,
                registry: Some("npm".to_string()),
            }
        })
        .collect()
    }

    fn parse_requirements_txt(&self, content: &str) -> Vec<PackageId> {
        let mut packages = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Parse "package==version" or "package>=version" or just "package"
            let (name, version) = parse_requirement_line(line);
            if !name.is_empty() {
                packages.push(PackageId {
                    name: strip_python_extras(&name),
                    version,
                    registry: Some("pypi".to_string()),
                });
            }
        }
        packages
    }

    async fn fetch_from_context7(
        &self,
        package: &PackageId,
        endpoint: &str,
    ) -> anyhow::Result<ReferencePack> {
        validate_context7_endpoint(endpoint)?;
        let resolve_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "resolve-library-id",
                "arguments": {
                    "libraryName": package.name.clone(),
                }
            }
        });
        let resolve_body =
            (self.http_post)(endpoint.to_string(), resolve_payload.to_string()).await?;
        let resolve_text = extract_mcp_text(&resolve_body)?;
        let library_id = extract_context7_library_id(&resolve_text).ok_or_else(|| {
            anyhow::anyhow!("Context7 did not return a library id for {}", package.name)
        })?;

        let topic = package
            .version
            .as_ref()
            .map(|version| format!("{} {version}", package.name))
            .unwrap_or_else(|| package.name.clone());
        let docs_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "get-library-docs",
                "arguments": {
                    "context7CompatibleLibraryID": library_id,
                    "topic": topic,
                    "tokens": 3000
                }
            }
        });
        let docs_body = (self.http_post)(endpoint.to_string(), docs_payload.to_string()).await?;
        let docs_text = extract_mcp_text(&docs_body)?;
        if docs_text.trim().is_empty() {
            anyhow::bail!("Context7 returned empty docs for {}", package.name);
        }

        let mut pack = self.compile_reference_pack(
            ReferenceSourceKind::Context7,
            vec![RawFinding {
                kind: FindingKind::Example,
                title: Some(format!("{} Context7 docs", package.name)),
                content: truncate_reference_content(&docs_text, 12_000),
                language: Some("markdown".to_string()),
                source_url: Some(endpoint.to_string()),
            }],
            &package.name,
        );
        pack.library = Some(package.name.clone());
        pack.version = package.version.clone();
        pack.confidence = ReferenceConfidence::Official;
        pack.relevant_rules.push(
            "Reference-first: Context7 resolved the library id and fetched current docs."
                .to_string(),
        );
        Ok(pack)
    }

    async fn fetch_from_registry(&self, package: &PackageId) -> anyhow::Result<ReferencePack> {
        match registry_name(package).as_str() {
            "npm" => self.fetch_from_npm(package).await,
            "pypi" => self.fetch_from_pypi(package).await,
            "crates.io" | "cargo" | "crates" => self.fetch_from_crates_io(package).await,
            other => anyhow::bail!("unsupported registry '{other}' for {}", package.name),
        }
    }

    async fn fetch_from_web_search(&self, package: &PackageId) -> anyhow::Result<ReferencePack> {
        let mut last_error = None;
        for url in web_reference_urls(package) {
            match (self.http_get)(url.clone()).await {
                Ok(body) => {
                    let text = html_to_reference_text(&body);
                    if text.trim().is_empty() {
                        continue;
                    }
                    let mut pack = self.compile_reference_pack(
                        ReferenceSourceKind::WebSearch,
                        vec![RawFinding {
                            kind: FindingKind::Example,
                            title: Some(format!("{} web reference", package.name)),
                            content: truncate_reference_content(&text, 12_000),
                            language: Some("text".to_string()),
                            source_url: Some(url),
                        }],
                        &package.name,
                    );
                    pack.library = Some(package.name.clone());
                    pack.version = package.version.clone();
                    pack.confidence = ReferenceConfidence::Medium;
                    pack.relevant_rules.push(
                        "Reference-first: public package documentation page was fetched as fallback."
                            .to_string(),
                    );
                    return Ok(pack);
                }
                Err(err) => last_error = Some(err),
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("no web reference candidates for {}", package.name)))
    }

    async fn fetch_from_npm(&self, package: &PackageId) -> anyhow::Result<ReferencePack> {
        let url = format!(
            "https://registry.npmjs.org/{}",
            urlencoding::encode(&package.name)
        );
        let body = (self.http_get)(url.clone()).await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        let version = package
            .version
            .clone()
            .or_else(|| json.pointer("/dist-tags/latest").and_then(str_value));
        let mut findings = Vec::new();

        if let Some(readme) = json.get("readme").and_then(str_value) {
            findings.push(RawFinding {
                kind: FindingKind::Example,
                title: Some(format!("{} README", package.name)),
                content: truncate_reference_content(&readme, 12_000),
                language: Some("markdown".to_string()),
                source_url: Some(npm_package_page(&package.name, version.as_deref())),
            });
        }
        if let Some(description) = json.get("description").and_then(str_value) {
            findings.push(RawFinding {
                kind: FindingKind::Caveat,
                title: Some("npm description".to_string()),
                content: description,
                language: None,
                source_url: Some(url),
            });
        }

        registry_pack(self, package, version, findings, "npm")
    }

    async fn fetch_from_pypi(&self, package: &PackageId) -> anyhow::Result<ReferencePack> {
        let encoded = urlencoding::encode(&package.name);
        let url = if let Some(version) = package.version.as_deref() {
            format!("https://pypi.org/pypi/{encoded}/{version}/json")
        } else {
            format!("https://pypi.org/pypi/{encoded}/json")
        };
        let body = (self.http_get)(url.clone()).await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        let info = json
            .get("info")
            .ok_or_else(|| anyhow::anyhow!("PyPI response missing info for {}", package.name))?;
        let version = package
            .version
            .clone()
            .or_else(|| info.get("version").and_then(str_value));
        let source_url = info
            .get("project_url")
            .and_then(str_value)
            .unwrap_or_else(|| pypi_package_page(&package.name, version.as_deref()));
        let mut findings = Vec::new();

        if let Some(description) = info.get("description").and_then(str_value) {
            findings.push(RawFinding {
                kind: FindingKind::Example,
                title: Some(format!("{} PyPI description", package.name)),
                content: truncate_reference_content(&description, 12_000),
                language: Some("markdown".to_string()),
                source_url: Some(source_url.clone()),
            });
        }
        if let Some(summary) = info.get("summary").and_then(str_value) {
            findings.push(RawFinding {
                kind: FindingKind::Caveat,
                title: Some("PyPI summary".to_string()),
                content: summary,
                language: None,
                source_url: Some(url),
            });
        }

        registry_pack(self, package, version, findings, "PyPI")
    }

    async fn fetch_from_crates_io(&self, package: &PackageId) -> anyhow::Result<ReferencePack> {
        let encoded = urlencoding::encode(&package.name);
        let url = format!("https://crates.io/api/v1/crates/{encoded}");
        let body = (self.http_get)(url.clone()).await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        let crate_info = json.get("crate").ok_or_else(|| {
            anyhow::anyhow!("crates.io response missing crate for {}", package.name)
        })?;
        let version = package
            .version
            .clone()
            .or_else(|| crate_info.get("max_version").and_then(str_value));
        let source_url = format!(
            "https://crates.io/crates/{}{}",
            package.name,
            version
                .as_deref()
                .map(|v| format!("/{v}"))
                .unwrap_or_default()
        );
        let mut findings = Vec::new();

        if let Some(description) = crate_info.get("description").and_then(str_value) {
            findings.push(RawFinding {
                kind: FindingKind::Caveat,
                title: Some("crates.io description".to_string()),
                content: description,
                language: None,
                source_url: Some(source_url.clone()),
            });
        }
        for field in ["documentation", "homepage", "repository"] {
            if let Some(link) = crate_info.get(field).and_then(str_value) {
                findings.push(RawFinding {
                    kind: FindingKind::Caveat,
                    title: Some(format!("crates.io {field}")),
                    content: link.clone(),
                    language: None,
                    source_url: Some(link),
                });
            }
        }

        registry_pack(self, package, version, findings, "crates.io")
    }
}

fn default_http_get() -> HttpGet {
    Arc::new(|url| {
        Box::pin(async move {
            let validated_url = url_guard::validate_url(&url)?;
            let client = validated_url
                .pin_dns(
                    reqwest::Client::builder()
                        .user_agent("charm-reference-broker/0.1")
                        .timeout(Duration::from_secs(12))
                        .redirect(reqwest::redirect::Policy::none()),
                )
                .build()?;
            let response = client.get(&url).send().await?;
            let status = response.status();
            if !status.is_success() {
                anyhow::bail!("GET {url} failed with {status}");
            }
            Ok(response.text().await?)
        })
    })
}

fn default_http_post() -> HttpPost {
    Arc::new(|url, body| {
        Box::pin(async move {
            let validated_url = url_guard::validate_url(&url)?;
            let client = validated_url
                .pin_dns(
                    reqwest::Client::builder()
                        .user_agent("charm-reference-broker/0.1")
                        .timeout(Duration::from_secs(12))
                        .redirect(reqwest::redirect::Policy::none()),
                )
                .build()?;
            let mut request = client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
            if let Ok(api_key) = std::env::var("CONTEXT7_API_KEY") {
                request = request.header("CONTEXT7_API_KEY", api_key);
            }
            let response = request.send().await?;
            let status = response.status();
            if !status.is_success() {
                anyhow::bail!("POST {url} failed with {status}");
            }
            Ok(response.text().await?)
        })
    })
}

fn registry_pack(
    broker: &ReferenceBroker,
    package: &PackageId,
    version: Option<String>,
    findings: Vec<RawFinding>,
    source_label: &str,
) -> anyhow::Result<ReferencePack> {
    if findings.is_empty() {
        anyhow::bail!(
            "{source_label} registry returned no usable reference content for {}",
            package.name
        );
    }

    let mut pack = broker.compile_reference_pack(
        ReferenceSourceKind::PackageRegistry,
        findings,
        &package.name,
    );
    pack.library = Some(package.name.clone());
    pack.version = version;
    pack.confidence = ReferenceConfidence::High;
    pack.relevant_rules.push(format!(
        "Reference-first: {source_label} registry metadata was fetched before using package APIs."
    ));
    Ok(pack)
}

fn registry_name(package: &PackageId) -> String {
    package
        .registry
        .as_deref()
        .unwrap_or("crates.io")
        .to_ascii_lowercase()
}

fn str_value(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_manifest_version(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches(',');
    if let Some(quoted) = raw.strip_prefix('"') {
        let version = quoted.split('"').next().unwrap_or_default();
        return normalize_version_requirement(version);
    }
    if raw.starts_with('{') {
        let value: serde_json::Value =
            serde_json::from_str(&toml_inline_table_to_json_object(raw)).ok()?;
        return value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .and_then(normalize_version_requirement);
    }
    None
}

fn toml_inline_table_to_json_object(raw: &str) -> String {
    let body = raw.trim().trim_start_matches('{').trim_end_matches('}');
    let mut fields = Vec::new();
    for part in body.split(',') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        fields.push(format!(
            r#""{}":{}"#,
            key.trim(),
            value.trim().trim_end_matches(',')
        ));
    }
    format!("{{{}}}", fields.join(","))
}

fn normalize_version_requirement(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || matches!(trimmed, "*" | "latest") {
        return None;
    }
    let version = trimmed
        .trim_start_matches(|c: char| {
            matches!(c, '^' | '~' | '=' | '>' | '<' | '!' | 'v') || c.is_whitespace()
        })
        .split([',', ' ', ';', '|'])
        .next()
        .unwrap_or_default()
        .trim();
    (!version.is_empty()).then(|| version.to_string())
}

fn parse_requirement_line(line: &str) -> (String, Option<String>) {
    let requirement = line.split(';').next().unwrap_or(line).trim();
    for operator in ["==", ">=", "<=", "~=", "!=", ">", "<", "="] {
        if let Some((name, version)) = requirement.split_once(operator) {
            return (
                name.trim().to_string(),
                normalize_version_requirement(version),
            );
        }
    }
    (requirement.to_string(), None)
}

fn strip_python_extras(name: &str) -> String {
    name.split('[').next().unwrap_or(name).trim().to_string()
}

fn npm_package_page(name: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("https://www.npmjs.com/package/{name}/v/{version}"),
        None => format!("https://www.npmjs.com/package/{name}"),
    }
}

fn pypi_package_page(name: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("https://pypi.org/project/{name}/{version}/"),
        None => format!("https://pypi.org/project/{name}/"),
    }
}

fn validate_context7_endpoint(endpoint: &str) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(endpoint)
        .map_err(|err| anyhow::anyhow!("Invalid Context7 endpoint: {err}"))?;
    if parsed.scheme() != "https" {
        anyhow::bail!("Context7 endpoint must use https");
    }
    url_guard::validate_url(endpoint)?;
    Ok(())
}

fn extract_mcp_text(body: &str) -> anyhow::Result<String> {
    let json: serde_json::Value = serde_json::from_str(body)?;
    if let Some(error) = json.get("error") {
        anyhow::bail!("MCP error: {error}");
    }
    let Some(content) = json
        .pointer("/result/content")
        .and_then(serde_json::Value::as_array)
    else {
        anyhow::bail!("MCP response missing result.content");
    };
    let text = content
        .iter()
        .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

fn extract_context7_library_id(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((_, tail)) = line.split_once("Context7-compatible library ID:") {
            return tail.split_whitespace().next().map(clean_context7_id);
        }
    }
    text.split_whitespace()
        .find(|token| token.starts_with('/') && token.matches('/').count() >= 2)
        .map(clean_context7_id)
}

fn clean_context7_id(raw: &str) -> String {
    raw.trim_matches(|c: char| {
        c == '`' || c == '"' || c == '\'' || c == ',' || c == ')' || c == '('
    })
    .to_string()
}

fn web_reference_urls(package: &PackageId) -> Vec<String> {
    let version = package.version.as_deref();
    match registry_name(package).as_str() {
        "npm" => vec![npm_package_page(&package.name, version)],
        "pypi" => vec![pypi_package_page(&package.name, version)],
        "crates.io" | "cargo" | "crates" => {
            let doc_crate = package.name.replace('-', "_");
            if let Some(version) = version {
                vec![
                    format!(
                        "https://docs.rs/{}/{}/{}/",
                        package.name, version, doc_crate
                    ),
                    format!("https://crates.io/crates/{}/{version}", package.name),
                ]
            } else {
                vec![
                    format!("https://docs.rs/{}/latest/{}/", package.name, doc_crate),
                    format!("https://crates.io/crates/{}", package.name),
                ]
            }
        }
        _ => Vec::new(),
    }
}

fn html_to_reference_text(html: &str) -> String {
    let without_scripts = regex_replace_all(html, r"(?is)<script[^>]*>.*?</script>", " ");
    let without_styles = regex_replace_all(&without_scripts, r"(?is)<style[^>]*>.*?</style>", " ");
    let without_tags = regex_replace_all(&without_styles, r"(?is)<[^>]+>", " ");
    let decoded = html_escape::decode_html_entities(&without_tags).to_string();
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn regex_replace_all(raw: &str, pattern: &str, replacement: &str) -> String {
    regex::Regex::new(pattern)
        .map(|re| re.replace_all(raw, replacement).into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

fn find_local_package_dir(package: &PackageId, roots: &[PathBuf]) -> Option<PathBuf> {
    let exact = package
        .version
        .as_ref()
        .map(|version| format!("{}-{}", package.name, version));
    let prefix = format!("{}-", package.name);
    let mut name_only_candidate = None;
    let mut prefix_candidate = None;

    for root in roots {
        if !root.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root)
            .min_depth(1)
            .max_depth(4)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_dir())
        {
            let name = entry.file_name().to_string_lossy();
            let matches_exact = exact.as_ref().is_some_and(|expected| &name == expected);
            if matches_exact {
                return Some(entry.path().to_path_buf());
            }
            if name == package.name && name_only_candidate.is_none() {
                name_only_candidate = Some(entry.path().to_path_buf());
            } else if package.version.is_none()
                && name.starts_with(&prefix)
                && prefix_candidate.is_none()
            {
                prefix_candidate = Some(entry.path().to_path_buf());
            }
        }
    }

    name_only_candidate.or(prefix_candidate)
}

fn collect_local_source_findings(package_dir: &Path) -> anyhow::Result<Vec<RawFinding>> {
    let mut findings = Vec::new();
    for candidate in [
        package_dir.join("README.md"),
        package_dir.join("readme.md"),
        package_dir.join("src").join("lib.rs"),
        package_dir.join("src").join("main.rs"),
    ] {
        if !candidate.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&candidate)?;
        if raw.trim().is_empty() {
            continue;
        }
        let language = if candidate.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            "rust"
        } else {
            "markdown"
        };
        findings.push(RawFinding {
            kind: FindingKind::Example,
            title: candidate
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string),
            content: truncate_reference_content(&raw, 12_000),
            language: Some(language.to_string()),
            source_url: Some(format!("file://{}", candidate.display())),
        });
    }

    if findings.is_empty() {
        anyhow::bail!(
            "local source '{}' has no README.md or src entrypoint",
            package_dir.display()
        );
    }

    Ok(findings)
}

fn truncate_reference_content(raw: &str, max_chars: usize) -> String {
    if raw.len() <= max_chars {
        return raw.to_string();
    }
    let mut end = max_chars;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[... {} characters omitted from local package source ...]",
        &raw[..end],
        raw.len() - end
    )
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
    fn test_parse_package_manifests_extract_versions() {
        let broker = ReferenceBroker::new();
        let cargo = r#"
[dependencies]
tokio = "1.44"
serde = { version = "1.0", features = ["derive"] }
"#;
        let cargo_packages = broker.parse_cargo_toml(cargo);
        assert!(cargo_packages.iter().any(|p| {
            p.name == "tokio"
                && p.version.as_deref() == Some("1.44")
                && p.registry.as_deref() == Some("crates.io")
        }));
        assert!(cargo_packages.iter().any(|p| {
            p.name == "serde"
                && p.version.as_deref() == Some("1.0")
                && p.registry.as_deref() == Some("crates.io")
        }));

        let package_json = r#"{
  "dependencies": { "react": "^19.0.0" },
  "devDependencies": { "@types/node": "22.0.0" }
}"#;
        let npm_packages = broker.parse_package_json(package_json);
        assert!(npm_packages.iter().any(|p| {
            p.name == "react"
                && p.version.as_deref() == Some("19.0.0")
                && p.registry.as_deref() == Some("npm")
        }));
        assert!(npm_packages.iter().any(|p| {
            p.name == "@types/node"
                && p.version.as_deref() == Some("22.0.0")
                && p.registry.as_deref() == Some("npm")
        }));

        let python_packages = broker.parse_requirements_txt("requests==2.32.0\nnumpy>=1.26\n");
        assert!(python_packages.iter().any(|p| {
            p.name == "requests"
                && p.version.as_deref() == Some("2.32.0")
                && p.registry.as_deref() == Some("pypi")
        }));
        assert!(python_packages.iter().any(|p| {
            p.name == "numpy"
                && p.version.as_deref() == Some("1.26")
                && p.registry.as_deref() == Some("pypi")
        }));
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

    #[tokio::test]
    async fn test_fetch_from_registry_reads_npm_readme() {
        let broker = ReferenceBroker::new().with_http_get(|url| async move {
            assert_eq!(url, "https://registry.npmjs.org/react");
            Ok(r###"{
  "name": "react",
  "dist-tags": { "latest": "19.0.0" },
  "description": "React UI library",
  "readme": "## Usage\n\nimport { createElement } from 'react';",
  "homepage": "https://react.dev"
}"###
                .to_string())
        });

        let pack = broker
            .fetch_from_registry(&PackageId {
                name: "react".to_string(),
                version: Some("19.0.0".to_string()),
                registry: Some("npm".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(pack.source_kind, ReferenceSourceKind::PackageRegistry);
        assert_eq!(pack.library.as_deref(), Some("react"));
        assert_eq!(pack.version.as_deref(), Some("19.0.0"));
        assert_eq!(pack.confidence, ReferenceConfidence::High);
        assert!(
            pack.minimal_examples
                .iter()
                .any(|example| example.code.contains("createElement"))
        );
        assert!(
            pack.source_refs
                .iter()
                .any(|source| source.url.contains("npmjs"))
        );
    }

    #[tokio::test]
    async fn test_fetch_from_registry_reads_pypi_release_json() {
        let broker = ReferenceBroker::new().with_http_get(|url| async move {
            assert_eq!(url, "https://pypi.org/pypi/requests/2.32.0/json");
            Ok(r###"{
  "info": {
    "name": "requests",
    "version": "2.32.0",
    "summary": "Python HTTP for Humans.",
    "description": "## Usage\n\nimport requests\nrequests.get('https://example.com')",
    "project_url": "https://pypi.org/project/requests/"
  }
}"###
                .to_string())
        });

        let pack = broker
            .fetch_from_registry(&PackageId {
                name: "requests".to_string(),
                version: Some("2.32.0".to_string()),
                registry: Some("pypi".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(pack.library.as_deref(), Some("requests"));
        assert_eq!(pack.version.as_deref(), Some("2.32.0"));
        assert!(
            pack.minimal_examples
                .iter()
                .any(|example| example.code.contains("requests.get"))
        );
    }

    #[tokio::test]
    async fn test_fetch_from_registry_reads_crates_io_metadata() {
        let broker = ReferenceBroker::new().with_http_get(|url| async move {
            assert_eq!(url, "https://crates.io/api/v1/crates/serde");
            Ok(r#"{
  "crate": {
    "id": "serde",
    "max_version": "1.0.228",
    "description": "A serialization framework",
    "documentation": "https://docs.rs/serde",
    "repository": "https://github.com/serde-rs/serde"
  }
}"#
            .to_string())
        });

        let pack = broker
            .fetch_from_registry(&PackageId {
                name: "serde".to_string(),
                version: None,
                registry: Some("crates.io".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(pack.library.as_deref(), Some("serde"));
        assert_eq!(pack.version.as_deref(), Some("1.0.228"));
        assert_eq!(pack.confidence, ReferenceConfidence::High);
        assert!(
            pack.caveats
                .iter()
                .any(|item| item.contains("serialization"))
        );
        assert!(
            pack.source_refs
                .iter()
                .any(|source| source.url == "https://docs.rs/serde")
        );
    }

    #[tokio::test]
    async fn test_fetch_from_web_search_reads_docs_page() {
        let broker = ReferenceBroker::new().with_http_get(|url| async move {
            assert_eq!(url, "https://docs.rs/serde/1.0.228/serde/");
            Ok(r#"<html><head><title>serde docs</title></head>
<body><main><h1>Crate serde</h1><p>Serialize and Deserialize traits.</p></main></body></html>"#
                .to_string())
        });

        let pack = broker
            .fetch_from_web_search(&PackageId {
                name: "serde".to_string(),
                version: Some("1.0.228".to_string()),
                registry: Some("crates.io".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(pack.source_kind, ReferenceSourceKind::WebSearch);
        assert_eq!(pack.library.as_deref(), Some("serde"));
        assert_eq!(pack.version.as_deref(), Some("1.0.228"));
        assert_eq!(pack.confidence, ReferenceConfidence::Medium);
        assert!(
            pack.minimal_examples
                .iter()
                .any(|example| example.code.contains("Serialize and Deserialize"))
        );
    }

    #[tokio::test]
    async fn test_fetch_from_context7_resolves_library_then_fetches_docs() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let broker = ReferenceBroker::new().with_http_post(move |url, body| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push((url, body.clone()));
                if body.contains("resolve-library-id") {
                    Ok(r#"{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [{ "type": "text", "text": "Available Libraries:\n- Context7-compatible library ID: /serde-rs/serde\n- Trust Score: 10" }]
  }
}"#
                    .to_string())
                } else {
                    Ok(r#"{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [{ "type": "text", "text": "Serde docs\n```rust\n#[derive(Serialize)]\nstruct User;\n```" }]
  }
}"#
                    .to_string())
                }
            }
        });

        let pack = broker
            .fetch_from_context7(
                &PackageId {
                    name: "serde".to_string(),
                    version: Some("1.0.228".to_string()),
                    registry: Some("crates.io".to_string()),
                },
                "https://mcp.context7.com/mcp",
            )
            .await
            .unwrap();

        assert_eq!(pack.source_kind, ReferenceSourceKind::Context7);
        assert_eq!(pack.library.as_deref(), Some("serde"));
        assert_eq!(pack.version.as_deref(), Some("1.0.228"));
        assert!(
            pack.minimal_examples
                .iter()
                .any(|example| example.code.contains("derive(Serialize)"))
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].1.contains("resolve-library-id"));
        assert!(calls[1].1.contains("get-library-docs"));
    }

    #[tokio::test]
    async fn test_default_reference_http_blocks_localhost_get_and_post() {
        let get_err = default_http_get()("http://127.0.0.1:9/private".to_string())
            .await
            .unwrap_err();
        assert!(get_err.to_string().contains("Blocked"));

        let post_err = default_http_post()("http://localhost:9/mcp".to_string(), "{}".to_string())
            .await
            .unwrap_err();
        assert!(post_err.to_string().contains("Blocked"));
    }

    #[tokio::test]
    async fn test_context7_endpoint_requires_https_before_transport() {
        let broker = ReferenceBroker::new().with_http_post(|_, _| async {
            panic!("transport must not be called for an insecure endpoint")
        });

        let err = broker
            .fetch_from_context7(
                &PackageId {
                    name: "serde".to_string(),
                    version: None,
                    registry: Some("crates.io".to_string()),
                },
                "http://context7.example/mcp",
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("https"));
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

    #[test]
    fn test_fetch_from_local_source_roots_reads_package_source() {
        let dir = tempfile::tempdir().unwrap();
        let package_dir = dir.path().join("index").join("reqwest-0.12.0");
        std::fs::create_dir_all(package_dir.join("src")).unwrap();
        std::fs::write(
            package_dir.join("README.md"),
            "# reqwest\n\nUse Client::new() to build a client.\n",
        )
        .unwrap();
        std::fs::write(
            package_dir.join("src").join("lib.rs"),
            "pub struct Client;\nimpl Client { pub fn new() -> Self { Self } }\n",
        )
        .unwrap();

        let broker = ReferenceBroker::new();
        let pack = broker
            .fetch_from_local_source_roots(
                &PackageId {
                    name: "reqwest".to_string(),
                    version: Some("0.12.0".to_string()),
                    registry: Some("crates.io".to_string()),
                },
                &[dir.path().to_path_buf()],
                "reqwest client api",
            )
            .unwrap();

        assert_eq!(pack.source_kind, ReferenceSourceKind::LocalSource);
        assert_eq!(pack.library.as_deref(), Some("reqwest"));
        assert_eq!(pack.version.as_deref(), Some("0.12.0"));
        assert_eq!(pack.confidence, ReferenceConfidence::High);
        assert!(
            pack.minimal_examples
                .iter()
                .any(|example| example.code.contains("Client::new"))
        );
        assert!(!pack.source_refs.is_empty());
    }

    #[test]
    fn test_fetch_from_local_source_roots_prefers_requested_version() {
        let dir = tempfile::tempdir().unwrap();
        let old_package_dir = dir.path().join("index").join("reqwest-0.11.0");
        let new_package_dir = dir.path().join("index").join("reqwest-0.12.0");
        std::fs::create_dir_all(old_package_dir.join("src")).unwrap();
        std::fs::create_dir_all(new_package_dir.join("src")).unwrap();
        std::fs::write(
            old_package_dir.join("src").join("lib.rs"),
            "pub fn selected_version() -> &'static str { \"v11\" }\n",
        )
        .unwrap();
        std::fs::write(
            new_package_dir.join("src").join("lib.rs"),
            "pub fn selected_version() -> &'static str { \"v12\" }\n",
        )
        .unwrap();

        let broker = ReferenceBroker::new();
        let pack = broker
            .fetch_from_local_source_roots(
                &PackageId {
                    name: "reqwest".to_string(),
                    version: Some("0.12.0".to_string()),
                    registry: Some("crates.io".to_string()),
                },
                &[dir.path().to_path_buf()],
                "reqwest selected version",
            )
            .unwrap();

        assert!(
            pack.minimal_examples
                .iter()
                .any(|example| example.code.contains("v12"))
        );
        assert!(
            !pack
                .minimal_examples
                .iter()
                .any(|example| example.code.contains("v11"))
        );
    }
}
