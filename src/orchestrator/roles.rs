use crate::orchestrator::types::{
    Subtask, SubtaskResult, TaskDecomposition, Verdict, Verification,
};
use crate::providers::client::ProviderClient;
use crate::providers::types::{ChatRequest, Message};

pub struct PlannerRole {
    client: ProviderClient,
    model: String,
}

impl PlannerRole {
    pub fn new(client: ProviderClient, model: String) -> Self {
        Self { client, model }
    }

    pub async fn decompose(&self, task: &str, context: &str) -> anyhow::Result<TaskDecomposition> {
        let system = r#"You are a Task Planner. Decompose the given task into small, independent subtasks.

Rules:
- Each subtask should be atomic (one logical operation).
- Order subtasks by dependency (earlier subtasks first).
- If a subtask requires reading code first, make that explicit.
- Do NOT write implementation code — only describe what needs to be done.

Respond in JSON format:
{
  "subtasks": [
    {"id": "1", "description": "...", "dependencies": []},
    {"id": "2", "description": "...", "dependencies": ["1"]}
  ]
}"#;

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: Some(system.to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_details: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Some(format!("Task: {}\n\nContext:\n{}", task, context)),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_details: None,
                },
            ],
            tools: None,
            tool_choice: None,
            temperature: Some(0.2),
            max_tokens: Some(4000),
            reasoning: None,
            parallel_tool_calls: None,
        };

        let (response, _) = self.client.chat(request).await?;
        let content = response.content.unwrap_or_default();

        let decomp: TaskDecomposition = serde_json::from_str(&Self::extract_json(&content))?;
        Ok(decomp)
    }

    fn extract_json(text: &str) -> String {
        if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                return text[start..=end].to_string();
            }
        }
        text.to_string()
    }
}

pub struct VerifierRole {
    client: ProviderClient,
    model: String,
}

impl VerifierRole {
    pub fn new(client: ProviderClient, model: String) -> Self {
        Self { client, model }
    }

    pub async fn verify(
        &self,
        subtask: &Subtask,
        result: &SubtaskResult,
    ) -> anyhow::Result<Verification> {
        let system = r#"You are a Code Verifier. Review the completed subtask and judge its correctness.

Rules:
- Check if the subtask description was fully satisfied.
- Check if the changed files look correct.
- Look for obvious bugs, missing imports, or syntax errors.
- Respond in JSON format:
{"verdict": "pass|retry|failed", "feedback": "..."}"#;

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: Some(system.to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_details: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Some(format!(
                        "Subtask: {}\nResult: {}\nFiles changed: {:?}",
                        subtask.description, result.output, result.files_changed
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning: None,
                    reasoning_details: None,
                },
            ],
            tools: None,
            tool_choice: None,
            temperature: Some(0.1),
            max_tokens: Some(2000),
            reasoning: None,
            parallel_tool_calls: None,
        };

        let (response, _) = self.client.chat(request).await?;
        let content = response.content.unwrap_or_default();

        let json_str = Self::extract_json(&content);
        let verdict_str = Self::extract_field(&json_str, "verdict").unwrap_or("failed".to_string());
        let feedback = Self::extract_field(&json_str, "feedback").unwrap_or_default();

        let verdict = match verdict_str.as_str() {
            "pass" => Verdict::Pass,
            "retry" => Verdict::Retry,
            _ => Verdict::Failed,
        };

        Ok(Verification {
            subtask_id: subtask.id.clone(),
            verdict,
            feedback,
        })
    }

    fn extract_json(text: &str) -> String {
        if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                return text[start..=end].to_string();
            }
        }
        text.to_string()
    }

    fn extract_field(json: &str, field: &str) -> Option<String> {
        let pattern = format!("\"{}\"", field);
        if let Some(start) = json.find(&pattern) {
            let after = &json[start + pattern.len()..];
            if let Some(colon) = after.find(':') {
                let rest = &after[colon + 1..].trim_start();
                if rest.starts_with('"') {
                    if let Some(end) = rest[1..].find('"') {
                        return Some(rest[1..end + 1].to_string());
                    }
                }
            }
        }
        None
    }
}
