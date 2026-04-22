use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::openai_compatible::OpenAiCompatibleClient;
use super::types::{ChatRequest, ChatResponse, Message, Usage};

#[derive(Clone)]
pub struct OpenAiClient(OpenAiCompatibleClient);

impl OpenAiClient {
    pub fn new(api_key: String) -> Self {
        let mut headers = HeaderMap::new();
        if let Ok(org) = std::env::var("OPENAI_ORGANIZATION")
            && !org.trim().is_empty()
            && let Ok(value) = HeaderValue::from_str(&org)
        {
            headers.insert(HeaderName::from_static("openai-organization"), value);
        }
        if let Ok(project) = std::env::var("OPENAI_PROJECT")
            && !project.trim().is_empty()
            && let Ok(value) = HeaderValue::from_str(&project)
        {
            headers.insert(HeaderName::from_static("openai-project"), value);
        }

        Self(OpenAiCompatibleClient::new(
            "OpenAI",
            api_key,
            "https://api.openai.com/v1",
            headers,
        ))
    }

    pub async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        self.0.chat(request).await
    }

    pub async fn chat_raw(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.0.chat_raw(request).await
    }
}
