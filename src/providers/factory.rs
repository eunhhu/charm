use crate::providers::anthropic::AnthropicClient;
use crate::providers::client::ProviderClient;
use crate::providers::google::GoogleClient;
use crate::providers::ollama::OllamaClient;
use crate::providers::openai::OpenAiClient;
use crate::providers::openai_codex::OpenAiCodexClient;
use crate::providers::openrouter::OpenRouterClient;
use crate::providers::types::{ToolSchema, default_tool_schemas};
use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    OpenAiCodex,
    Anthropic,
    Google,
    Ollama,
    OpenRouter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAuth {
    pub token: String,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider: Provider,
    pub request_model: String,
    pub display_model: String,
}

pub struct ResolvedProviderSession {
    pub provider: Provider,
    pub request_model: String,
    pub display_model: String,
    pub client: ProviderClient,
}

#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    tokens: Option<CodexAuthTokens>,
}

#[derive(Debug, Deserialize)]
struct CodexAuthTokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

impl Provider {
    pub fn id(&self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::OpenAiCodex => "openai-codex",
            Provider::Anthropic => "anthropic",
            Provider::Google => "google",
            Provider::Ollama => "ollama",
            Provider::OpenRouter => "openrouter",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Provider::OpenAi),
            "openai-codex" => Some(Provider::OpenAiCodex),
            "anthropic" => Some(Provider::Anthropic),
            "google" => Some(Provider::Google),
            "ollama" => Some(Provider::Ollama),
            "openrouter" => Some(Provider::OpenRouter),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        resolve_model_selection(None, "moonshotai/kimi-k2.6")
            .map(|selection| selection.provider)
            .unwrap_or(Provider::OpenRouter)
    }

    pub fn create_client(&self, auth: ProviderAuth) -> ProviderClient {
        match self {
            Provider::OpenAi => ProviderClient::OpenAi(OpenAiClient::new(auth.token)),
            Provider::OpenAiCodex => {
                ProviderClient::OpenAiCodex(OpenAiCodexClient::new(auth.token, auth.account_id))
            }
            Provider::Anthropic => ProviderClient::Anthropic(AnthropicClient::new(auth.token)),
            Provider::Google => ProviderClient::Google(GoogleClient::new(auth.token)),
            Provider::Ollama => ProviderClient::Ollama(OllamaClient::new(auth.token)),
            Provider::OpenRouter => ProviderClient::OpenRouter(OpenRouterClient::new(auth.token)),
        }
    }

    pub fn build_tool_schemas(&self) -> Vec<ToolSchema> {
        default_tool_schemas()
    }

    fn infer_from_model(raw_model: &str) -> Self {
        let lower = raw_model.to_ascii_lowercase();
        if lower.starts_with("claude") {
            return Provider::Anthropic;
        }
        if lower.starts_with("gemini") {
            return Provider::Google;
        }
        if lower.starts_with("gpt")
            || lower.starts_with("o1")
            || lower.starts_with("o3")
            || lower.starts_with("o4")
            || lower.starts_with("codex")
        {
            return Provider::OpenAi;
        }
        if raw_model.contains(':') {
            return Provider::Ollama;
        }
        Provider::OpenRouter
    }
}

pub fn resolve_model_selection(
    preferred: Option<Provider>,
    raw_model: &str,
) -> anyhow::Result<ModelSelection> {
    let trimmed = raw_model.trim();
    anyhow::ensure!(!trimmed.is_empty(), "model must not be empty");

    if let Some((provider, request_model)) = split_provider_prefix(trimmed) {
        if let Some(explicit) = preferred
            && explicit != provider
        {
            anyhow::bail!(
                "model `{}` implies provider `{}`, but `--provider {}` was requested",
                trimmed,
                provider.id(),
                explicit.id()
            );
        }
        return Ok(ModelSelection {
            provider,
            request_model: request_model.clone(),
            display_model: format!("{}/{}", provider.id(), request_model),
        });
    }

    let provider = preferred.unwrap_or_else(|| Provider::infer_from_model(trimmed));
    Ok(ModelSelection {
        provider,
        request_model: trimmed.to_string(),
        display_model: format!("{}/{}", provider.id(), trimmed),
    })
}

pub fn resolve_provider_session(
    preferred: Option<Provider>,
    raw_model: &str,
) -> anyhow::Result<ResolvedProviderSession> {
    let selection = resolve_model_selection(preferred, raw_model)?;
    let auth = resolve_provider_auth(&selection.provider)?;
    let client = selection.provider.create_client(auth);

    Ok(ResolvedProviderSession {
        provider: selection.provider,
        request_model: selection.request_model,
        display_model: selection.display_model,
        client,
    })
}

pub fn resolve_provider_auth(provider: &Provider) -> anyhow::Result<ProviderAuth> {
    let env = process_env();
    let codex_home = codex_home_path();
    resolve_provider_auth_inner(provider, &env, codex_home.as_deref())
}

pub fn resolve_api_key_with_provider(
    preferred: Option<Provider>,
) -> anyhow::Result<(String, Provider)> {
    let provider = preferred.unwrap_or_else(Provider::from_env);
    let auth = resolve_provider_auth(&provider)?;
    Ok((auth.token, provider))
}

pub fn resolve_api_key() -> anyhow::Result<(String, Provider)> {
    resolve_api_key_with_provider(None)
}

fn split_provider_prefix(raw_model: &str) -> Option<(Provider, String)> {
    let (provider_id, request_model) = raw_model.split_once('/')?;
    let provider = Provider::from_id(provider_id)?;
    if request_model.trim().is_empty() {
        return None;
    }
    Some((provider, request_model.to_string()))
}

fn resolve_provider_auth_inner(
    provider: &Provider,
    env: &HashMap<String, String>,
    codex_home: Option<&Path>,
) -> anyhow::Result<ProviderAuth> {
    match provider {
        Provider::OpenAi => {
            if let Some(token) = first_env(env, &["OPENAI_API_KEY"]) {
                return Ok(ProviderAuth {
                    token,
                    account_id: None,
                });
            }

            if let Some(file) = load_codex_auth_file(codex_home)?
                && let Some(token) = file.openai_api_key
                && !token.trim().is_empty()
            {
                return Ok(ProviderAuth {
                    token,
                    account_id: None,
                });
            }

            anyhow::bail!("OPENAI_API_KEY must be set")
        }
        Provider::OpenAiCodex => {
            let file = load_codex_auth_file(codex_home)?
                .context("OpenAI Codex auth not found. Sign into Codex first.")?;
            let tokens = file
                .tokens
                .context("OpenAI Codex auth file is missing token data")?;
            let access_token = tokens
                .access_token
                .filter(|token| !token.trim().is_empty())
                .context("OpenAI Codex auth file is missing an access token")?;

            Ok(ProviderAuth {
                token: access_token,
                account_id: tokens.account_id.filter(|id| !id.trim().is_empty()),
            })
        }
        Provider::Anthropic => {
            let token =
                first_env(env, &["ANTHROPIC_API_KEY"]).context("ANTHROPIC_API_KEY must be set")?;
            Ok(ProviderAuth {
                token,
                account_id: None,
            })
        }
        Provider::Google => {
            let token = first_env(env, &["GEMINI_API_KEY", "GOOGLE_API_KEY"])
                .context("GEMINI_API_KEY or GOOGLE_API_KEY must be set")?;
            Ok(ProviderAuth {
                token,
                account_id: None,
            })
        }
        Provider::Ollama => Ok(ProviderAuth {
            token: first_env(env, &["OLLAMA_API_KEY"]).unwrap_or_else(|| "ollama".to_string()),
            account_id: None,
        }),
        Provider::OpenRouter => {
            let token = first_env(env, &["OPENROUTER_API_KEY"])
                .context("OPENROUTER_API_KEY must be set")?;
            Ok(ProviderAuth {
                token,
                account_id: None,
            })
        }
    }
}

fn first_env(env: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        env.get(*key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
    })
}

fn process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn codex_home_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODEX_HOME")
        && !path.trim().is_empty()
    {
        return Some(PathBuf::from(path));
    }

    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".codex"))
}

fn load_codex_auth_file(codex_home: Option<&Path>) -> anyhow::Result<Option<CodexAuthFile>> {
    let Some(codex_home) = codex_home else {
        return Ok(None);
    };

    let path = codex_home.join("auth.json");
    if !path.exists() {
        return Ok(None);
    }

    let body = std::fs::read_to_string(&path)?;
    let parsed = serde_json::from_str::<CodexAuthFile>(&body)?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn auto_infers_openrouter_for_legacy_router_model() {
        let resolved = resolve_model_selection(None, "moonshotai/kimi-k2.6").expect("resolve");
        assert_eq!(resolved.provider, Provider::OpenRouter);
        assert_eq!(resolved.request_model, "moonshotai/kimi-k2.6");
        assert_eq!(resolved.display_model, "openrouter/moonshotai/kimi-k2.6");
    }

    #[test]
    fn explicit_provider_wraps_unqualified_model() {
        let resolved =
            resolve_model_selection(Some(Provider::OpenRouter), "gpt-5").expect("resolve");
        assert_eq!(resolved.provider, Provider::OpenRouter);
        assert_eq!(resolved.request_model, "gpt-5");
        assert_eq!(resolved.display_model, "openrouter/gpt-5");
    }

    #[test]
    fn canonical_model_prefix_overrides_auto_provider() {
        let resolved =
            resolve_model_selection(None, "anthropic/claude-sonnet-4-5").expect("resolve");
        assert_eq!(resolved.provider, Provider::Anthropic);
        assert_eq!(resolved.request_model, "claude-sonnet-4-5");
        assert_eq!(resolved.display_model, "anthropic/claude-sonnet-4-5");
    }

    #[test]
    fn canonical_openai_prefix_normalizes_request_and_display_model() {
        let resolved = resolve_model_selection(None, "openai/gpt-5").expect("resolve");
        assert_eq!(resolved.provider, Provider::OpenAi);
        assert_eq!(resolved.request_model, "gpt-5");
        assert_eq!(resolved.display_model, "openai/gpt-5");
    }

    #[test]
    fn mismatched_provider_and_canonical_model_fails() {
        let error =
            resolve_model_selection(Some(Provider::Google), "openai/gpt-5").expect_err("must fail");
        assert!(error.to_string().contains("implies provider"));
    }

    #[test]
    fn auto_infers_ollama_for_tagged_models() {
        let resolved = resolve_model_selection(None, "qwen3-coder:30b").expect("resolve");
        assert_eq!(resolved.provider, Provider::Ollama);
        assert_eq!(resolved.display_model, "ollama/qwen3-coder:30b");
    }

    #[test]
    fn loads_openai_codex_auth_from_auth_file() {
        let temp = tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": null,
                "tokens": {
                    "access_token": "chatgpt-access-token",
                    "account_id": "workspace-123",
                }
            })
            .to_string(),
        )
        .expect("write auth");

        let env = HashMap::new();
        let auth = resolve_provider_auth_inner(&Provider::OpenAiCodex, &env, Some(temp.path()))
            .expect("auth");
        assert_eq!(
            auth,
            ProviderAuth {
                token: "chatgpt-access-token".to_string(),
                account_id: Some("workspace-123".to_string()),
            }
        );
    }

    #[test]
    fn openai_api_key_falls_back_to_codex_auth_file_key() {
        let temp = tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": "sk-from-codex-home"
            })
            .to_string(),
        )
        .expect("write auth");

        let env = HashMap::new();
        let auth =
            resolve_provider_auth_inner(&Provider::OpenAi, &env, Some(temp.path())).expect("auth");
        assert_eq!(
            auth,
            ProviderAuth {
                token: "sk-from-codex-home".to_string(),
                account_id: None,
            }
        );
    }

    #[test]
    fn google_accepts_both_common_env_vars() {
        let env = HashMap::from([("GOOGLE_API_KEY".to_string(), "google-key".to_string())]);
        let auth = resolve_provider_auth_inner(&Provider::Google, &env, None).expect("auth");
        assert_eq!(auth.token, "google-key");
    }
}
