use std::collections::HashMap;

pub struct ProviderPrompts {
    prompts: HashMap<String, String>,
}

impl ProviderPrompts {
    pub fn new() -> Self {
        let mut prompts = HashMap::new();

        prompts.insert("anthropic".to_string(), anthropic_prompt());
        prompts.insert("openai".to_string(), openai_prompt());
        prompts.insert("moonshot".to_string(), moonshot_prompt());
        prompts.insert("google".to_string(), google_prompt());
        prompts.insert("xai".to_string(), xai_prompt());
        prompts.insert("default".to_string(), default_prompt());

        Self { prompts }
    }

    pub fn get(&self, provider: &str) -> &str {
        self.prompts
            .get(provider)
            .or_else(|| self.prompts.get("default"))
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    pub fn resolve_provider(model_id: &str) -> &'static str {
        let lower = model_id.to_lowercase();
        if lower.contains("claude") || lower.contains("anthropic") {
            "anthropic"
        } else if lower.contains("gpt")
            || lower.contains("openai")
            || matches_openai_o_series(&lower)
        {
            "openai"
        } else if lower.contains("kimi") || lower.contains("moonshot") {
            "moonshot"
        } else if lower.contains("gemini") || lower.contains("google") {
            "google"
        } else if lower.contains("grok") || lower.contains("xai") {
            "xai"
        } else {
            "default"
        }
    }
}

impl Default for ProviderPrompts {
    fn default() -> Self {
        Self::new()
    }
}

fn matches_openai_o_series(model_id: &str) -> bool {
    matches!(
        model_id,
        "o1" | "o1-mini" | "o1-preview" | "o3" | "o3-mini" | "o4-mini"
    ) || model_id.starts_with("o1-")
        || model_id.starts_with("o3-")
        || model_id.starts_with("o4-")
}

fn default_prompt() -> String {
    r#"- Use tools efficiently. Avoid redundant calls.
- Be concise in responses."#
        .to_string()
}

fn anthropic_prompt() -> String {
    r#"- You are Claude, an AI assistant by Anthropic.
- You excel at careful reasoning and nuanced analysis.
- When editing code, think step-by-step before making changes.
- You have strong capabilities for long-context understanding (200K tokens).
- Use your reasoning ability to trace through code execution paths.
- For complex tasks, break down into smaller steps and verify each one."#
        .to_string()
}

fn openai_prompt() -> String {
    r#"- You are GPT, an AI assistant by OpenAI.
- You are optimized for tool use and code generation.
- When provided with tools, use them proactively and efficiently.
- You support parallel tool calls - use this for independent operations.
- Keep responses focused and actionable."#
        .to_string()
}

fn moonshot_prompt() -> String {
    r#"- You are Kimi, an AI assistant by Moonshot AI.
- You excel at long-context processing (up to 2M characters).
- You have strong capabilities in code understanding and generation.
- When editing files, ensure exact string matching for replacements.
- You can handle large codebases efficiently."#
        .to_string()
}

fn google_prompt() -> String {
    r#"- You are Gemini, an AI assistant by Google.
- You have strong multimodal and long-context capabilities.
- You excel at understanding complex code structures.
- When making changes, verify compatibility with existing patterns.
- You support up to 1M+ token context windows."#
        .to_string()
}

fn xai_prompt() -> String {
    r#"- You are Grok, an AI assistant by xAI.
- You have strong reasoning and coding capabilities.
- You excel at understanding and modifying large codebases.
- When debugging, trace through execution paths carefully.
- You support long context windows for comprehensive analysis."#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_prompt_golden_snapshots_cover_known_providers() {
        let prompts = ProviderPrompts::new();
        let cases = [
            (
                "anthropic",
                "- You are Claude, an AI assistant by Anthropic.\n- You excel at careful reasoning and nuanced analysis.\n- When editing code, think step-by-step before making changes.\n- You have strong capabilities for long-context understanding (200K tokens).\n- Use your reasoning ability to trace through code execution paths.\n- For complex tasks, break down into smaller steps and verify each one.",
            ),
            (
                "openai",
                "- You are GPT, an AI assistant by OpenAI.\n- You are optimized for tool use and code generation.\n- When provided with tools, use them proactively and efficiently.\n- You support parallel tool calls - use this for independent operations.\n- Keep responses focused and actionable.",
            ),
            (
                "moonshot",
                "- You are Kimi, an AI assistant by Moonshot AI.\n- You excel at long-context processing (up to 2M characters).\n- You have strong capabilities in code understanding and generation.\n- When editing files, ensure exact string matching for replacements.\n- You can handle large codebases efficiently.",
            ),
            (
                "google",
                "- You are Gemini, an AI assistant by Google.\n- You have strong multimodal and long-context capabilities.\n- You excel at understanding complex code structures.\n- When making changes, verify compatibility with existing patterns.\n- You support up to 1M+ token context windows.",
            ),
            (
                "xai",
                "- You are Grok, an AI assistant by xAI.\n- You have strong reasoning and coding capabilities.\n- You excel at understanding and modifying large codebases.\n- When debugging, trace through execution paths carefully.\n- You support long context windows for comprehensive analysis.",
            ),
            (
                "unknown-provider",
                "- Use tools efficiently. Avoid redundant calls.\n- Be concise in responses.",
            ),
        ];

        for (provider, expected) in cases {
            assert_eq!(prompts.get(provider), expected, "provider={provider}");
        }
    }

    #[test]
    fn provider_resolver_covers_current_model_family_aliases() {
        let cases = [
            ("claude-opus-4.5", "anthropic"),
            ("gpt-5.1-codex", "openai"),
            ("o3", "openai"),
            ("o4-mini", "openai"),
            ("kimi-k2", "moonshot"),
            ("gemini-2.5-pro", "google"),
            ("grok-4", "xai"),
            ("local-dev-model", "default"),
        ];

        for (model, expected) in cases {
            assert_eq!(
                ProviderPrompts::resolve_provider(model),
                expected,
                "model={model}"
            );
        }
    }
}
