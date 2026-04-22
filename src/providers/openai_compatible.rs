use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};

use super::types::{ChatRequest, ChatResponse, Message, Usage};

#[derive(Clone)]
pub struct OpenAiCompatibleClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    extra_headers: HeaderMap,
    provider_name: &'static str,
}

impl OpenAiCompatibleClient {
    pub fn new(
        provider_name: &'static str,
        api_key: String,
        base_url: impl Into<String>,
        extra_headers: HeaderMap,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: base_url.into(),
            extra_headers,
            provider_name,
        }
    }

    pub async fn chat(&self, request: ChatRequest) -> anyhow::Result<(Message, Option<Usage>)> {
        let parsed = self.chat_raw(request).await?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No choices in response"))?;

        Ok((choice.message, parsed.usage))
    }

    pub async fn chat_raw(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if !self.api_key.trim().is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", self.api_key))?,
            );
        }
        headers.extend(self.extra_headers.clone());

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "{} error {}: {}",
                self.provider_name,
                status,
                body
            ));
        }

        Ok(serde_json::from_str(&body)?)
    }
}
