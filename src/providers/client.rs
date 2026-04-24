use crate::providers::anthropic::AnthropicClient;
use crate::providers::google::GoogleClient;
use crate::providers::ollama::OllamaClient;
use crate::providers::openai::OpenAiClient;
use crate::providers::openai_codex::OpenAiCodexClient;
use crate::providers::openrouter::OpenRouterClient;
use crate::providers::sse::StreamChunk;
use crate::providers::types::{
    ChatRequest, ChatResponse, Message, ModelInfo, ToolSchema, Usage, default_tool_schemas,
};

#[derive(Clone)]
pub enum ProviderClient {
    OpenAi(OpenAiClient),
    OpenAiCodex(OpenAiCodexClient),
    Anthropic(AnthropicClient),
    Google(GoogleClient),
    Ollama(OllamaClient),
    OpenRouter(OpenRouterClient),
}

impl ProviderClient {
    pub async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        match self {
            ProviderClient::OpenAi(client) => client.chat(request).await,
            ProviderClient::OpenAiCodex(client) => client.chat(request).await,
            ProviderClient::Anthropic(client) => client.chat(request).await,
            ProviderClient::Google(client) => client.chat(request).await,
            ProviderClient::Ollama(client) => client.chat(request).await,
            ProviderClient::OpenRouter(client) => client.chat(request).await,
        }
    }

    pub async fn chat_raw(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        match self {
            ProviderClient::OpenAi(client) => client.chat_raw(request).await,
            ProviderClient::OpenAiCodex(client) => client.chat_raw(request).await,
            ProviderClient::Anthropic(client) => client.chat_raw(request).await,
            ProviderClient::Google(client) => client.chat_raw(request).await,
            ProviderClient::Ollama(client) => client.chat_raw(request).await,
            ProviderClient::OpenRouter(client) => client.chat_raw(request).await,
        }
    }

    pub async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
        match self {
            ProviderClient::OpenAi(client) => client.chat_stream(request).await,
            ProviderClient::OpenAiCodex(_) => {
                Err(anyhow::anyhow!("Codex provider does not support streaming"))
            }
            ProviderClient::Anthropic(client) => client.chat_stream(request).await,
            ProviderClient::Google(client) => client.chat_stream(request).await,
            ProviderClient::Ollama(client) => client.chat_stream(request).await,
            ProviderClient::OpenRouter(client) => client.chat_stream(request).await,
        }
    }

    pub fn build_tool_schemas(&self) -> Vec<ToolSchema> {
        default_tool_schemas()
    }

    pub async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        match self {
            ProviderClient::OpenAi(client) => client.list_models().await,
            ProviderClient::OpenAiCodex(_) => Ok(Vec::new()),
            ProviderClient::Anthropic(client) => client.list_models().await,
            ProviderClient::Google(client) => client.list_models().await,
            ProviderClient::Ollama(client) => client.list_models().await,
            ProviderClient::OpenRouter(client) => client.list_models().await,
        }
    }
}
