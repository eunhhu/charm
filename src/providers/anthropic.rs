use reqwest::header::HeaderMap;

use super::openai_compatible::OpenAiCompatibleClient;
use super::types::{ChatRequest, ChatResponse, Message, Usage};

#[derive(Clone)]
pub struct AnthropicClient(OpenAiCompatibleClient);

impl AnthropicClient {
    pub fn new(api_key: String) -> Self {
        Self(OpenAiCompatibleClient::new(
            "Anthropic",
            api_key,
            "https://api.anthropic.com/v1",
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
