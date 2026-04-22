use reqwest::header::HeaderMap;

use super::openai_compatible::OpenAiCompatibleClient;
use super::types::{ChatRequest, ChatResponse, Message, Usage};

#[derive(Clone)]
pub struct GoogleClient(OpenAiCompatibleClient);

impl GoogleClient {
    pub fn new(api_key: String) -> Self {
        Self(OpenAiCompatibleClient::new(
            "Google AI Studio",
            api_key,
            "https://generativelanguage.googleapis.com/v1beta/openai",
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
