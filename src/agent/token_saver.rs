use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Source kind for minification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Cargo build/test output
    CargoOutput,
    /// Rustc compiler output
    RustcOutput,
    /// Test framework output
    TestOutput,
    /// Grep/search results
    SearchResults,
    /// Code snippet from file
    CodeSnippet,
    /// Documentation/reference
    Documentation,
    /// Command stdout/stderr
    CommandOutput,
}

/// What to preserve during minification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreservePolicy {
    /// Keep line numbers
    pub line_numbers: bool,
    /// Keep error messages
    pub error_messages: bool,
    /// Keep file paths
    pub file_paths: bool,
    /// Keep code spans
    pub code_spans: bool,
    /// Keep first N and last M lines
    pub head_lines: Option<usize>,
    pub tail_lines: Option<usize>,
}

impl Default for PreservePolicy {
    fn default() -> Self {
        Self {
            line_numbers: true,
            error_messages: true,
            file_paths: true,
            code_spans: true,
            head_lines: Some(50),
            tail_lines: Some(20),
        }
    }
}

/// Token budget for minification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub max_tokens: usize,
    pub target_tokens: usize,
    pub min_tokens: usize,
}

impl TokenBudget {
    pub fn new(max: usize) -> Self {
        Self {
            max_tokens: max,
            target_tokens: max * 8 / 10, // 80% of max
            min_tokens: max * 5 / 10,    // 50% of max
        }
    }
}

/// Reference to raw source for traceability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRef {
    pub source_id: String,
    pub source_kind: SourceKind,
    pub original_size: usize,
    pub storage_path: Option<String>,
}

/// Evidence extracted from minified content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: EvidenceKind,
    pub location: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Error,
    Warning,
    Definition,
    Usage,
    TestResult,
}

/// What was omitted during minification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Omission {
    pub kind: OmissionKind,
    pub count: usize,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionKind {
    DuplicateLines,
    SimilarResults,
    VerboseOutput,
    ElidedLines,
    RedundantContext,
}

/// Request to minify content
#[derive(Debug, Clone)]
pub struct MinifyRequest {
    pub source_kind: SourceKind,
    pub raw: String,
    pub budget: TokenBudget,
    pub preserve: PreservePolicy,
}

/// Minified view of content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinifiedView {
    pub text: String,
    pub evidence: Vec<EvidenceRef>,
    pub omissions: Vec<Omission>,
    pub raw_ref: Option<RawRef>,
    pub token_estimate: usize,
}

/// TokenSaver that minifies various outputs while preserving key information
pub struct TokenSaver {
    /// Storage for raw outputs (separate from minified views)
    raw_storage: RawOutputStore,
    /// Dedupe tracking
    seen_hashes: HashSet<u64>,
}

impl TokenSaver {
    pub fn new() -> Self {
        Self {
            raw_storage: RawOutputStore::new(),
            seen_hashes: HashSet::new(),
        }
    }

    /// Minify content based on source kind and budget
    pub fn minify(&mut self, request: MinifyRequest) -> MinifiedView {
        // Store raw first
        let raw_ref = self
            .raw_storage
            .store(&request.raw, request.source_kind.clone());

        // Check for exact duplicate
        let hash = Self::compute_hash(&request.raw);
        let is_duplicate = !self.seen_hashes.insert(hash);

        let (text, evidence, omissions) = match request.source_kind {
            SourceKind::CargoOutput => self.minify_cargo(&request),
            SourceKind::RustcOutput => self.minify_rustc(&request),
            SourceKind::TestOutput => self.minify_test(&request),
            SourceKind::SearchResults => self.minify_search(&request),
            SourceKind::CodeSnippet => self.minify_code(&request),
            SourceKind::Documentation => self.minify_docs(&request),
            SourceKind::CommandOutput => self.minify_command(&request),
        };

        let mut final_omissions = omissions;

        if is_duplicate {
            final_omissions.push(Omission {
                kind: OmissionKind::DuplicateLines,
                count: 0,
                description: "Exact duplicate of previously seen content".to_string(),
            });
        }

        let token_estimate = Self::estimate_tokens(&text);

        MinifiedView {
            text,
            evidence,
            omissions: final_omissions,
            raw_ref: Some(raw_ref),
            token_estimate,
        }
    }

    /// Minify cargo output (build/test)
    fn minify_cargo(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        let lines: Vec<&str> = request.raw.lines().collect();
        let mut evidence = Vec::new();
        let mut omissions = Vec::new();

        // Extract errors and warnings
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.contains("error[") || line.starts_with("error:") {
                // Capture error with context
                let mut error_block = vec![line];
                let mut j = i + 1;
                while j < lines.len() && lines[j].starts_with("  ") {
                    error_block.push(lines[j]);
                    j += 1;
                }
                evidence.push(EvidenceRef {
                    kind: EvidenceKind::Error,
                    location: format!("line {}", i + 1),
                    content: error_block.join("\n"),
                });
            } else if line.contains("warning[") || line.starts_with("warning:") {
                evidence.push(EvidenceRef {
                    kind: EvidenceKind::Warning,
                    location: format!("line {}", i + 1),
                    content: line.to_string(),
                });
            }
            i += 1;
        }

        // Head/tail truncation
        let head = request.preserve.head_lines.unwrap_or(30);
        let tail = request.preserve.tail_lines.unwrap_or(10);

        let minified = if lines.len() > head + tail + 10 {
            let head_lines: Vec<_> = lines.iter().take(head).copied().collect();
            let tail_lines: Vec<_> = lines.iter().skip(lines.len() - tail).copied().collect();

            omissions.push(Omission {
                kind: OmissionKind::ElidedLines,
                count: lines.len() - head - tail,
                description: format!(
                    "Elided {} lines of verbose output",
                    lines.len() - head - tail
                ),
            });

            format!(
                "{}}}\n\n... {} lines omitted ...\n\n{}",
                head_lines.join("\n"),
                lines.len() - head - tail,
                tail_lines.join("\n")
            )
        } else {
            lines.join("\n")
        };

        (minified, evidence, omissions)
    }

    /// Minify rustc compiler output
    fn minify_rustc(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        // Similar to cargo but focused on single file diagnostics
        self.minify_cargo(request) // Reuse logic for now
    }

    /// Minify test output
    fn minify_test(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        let lines: Vec<&str> = request.raw.lines().collect();
        let mut evidence = Vec::new();
        let mut omissions = Vec::new();

        // Extract test results
        let mut result_summary = String::new();
        let mut failed_tests = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            if line.contains("test result:") {
                result_summary = line.to_string();
            }
            if line.starts_with("test ") && line.contains(" ... FAILED") {
                failed_tests.push((i + 1, line.to_string()));
                evidence.push(EvidenceRef {
                    kind: EvidenceKind::TestResult,
                    location: format!("line {}", i + 1),
                    content: line.to_string(),
                });
            }
            if line.starts_with("---- ") && line.contains(" stdout ----") {
                // Capture test failure output
                evidence.push(EvidenceRef {
                    kind: EvidenceKind::Error,
                    location: format!("line {}", i + 1),
                    content: line.to_string(),
                });
            }
        }

        // Keep summary + failed tests + some context
        let mut kept = vec![result_summary];
        for (line_num, failed) in failed_tests {
            kept.push(format!("Line {}: {}", line_num, failed));
        }

        if lines.len() > kept.len() + 10 {
            omissions.push(Omission {
                kind: OmissionKind::VerboseOutput,
                count: lines.len() - kept.len(),
                description: "Elided passing test details".to_string(),
            });
        }

        (kept.join("\n"), evidence, omissions)
    }

    /// Minify search/grep results
    fn minify_search(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        let lines: Vec<&str> = request.raw.lines().collect();
        let mut evidence = Vec::new();
        let mut omissions = Vec::new();
        let mut seen_files: HashSet<&str> = HashSet::new();

        let mut deduped = Vec::new();
        let mut duplicates = 0;

        for line in &lines {
            // Extract file path from grep-style output
            if let Some(colon_pos) = line.find(':') {
                let file_path = &line[..colon_pos];
                if seen_files.contains(file_path) {
                    // Same file, likely similar context
                    duplicates += 1;
                    continue;
                }
                seen_files.insert(file_path);
            }
            deduped.push(*line);
        }

        if duplicates > 0 {
            omissions.push(Omission {
                kind: OmissionKind::SimilarResults,
                count: duplicates,
                description: format!("Omitted {} similar results from same files", duplicates),
            });
        }

        // Limit total results
        let max_results = 50;
        let final_lines = if deduped.len() > max_results {
            omissions.push(Omission {
                kind: OmissionKind::RedundantContext,
                count: deduped.len() - max_results,
                description: format!("Limited to {} results", max_results),
            });
            deduped[..max_results].to_vec()
        } else {
            deduped
        };

        for line in &final_lines {
            evidence.push(EvidenceRef {
                kind: EvidenceKind::Usage,
                location: "search result".to_string(),
                content: line.to_string(),
            });
        }

        (final_lines.join("\n"), evidence, omissions)
    }

    /// Minify code snippets
    fn minify_code(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        let lines: Vec<&str> = request.raw.lines().collect();
        let mut omissions = Vec::new();

        // Preserve with line numbers
        let minified: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:4} | {}", i + 1, line))
            .collect();

        if lines.len() > 100 {
            omissions.push(Omission {
                kind: OmissionKind::ElidedLines,
                count: 0,
                description: format!("Full snippet has {} lines", lines.len()),
            });
        }

        (minified.join("\n"), Vec::new(), omissions)
    }

    /// Minify documentation
    fn minify_docs(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        // Extract code blocks and key sections
        let mut in_code_block = false;
        let mut code_blocks = Vec::new();
        let mut current_block = String::new();

        for line in request.raw.lines() {
            if line.trim().starts_with("```") {
                if in_code_block {
                    code_blocks.push(current_block.clone());
                    current_block.clear();
                }
                in_code_block = !in_code_block;
            } else if in_code_block {
                current_block.push_str(line);
                current_block.push('\n');
            }
        }

        let mut omissions = Vec::new();
        omissions.push(Omission {
            kind: OmissionKind::RedundantContext,
            count: 0,
            description: "Elided prose, kept code examples".to_string(),
        });

        (code_blocks.join("\n\n"), Vec::new(), omissions)
    }

    /// Minify generic command output
    fn minify_command(&self, request: &MinifyRequest) -> (String, Vec<EvidenceRef>, Vec<Omission>) {
        // Default: head/tail truncation
        let lines: Vec<&str> = request.raw.lines().collect();
        let head = request.preserve.head_lines.unwrap_or(20);
        let tail = request.preserve.tail_lines.unwrap_or(5);

        let mut omissions = Vec::new();

        let minified = if lines.len() > head + tail + 5 {
            let head_str: Vec<_> = lines.iter().take(head).copied().collect();
            let tail_str: Vec<_> = lines.iter().skip(lines.len() - tail).copied().collect();

            omissions.push(Omission {
                kind: OmissionKind::ElidedLines,
                count: lines.len() - head - tail,
                description: format!("Elided {} lines", lines.len() - head - tail),
            });

            format!(
                "{}}}\n\n... {} lines ...\n\n{}",
                head_str.join("\n"),
                lines.len() - head - tail,
                tail_str.join("\n")
            )
        } else {
            request.raw.clone()
        };

        (minified, Vec::new(), omissions)
    }

    /// Simple hash for dedupe detection
    fn compute_hash(content: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    /// Estimate token count (rough approximation: ~4 chars per token)
    fn estimate_tokens(text: &str) -> usize {
        text.len() / 4
    }
}

/// Storage for raw outputs
pub struct RawOutputStore {
    storage_dir: Option<std::path::PathBuf>,
}

impl RawOutputStore {
    fn new() -> Self {
        Self {
            storage_dir: std::env::var("CHARM_RAW_STORAGE")
                .ok()
                .map(std::path::PathBuf::from),
        }
    }

    fn store(&self, content: &str, source_kind: SourceKind) -> RawRef {
        let source_id = format!("{}_{}", source_kind.as_str(), uuid::Uuid::new_v4());
        let original_size = content.len();

        let storage_path = if let Some(ref dir) = self.storage_dir {
            let path = dir.join(&source_id);
            // In real impl, would write to file
            Some(path.to_string_lossy().to_string())
        } else {
            None
        };

        RawRef {
            source_id,
            source_kind,
            original_size,
            storage_path,
        }
    }
}

impl SourceKind {
    fn as_str(&self) -> &'static str {
        match self {
            SourceKind::CargoOutput => "cargo",
            SourceKind::RustcOutput => "rustc",
            SourceKind::TestOutput => "test",
            SourceKind::SearchResults => "search",
            SourceKind::CodeSnippet => "code",
            SourceKind::Documentation => "docs",
            SourceKind::CommandOutput => "command",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_budget_calculation() {
        let budget = TokenBudget::new(1000);
        assert_eq!(budget.max_tokens, 1000);
        assert_eq!(budget.target_tokens, 800);
        assert_eq!(budget.min_tokens, 500);
    }

    #[test]
    fn test_preserve_policy_default() {
        let policy = PreservePolicy::default();
        assert!(policy.line_numbers);
        assert!(policy.error_messages);
        assert!(policy.file_paths);
        assert!(policy.code_spans);
        assert_eq!(policy.head_lines, Some(50));
        assert_eq!(policy.tail_lines, Some(20));
    }

    #[test]
    fn test_minify_cargo_extracts_errors() {
        let mut saver = TokenSaver::new();
        let cargo_output = r#"
Compiling mycrate v0.1.0
error[E0425]: cannot find value `x` in this scope
  --> src/main.rs:10:5
   |
10 |     println!("{}", x);
   |                    ^ not found in this scope

error[E0308]: mismatched types
  --> src/lib.rs:20:10
   |
20 |     let y: i32 = "hello";
   |          ---   ^^^^^^^ expected `i32`, found `&str`
   |          |
   |          expected due to this

Finished with errors
        "#
        .trim();

        let request = MinifyRequest {
            source_kind: SourceKind::CargoOutput,
            raw: cargo_output.to_string(),
            budget: TokenBudget::new(500),
            preserve: PreservePolicy::default(),
        };

        let result = saver.minify(request);

        assert!(!result.evidence.is_empty());
        assert!(
            result
                .evidence
                .iter()
                .any(|e| matches!(e.kind, EvidenceKind::Error))
        );
        assert!(result.text.contains("error[E0425]") || result.text.contains("error[E0308]"));
    }

    #[test]
    fn test_minify_test_extracts_failures() {
        let mut saver = TokenSaver::new();
        let test_output = r#"
running 3 tests
test tests::test_pass ... ok
test tests::test_fail ... FAILED

test result: FAILED. 2 passed; 1 failed
        "#
        .trim();

        let request = MinifyRequest {
            source_kind: SourceKind::TestOutput,
            raw: test_output.to_string(),
            budget: TokenBudget::new(200),
            preserve: PreservePolicy::default(),
        };

        let result = saver.minify(request);

        assert!(result.text.contains("test result:"));
        assert!(result.text.contains("FAILED"));
    }

    #[test]
    fn test_minify_search_dedupes() {
        let mut saver = TokenSaver::new();
        let search_output = r#"
src/main.rs:10:fn main() {
src/main.rs:20:fn helper() {
src/main.rs:30:fn another() {
src/lib.rs:5:pub fn lib_fn() {
src/lib.rs:15:pub fn other_fn() {
        "#
        .trim();

        let request = MinifyRequest {
            source_kind: SourceKind::SearchResults,
            raw: search_output.to_string(),
            budget: TokenBudget::new(200),
            preserve: PreservePolicy::default(),
        };

        let result = saver.minify(request);

        // Should keep results from both files
        assert!(result.text.contains("src/main.rs"));
        assert!(result.text.contains("src/lib.rs"));
    }

    #[test]
    fn test_minify_code_preserves_line_numbers() {
        let mut saver = TokenSaver::new();
        let code = r#"
fn main() {
    let x = 42;
    println!("{}", x);
}
        "#
        .trim();

        let request = MinifyRequest {
            source_kind: SourceKind::CodeSnippet,
            raw: code.to_string(),
            budget: TokenBudget::new(100),
            preserve: PreservePolicy::default(),
        };

        let result = saver.minify(request);

        assert!(result.text.contains("   1 |"));
        assert!(result.text.contains("   2 |"));
        assert!(result.text.contains("   3 |"));
    }

    #[test]
    fn test_duplicate_detection() {
        let mut saver = TokenSaver::new();
        let content = "Some output\nwith multiple\nlines".to_string();

        let request1 = MinifyRequest {
            source_kind: SourceKind::CommandOutput,
            raw: content.clone(),
            budget: TokenBudget::new(100),
            preserve: PreservePolicy::default(),
        };

        let request2 = MinifyRequest {
            source_kind: SourceKind::CommandOutput,
            raw: content,
            budget: TokenBudget::new(100),
            preserve: PreservePolicy::default(),
        };

        let result1 = saver.minify(request1);
        let result2 = saver.minify(request2);

        assert!(
            !result1
                .omissions
                .iter()
                .any(|o| matches!(o.kind, OmissionKind::DuplicateLines))
        );
        assert!(
            result2
                .omissions
                .iter()
                .any(|o| matches!(o.kind, OmissionKind::DuplicateLines))
        );
    }

    #[test]
    fn test_minify_docs_extracts_code_blocks() {
        let mut saver = TokenSaver::new();
        let docs = r#"
# My Library

This is documentation prose.

```rust
fn example() {
    let x = 42;
}
```

More prose here.

```python
def python_example():
    return 42
```
        "#
        .trim();

        let request = MinifyRequest {
            source_kind: SourceKind::Documentation,
            raw: docs.to_string(),
            budget: TokenBudget::new(200),
            preserve: PreservePolicy::default(),
        };

        let result = saver.minify(request);

        assert!(result.text.contains("fn example()"));
        assert!(result.text.contains("python_example"));
        assert!(!result.text.contains("This is documentation prose"));
    }

    #[test]
    fn test_token_estimate() {
        // ~4 chars per token
        let text = "x".repeat(400);
        let estimate = TokenSaver::estimate_tokens(&text);
        assert_eq!(estimate, 100);
    }
}
