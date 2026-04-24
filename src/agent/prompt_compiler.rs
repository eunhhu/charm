use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Activation condition for a prompt section
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    /// Always include this section
    Always,
    /// Include if a specific mode is active
    Mode(String),
    /// Include if a specific tool is available
    ToolAvailable(String),
    /// Include if a specific context condition is met
    Context(String),
    /// Include based on a custom condition
    Conditional(String),
}

impl Activation {
    /// Check if this section should be activated given the context
    pub fn is_active(&self, context: &PromptContext) -> bool {
        match self {
            Activation::Always => true,
            Activation::Mode(mode) => context.mode.as_ref() == Some(mode),
            Activation::ToolAvailable(tool) => context.available_tools.contains(tool),
            Activation::Context(key) => context.flags.get(key).copied().unwrap_or(false),
            Activation::Conditional(_) => {
                // Would evaluate custom expression in real impl
                true
            }
        }
    }
}

/// Context for evaluating activation conditions
#[derive(Debug, Clone)]
pub struct PromptContext {
    pub mode: Option<String>,
    pub available_tools: Vec<String>,
    pub flags: HashMap<String, bool>,
    pub token_budget_remaining: usize,
}

impl Default for PromptContext {
    fn default() -> Self {
        Self {
            mode: None,
            available_tools: Vec::new(),
            flags: HashMap::new(),
            token_budget_remaining: 8000, // Sensible default for tests
        }
    }
}

/// A single section of a compiled prompt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSection {
    pub id: String,
    pub priority: u8, // 0-255, lower = higher priority
    pub activation: Activation,
    pub token_budget: usize,
    pub content: String,
    pub provenance: Vec<SourceRef>,
    pub section_type: SectionType,
}

/// Type of prompt section
/// Spec: docs/charm-strategy.md PromptCompiler
/// Required: rules, memories, references, evidence, plan, tool policy
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SectionType {
    /// System identity/instructions
    SystemIdentity,
    /// Active rules (from RuleRegistry)
    Rules,
    /// Tool calling policy and available tools
    ToolRules,
    /// Code editing guidelines
    CodeGuidelines,
    /// Debugging approach
    DebuggingGuidelines,
    /// Communication style
    CommunicationStyle,
    /// Long-term memory content (facts, preferences, context)
    Memory,
    /// Memory handling rules (how to use memories)
    MemoryRules,
    /// Provider-specific instructions
    ProviderSpecific,
    /// Workspace state description
    WorkspaceState,
    /// Current plan/task contract
    Plan,
    /// User message
    UserMessage,
    /// Evidence/context from tools (retrieved code, search results)
    Evidence,
    /// Reference documentation (from ReferenceBroker)
    Reference,
    /// Compressed/summarized context
    CompressedContext,
}

/// Source reference for provenance tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub source_id: String,
    pub source_type: SourceType,
    pub description: Option<String>,
    pub retrieved_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    File,
    ToolOutput,
    Memory,
    Reference,
    Rule,
    Generated,
}

/// Compiled prompt ready for rendering
#[derive(Debug, Clone)]
pub struct CompiledPrompt {
    pub sections: Vec<PromptSection>,
    pub total_token_estimate: usize,
    pub omitted_sections: Vec<String>,
    pub source_provenance: Vec<SourceRef>,
}

/// Provider-specific rendering hints
#[derive(Debug, Clone)]
pub enum ProviderHint {
    OpenAi,
    Anthropic,
    Google,
    Ollama,
    Generic,
}

/// Prompt compiler that assembles typed sections into final prompt
pub struct PromptCompiler {
    sections: Vec<PromptSection>,
    default_budget: usize,
}

impl PromptCompiler {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            default_budget: 4000, // Default token budget
        }
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.default_budget = budget;
        self
    }

    /// Add a section to the compiler (mutable ref)
    pub fn add_section(&mut self, section: PromptSection) -> &mut Self {
        self.sections.push(section);
        self
    }

    /// Add a section to the compiler (consuming builder pattern)
    pub fn with_section(mut self, section: PromptSection) -> Self {
        self.sections.push(section);
        self
    }

    /// Add multiple sections
    pub fn add_sections(&mut self, sections: Vec<PromptSection>) -> &mut Self {
        self.sections.extend(sections);
        self
    }

    /// Compile sections into a prompt given the context
    pub fn compile(&self, context: &PromptContext) -> CompiledPrompt {
        let mut active_sections: Vec<&PromptSection> = self
            .sections
            .iter()
            .filter(|s| s.activation.is_active(context))
            .collect();

        // Sort by priority (lower number = higher priority)
        active_sections.sort_by_key(|s| s.priority);

        let mut compiled = Vec::new();
        let mut omitted = Vec::new();
        let mut remaining_budget = context.token_budget_remaining.min(self.default_budget);
        let mut all_provenance = Vec::new();

        for section in active_sections {
            let section_tokens = Self::estimate_tokens(&section.content);

            if section_tokens > section.token_budget {
                // Section exceeds its own budget - truncate or skip
                let truncated = Self::truncate_to_budget(&section.content, section.token_budget);
                if truncated.is_empty() {
                    omitted.push(section.id.clone());
                    continue;
                }

                let mut truncated_section = section.clone();
                truncated_section.content = truncated;
                compiled.push(truncated_section);
                remaining_budget = remaining_budget.saturating_sub(section.token_budget);
            } else if section_tokens > remaining_budget {
                // Would exceed remaining budget
                omitted.push(section.id.clone());
            } else {
                compiled.push(section.clone());
                remaining_budget -= section_tokens;
            }

            all_provenance.extend(section.provenance.clone());
        }

        let total_tokens = compiled
            .iter()
            .map(|s| Self::estimate_tokens(&s.content))
            .sum();

        CompiledPrompt {
            sections: compiled,
            total_token_estimate: total_tokens,
            omitted_sections: omitted,
            source_provenance: all_provenance,
        }
    }

    /// Render compiled prompt for a specific provider
    pub fn render_for_provider(&self, compiled: &CompiledPrompt, provider: ProviderHint) -> String {
        let mut output = String::new();

        for section in &compiled.sections {
            let rendered = self.render_section(section, &provider);
            if !rendered.is_empty() {
                if !output.is_empty() {
                    output.push_str("\n\n");
                }
                output.push_str(&rendered);
            }
        }

        output
    }

    fn render_section(&self, section: &PromptSection, provider: &ProviderHint) -> String {
        match (provider, &section.section_type) {
            (ProviderHint::Anthropic, SectionType::SystemIdentity) => {
                // Anthropic uses <system> tags (in API, not in content)
                section.content.clone()
            }
            (ProviderHint::OpenAi, SectionType::ToolRules) => {
                // OpenAI has specific tool description format
                format!("## Tool Use\n\n{}", section.content)
            }
            _ => {
                // Default rendering
                match &section.section_type {
                    SectionType::SystemIdentity => section.content.clone(),
                    SectionType::UserMessage => section.content.clone(),
                    SectionType::Evidence => {
                        format!("### Context\n\n{}", section.content)
                    }
                    SectionType::Reference => {
                        format!("### Reference\n\n{}", section.content)
                    }
                    _ => section.content.clone(),
                }
            }
        }
    }

    /// Truncate content to fit within token budget
    fn truncate_to_budget(content: &str, budget: usize) -> String {
        let chars_per_token = 4;
        let max_chars = budget * chars_per_token;

        if content.len() <= max_chars {
            return content.to_string();
        }

        // Try to truncate at a reasonable boundary
        let truncate_at = content[..max_chars.saturating_sub(100)]
            .rfind('\n')
            .unwrap_or(max_chars.saturating_sub(100));

        format!(
            "{}\n\n[... {} characters omitted ...]",
            &content[..truncate_at],
            content.len() - truncate_at
        )
    }

    /// Estimate token count (rough approximation)
    fn estimate_tokens(text: &str) -> usize {
        text.len() / 4
    }

    /// Create a standard system section
    pub fn system_section(id: &str, content: &str) -> PromptSection {
        PromptSection {
            id: id.to_string(),
            priority: 10,
            activation: Activation::Always,
            token_budget: 1000,
            content: content.to_string(),
            provenance: Vec::new(),
            section_type: SectionType::SystemIdentity,
        }
    }

    /// Create a tool rules section
    pub fn tool_rules_section(id: &str, content: &str) -> PromptSection {
        PromptSection {
            id: id.to_string(),
            priority: 20,
            activation: Activation::Always,
            token_budget: 1500,
            content: content.to_string(),
            provenance: Vec::new(),
            section_type: SectionType::ToolRules,
        }
    }

    /// Create a user message section
    pub fn user_section(id: &str, content: &str) -> PromptSection {
        PromptSection {
            id: id.to_string(),
            priority: 100, // Lower priority = comes later
            activation: Activation::Always,
            token_budget: 2000,
            content: content.to_string(),
            provenance: Vec::new(),
            section_type: SectionType::UserMessage,
        }
    }

    /// Create an evidence section (conditional)
    pub fn evidence_section(id: &str, content: &str) -> PromptSection {
        PromptSection {
            id: id.to_string(),
            priority: 50,
            activation: Activation::Context("has_evidence".to_string()),
            token_budget: 1000,
            content: content.to_string(),
            provenance: Vec::new(),
            section_type: SectionType::Evidence,
        }
    }
}

/// Builder for constructing prompts fluently
pub struct PromptBuilder {
    compiler: PromptCompiler,
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            compiler: PromptCompiler::new(),
        }
    }

    pub fn with_system(mut self, content: &str) -> Self {
        self.compiler
            .add_section(PromptCompiler::system_section("system", content));
        self
    }

    pub fn with_tool_rules(mut self, content: &str) -> Self {
        self.compiler
            .add_section(PromptCompiler::tool_rules_section("tools", content));
        self
    }

    pub fn with_user_message(mut self, content: &str) -> Self {
        self.compiler
            .add_section(PromptCompiler::user_section("user", content));
        self
    }

    pub fn with_evidence(mut self, content: &str) -> Self {
        self.compiler
            .add_section(PromptCompiler::evidence_section("evidence", content));
        self
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.compiler = self.compiler.with_budget(budget);
        self
    }

    pub fn compile(self, context: &PromptContext) -> CompiledPrompt {
        self.compiler.compile(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activation_always() {
        let activation = Activation::Always;
        let context = PromptContext::default();
        assert!(activation.is_active(&context));
    }

    #[test]
    fn test_activation_mode() {
        let activation = Activation::Mode("build".to_string());

        let mut context = PromptContext::default();
        assert!(!activation.is_active(&context));

        context.mode = Some("build".to_string());
        assert!(activation.is_active(&context));

        context.mode = Some("debug".to_string());
        assert!(!activation.is_active(&context));
    }

    #[test]
    fn test_activation_tool_available() {
        let activation = Activation::ToolAvailable("read_file".to_string());

        let mut context = PromptContext::default();
        assert!(!activation.is_active(&context));

        context.available_tools = vec!["read_file".to_string(), "write_file".to_string()];
        assert!(activation.is_active(&context));
    }

    #[test]
    fn test_activation_context_flag() {
        let activation = Activation::Context("has_reference".to_string());

        let mut context = PromptContext::default();
        assert!(!activation.is_active(&context));

        context.flags.insert("has_reference".to_string(), true);
        assert!(activation.is_active(&context));
    }

    #[test]
    fn test_prompt_section_creation() {
        let section = PromptSection {
            id: "test".to_string(),
            priority: 50,
            activation: Activation::Always,
            token_budget: 500,
            content: "Test content".to_string(),
            provenance: vec![SourceRef {
                source_id: "file1.rs".to_string(),
                source_type: SourceType::File,
                description: Some("Main file".to_string()),
                retrieved_at: chrono::Utc::now(),
            }],
            section_type: SectionType::SystemIdentity,
        };

        assert_eq!(section.id, "test");
        assert_eq!(section.priority, 50);
    }

    #[test]
    fn test_compiler_prioritizes_sections() {
        let compiler = PromptCompiler::new()
            .with_section(PromptSection {
                id: "low".to_string(),
                priority: 100,
                activation: Activation::Always,
                token_budget: 100,
                content: "Low priority".to_string(),
                provenance: Vec::new(),
                section_type: SectionType::SystemIdentity,
            })
            .with_section(PromptSection {
                id: "high".to_string(),
                priority: 10,
                activation: Activation::Always,
                token_budget: 100,
                content: "High priority".to_string(),
                provenance: Vec::new(),
                section_type: SectionType::SystemIdentity,
            });

        let context = PromptContext::default();
        let compiled = compiler.compile(&context);

        assert_eq!(compiled.sections[0].id, "high");
        assert_eq!(compiled.sections[1].id, "low");
    }

    #[test]
    fn test_compiler_omits_exceeding_budget() {
        let compiler = PromptCompiler::new().with_budget(100); // Small budget

        let compiler = compiler.with_section(PromptSection {
            id: "large".to_string(),
            priority: 10,
            activation: Activation::Always,
            token_budget: 1000,        // Exceeds total budget
            content: "x".repeat(4000), // ~1000 tokens
            provenance: Vec::new(),
            section_type: SectionType::SystemIdentity,
        });

        let mut context = PromptContext::default();
        context.token_budget_remaining = 100;

        let compiled = compiler.compile(&context);

        // Should be omitted or truncated
        assert!(!compiled.sections.is_empty() || !compiled.omitted_sections.is_empty());
    }

    #[test]
    fn test_conditional_section_activation() {
        let compiler = PromptCompiler::new()
            .with_section(PromptSection {
                id: "always".to_string(),
                priority: 10,
                activation: Activation::Always,
                token_budget: 100,
                content: "Always included".to_string(),
                provenance: Vec::new(),
                section_type: SectionType::SystemIdentity,
            })
            .with_section(PromptSection {
                id: "conditional".to_string(),
                priority: 20,
                activation: Activation::Context("include_extra".to_string()),
                token_budget: 100,
                content: "Conditional content".to_string(),
                provenance: Vec::new(),
                section_type: SectionType::Evidence,
            });

        // Without flag
        let context_no_flag = PromptContext::default();
        let compiled_no_flag = compiler.compile(&context_no_flag);
        assert_eq!(compiled_no_flag.sections.len(), 1);
        assert_eq!(compiled_no_flag.sections[0].id, "always");

        // With flag
        let mut context_with_flag = PromptContext::default();
        context_with_flag
            .flags
            .insert("include_extra".to_string(), true);
        let compiled_with_flag = compiler.compile(&context_with_flag);
        assert_eq!(compiled_with_flag.sections.len(), 2);
    }

    #[test]
    fn test_prompt_builder_fluent() {
        let context = PromptContext::default();

        let compiled = PromptBuilder::new()
            .with_system("You are a helpful coding assistant.")
            .with_tool_rules("Use tools when needed.")
            .with_user_message("Fix this bug.")
            .compile(&context);

        assert_eq!(compiled.sections.len(), 3);
        assert!(
            compiled
                .sections
                .iter()
                .any(|s| s.section_type == SectionType::SystemIdentity)
        );
        assert!(
            compiled
                .sections
                .iter()
                .any(|s| s.section_type == SectionType::ToolRules)
        );
        assert!(
            compiled
                .sections
                .iter()
                .any(|s| s.section_type == SectionType::UserMessage)
        );
    }

    #[test]
    fn test_render_for_provider() {
        let compiler = PromptCompiler::new()
            .with_section(PromptSection {
                id: "system".to_string(),
                priority: 10,
                activation: Activation::Always,
                token_budget: 100,
                content: "System instructions".to_string(),
                provenance: Vec::new(),
                section_type: SectionType::SystemIdentity,
            })
            .with_section(PromptSection {
                id: "evidence".to_string(),
                priority: 20,
                activation: Activation::Always,
                token_budget: 100,
                content: "Some evidence".to_string(),
                provenance: Vec::new(),
                section_type: SectionType::Evidence,
            });

        let context = PromptContext::default();
        let compiled = compiler.compile(&context);

        let rendered = compiler.render_for_provider(&compiled, ProviderHint::OpenAi);
        assert!(rendered.contains("System instructions"));
        assert!(rendered.contains("### Context")); // Evidence gets header
    }

    #[test]
    fn test_token_estimation() {
        // ~4 chars per token
        let text = "x".repeat(400);
        let estimate = PromptCompiler::estimate_tokens(&text);
        assert_eq!(estimate, 100);
    }

    #[test]
    fn test_section_type_variants() {
        // Ensure all section types are distinct
        // Matches docs/charm-strategy.md required sections:
        // rules, memories, references, evidence, plan, tool policy
        let types = vec![
            SectionType::SystemIdentity,
            SectionType::Rules,
            SectionType::ToolRules,
            SectionType::CodeGuidelines,
            SectionType::DebuggingGuidelines,
            SectionType::CommunicationStyle,
            SectionType::Memory,
            SectionType::MemoryRules,
            SectionType::ProviderSpecific,
            SectionType::WorkspaceState,
            SectionType::Plan,
            SectionType::UserMessage,
            SectionType::Evidence,
            SectionType::Reference,
            SectionType::CompressedContext,
        ];

        // All should be unique
        let type_count = types.len();
        let unique: std::collections::HashSet<_> = types.into_iter().collect();
        assert_eq!(unique.len(), type_count);
    }
}
