use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;

use super::sse::{StreamChunk, accumulate_stream_to_response, parse_sse_line};
use super::types::{ChatRequest, ChatResponse, Message, ModelInfo, Usage};

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

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

    pub async fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let mut headers = HeaderMap::new();
        if !self.api_key.trim().is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", self.api_key))?,
            );
        }
        headers.extend(self.extra_headers.clone());

        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let response = self.client.get(&url).headers(headers).send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            return Err(anyhow::anyhow!(
                "{} models error {}: {}",
                self.provider_name,
                status,
                body
            ));
        }

        let body = response.text().await?;
        let parsed: ModelsResponse = serde_json::from_str(&body)?;

        Ok(parsed
            .data
            .into_iter()
            .map(|entry| {
                let display = entry
                    .display_name
                    .or(entry.name)
                    .unwrap_or_else(|| entry.id.clone());
                ModelInfo {
                    id: entry.id,
                    display_name: display,
                    provider: self.provider_name.to_string(),
                }
            })
            .collect())
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

    pub async fn chat_stream(
        &self,
        mut request: ChatRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamChunk>>> {
        request.stream = Some(true);

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
        if !status.is_success() {
            let body = response.text().await?;
            return Err(anyhow::anyhow!(
                "{} stream error {}: {}",
                self.provider_name,
                status,
                body
            ));
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            use futures_util::StreamExt;
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("Stream error: {}", e))).await;
                        break;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    if let Some(result) = parse_sse_line(&line) {
                        match result {
                            Ok(stream_chunk) => {
                                if tx.send(Ok(stream_chunk)).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }
}
