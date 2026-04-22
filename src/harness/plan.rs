use crate::core::ToolResult;
use serde_json::Value;
use std::path::Path;

pub struct PlanArtifact {
    pub objective: String,
    pub current_phase: String,
    pub completed_steps: Vec<String>,
    pub blocked_steps: Vec<String>,
    pub verification_checklist: Vec<String>,
    pub rollback_refs: Vec<String>,
}

pub struct PlanManager {
    plan_path: std::path::PathBuf,
}

impl PlanManager {
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            plan_path: workspace_root.join(".charm").join("plan.md"),
        }
    }

    pub fn load(&self) -> PlanArtifact {
        if !self.plan_path.exists() {
            return default_artifact();
        }
        let raw = std::fs::read_to_string(&self.plan_path).unwrap_or_default();
        parse_plan(&raw)
    }

    pub fn update(&self, args: Value) -> anyhow::Result<ToolResult> {
        let mut content = if self.plan_path.exists() {
            std::fs::read_to_string(&self.plan_path)?
        } else {
            default_plan()
        };

        if let Some(obj) = args["objective"].as_str() {
            content = replace_section(&content, "Objective", obj);
        }
        if let Some(phase) = args["current_phase"].as_str() {
            content = replace_section(&content, "Current Phase", phase);
        }
        if let Some(steps) = args["completed_steps"].as_array() {
            let list: Vec<String> = steps
                .iter()
                .filter_map(|v| v.as_str().map(|s| format!("- {}", s)))
                .collect();
            content = replace_section(&content, "Completed Steps", &list.join("\n"));
        }
        if let Some(steps) = args["blocked_steps"].as_array() {
            let list: Vec<String> = steps
                .iter()
                .filter_map(|v| v.as_str().map(|s| format!("- {}", s)))
                .collect();
            content = replace_section(&content, "Blocked Steps", &list.join("\n"));
        }
        if let Some(notes) = args["notes"].as_str() {
            content = replace_section(&content, "Notes", notes);
        }

        std::fs::create_dir_all(self.plan_path.parent().unwrap())?;
        std::fs::write(&self.plan_path, content)?;

        Ok(ToolResult {
            success: true,
            output: "Plan updated".to_string(),
            error: None,
            metadata: None,
        })
    }
}

fn default_plan() -> String {
    "# Charm Plan\n\n## Objective\n(none)\n\n## Current Phase\nidle\n\n## Completed Steps\n\n## Blocked Steps\n\n## Verification Checklist\n\n## Rollback References\n\n".to_string()
}

fn default_artifact() -> PlanArtifact {
    PlanArtifact {
        objective: "(none)".to_string(),
        current_phase: "idle".to_string(),
        completed_steps: Vec::new(),
        blocked_steps: Vec::new(),
        verification_checklist: Vec::new(),
        rollback_refs: Vec::new(),
    }
}

fn parse_plan(raw: &str) -> PlanArtifact {
    let mut artifact = default_artifact();
    let mut current_section = "";
    let mut buffer = Vec::new();

    for line in raw.lines() {
        if line.starts_with("## ") {
            if !current_section.is_empty() {
                apply_section(&mut artifact, current_section, &buffer);
            }
            current_section = line.strip_prefix("## ").unwrap_or("").trim();
            buffer.clear();
        } else {
            buffer.push(line);
        }
    }
    if !current_section.is_empty() {
        apply_section(&mut artifact, current_section, &buffer);
    }
    artifact
}

fn apply_section(artifact: &mut PlanArtifact, section: &str, lines: &[&str]) {
    let text = lines.join("\n").trim().to_string();
    match section.to_lowercase().as_str() {
        "objective" => artifact.objective = text,
        "current phase" => artifact.current_phase = text,
        "completed steps" => artifact.completed_steps = parse_list(lines),
        "blocked steps" => artifact.blocked_steps = parse_list(lines),
        "verification checklist" => artifact.verification_checklist = parse_list(lines),
        "rollback references" => artifact.rollback_refs = parse_list(lines),
        _ => {}
    }
}

fn parse_list(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| l.trim().strip_prefix("-").map(|s| s.trim().to_string()))
        .collect()
}

fn replace_section(content: &str, header: &str, new_body: &str) -> String {
    let pattern = format!("## {}\\n", regex::escape(header));
    let re = regex::Regex::new(&pattern).unwrap();

    if let Some(mat) = re.find(content) {
        let start = mat.end();
        let end = content[start..]
            .find("\n## ")
            .map(|i| start + i)
            .unwrap_or(content.len());
        format!("{}{}\n{}", &content[..start], new_body, &content[end..])
    } else {
        format!("{}\n## {}\n{}\n", content, header, new_body)
    }
}
