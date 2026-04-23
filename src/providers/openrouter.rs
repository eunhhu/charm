use reqwest::header::{HeaderMap, HeaderValue};

use super::openai_compatible::OpenAiCompatibleClient;
use super::sse::StreamChunk;
use super::types::{ChatRequest, ChatResponse, Message, ModelInfo, Usage};

#[derive(Clone)]
pub struct OpenRouterClient(OpenAiCompatibleClient);

impl OpenRouterClient {
    pub fn new(api_key: String) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            "HTTP-Referer",
            HeaderValue::from_static("https://github.com/charm"),
        );
        headers.insert("X-Title", HeaderValue::from_static("Charm Agent Harness"));

        Self(OpenAiCompatibleClient::new(
            "OpenRouter",
            api_key,
            "https://openrouter.ai/api/v1",
            headers,
        ))
    }

    pub async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        self.0.chat(request).await
    }

    pub async fn chat_raw(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.0.chat_raw(request).await
    }

    pub async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
        self.0.chat_stream(request).await
    }

    pub async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        self.0.list_models().await
    }
}
