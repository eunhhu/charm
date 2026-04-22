use reqwest::header::HeaderMap;

use super::openai_compatible::OpenAiCompatibleClient;
use super::types::{ChatRequest, ChatResponse, Message, Usage};

#[derive(Clone)]
pub struct OllamaClient(OpenAiCompatibleClient);

impl OllamaClient {
    pub fn new(api_key: String) -> Self {
        let host = std::env::var("OLLAMA_HOST")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:11434".to_string());
        let host = host.trim_end_matches('/');
        let base_url = if host.ends_with("/v1") {
            host.to_string()
        } else {
            format!("{host}/v1")
        };

        Self(OpenAiCompatibleClient::new(
            "Ollama",
            api_key,
            base_url,
            HeaderMap::new(),
        ))
    }

    pub async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        self.0.chat(request).await
    }

    pub async fn chat_raw(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.0.chat_raw(request).await
    }
}
