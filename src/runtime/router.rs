use crate::core::{RiskClass, ToolCall};

use super::types::{AutonomyLevel, RouterIntent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterDecision {
    pub intent: RouterIntent,
    pub source: String,
}

pub fn decide_intent(
    message: &str,
    override_intent: Option<RouterIntent>,
    has_plan: bool,
    has_diagnostics: bool,
) -> RouterDecision {
    if let Some(intent) = override_intent {
        return RouterDecision {
            intent,
            source: "slash_override".to_string(),
        };
    }

    let lower = message.to_lowercase();
    let intent = if lower.contains("verify")
        || lower.contains("test")
        || lower.contains("check")
        || has_diagnostics
    {
        RouterIntent::Verify
    } else if lower.contains("plan")
        || lower.contains("approach")
        || lower.contains("design")
        || (!has_plan && lower.contains("before"))
    {
        RouterIntent::Plan
    } else if lower.contains("fix")
        || lower.contains("implement")
        || lower.contains("add")
        || lower.contains("change")
        || lower.contains("refactor")
        || has_plan
    {
        RouterIntent::Implement
    } else {
        RouterIntent::Explore
    };

    RouterDecision {
        intent,
        source: "auto".to_string(),
    }
}

pub fn parse_slash_override(message: &str) -> (Option<RouterIntent>, &str) {
    let trimmed = message.trim();
    if !trimmed.starts_with('/') {
        return (None, message);
    }

    let mut parts = trimmed.splitn(2, ' ');
    let command = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default();
    let intent = match command {
        "/explore" => Some(RouterIntent::Explore),
        "/plan" => Some(RouterIntent::Plan),
        "/build" => Some(RouterIntent::Implement),
        "/verify" => Some(RouterIntent::Verify),
        _ => None,
    };

    (intent, rest)
}

pub fn requires_approval(autonomy: AutonomyLevel, risk: RiskClass) -> bool {
    match autonomy {
        AutonomyLevel::Conservative => !matches!(risk, RiskClass::SafeRead),
        AutonomyLevel::Balanced => matches!(
            risk,
            RiskClass::StatefulExec | RiskClass::Destructive | RiskClass::ExternalSideEffect
        ),
        AutonomyLevel::Aggressive => {
            matches!(risk, RiskClass::Destructive | RiskClass::ExternalSideEffect)
        }
        AutonomyLevel::Yolo => false,
    }
}

pub fn tool_risk(call: &ToolCall) -> RiskClass {
    match call {
        ToolCall::ReadRange { .. }
        | ToolCall::ReadSymbol { .. }
        | ToolCall::GrepSearch { .. }
        | ToolCall::GlobSearch { .. }
        | ToolCall::ListDir { .. }
        | ToolCall::SemanticSearch { .. }
        | ToolCall::ParallelSearch { .. }
        | ToolCall::PollCommand { .. } => RiskClass::SafeRead,
        ToolCall::CancelCommand { .. } => RiskClass::SafeExec,
        ToolCall::RunCommand { risk_class, .. } => risk_class.clone(),
        ToolCall::EditPatch { .. }
        | ToolCall::WriteFile { .. }
        | ToolCall::PlanUpdate { .. }
        | ToolCall::CheckpointCreate { .. }
        | ToolCall::MemoryStage { .. }
        | ToolCall::MemoryCommit { .. } => RiskClass::StatefulExec,
        ToolCall::CheckpointRestore { .. } => RiskClass::Destructive,
    }
}

pub fn requires_tool_approval(autonomy: AutonomyLevel, call: &ToolCall) -> bool {
    requires_approval(autonomy, tool_risk(call))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CheckpointScope, MemoryScope, ToolCall};

    #[test]
    fn slash_override_wins_over_auto_routing() {
        let (override_intent, rest) = parse_slash_override("/plan fix the bug");
        assert_eq!(rest, "fix the bug");

        let decision = decide_intent(rest, override_intent, true, false);
        assert_eq!(
            decision,
            RouterDecision {
                intent: RouterIntent::Plan,
                source: "slash_override".to_string(),
            }
        );
    }

    #[test]
    fn diagnostics_force_verify_when_not_overridden() {
        let decision = decide_intent("implement the fix", None, false, true);
        assert_eq!(decision.intent, RouterIntent::Verify);
    }

    #[test]
    fn aggressive_autonomy_only_escalates_destructive_work() {
        assert!(!requires_approval(
            AutonomyLevel::Aggressive,
            RiskClass::StatefulExec
        ));
        assert!(requires_approval(
            AutonomyLevel::Aggressive,
            RiskClass::Destructive
        ));
    }

    #[test]
    fn yolo_autonomy_never_requires_approval() {
        for risk in [
            RiskClass::SafeRead,
            RiskClass::SafeExec,
            RiskClass::StatefulExec,
            RiskClass::Destructive,
            RiskClass::ExternalSideEffect,
        ] {
            assert!(
                !requires_approval(AutonomyLevel::Yolo, risk),
                "yolo should auto-approve"
            );
        }
    }

    #[test]
    fn balanced_autonomy_requires_approval_for_stateful_write_tools() {
        let write = ToolCall::WriteFile {
            file_path: "src/lib.rs".to_string(),
            content: "changed".to_string(),
        };
        let patch = ToolCall::EditPatch {
            file_path: "src/lib.rs".to_string(),
            old_string: "old".to_string(),
            new_string: "new".to_string(),
        };

        assert_eq!(tool_risk(&write), RiskClass::StatefulExec);
        assert_eq!(tool_risk(&patch), RiskClass::StatefulExec);
        assert!(requires_tool_approval(AutonomyLevel::Balanced, &write));
        assert!(requires_tool_approval(AutonomyLevel::Balanced, &patch));
    }

    #[test]
    fn aggressive_autonomy_allows_edits_but_blocks_destructive_restore() {
        let write = ToolCall::WriteFile {
            file_path: "src/lib.rs".to_string(),
            content: "changed".to_string(),
        };
        let restore = ToolCall::CheckpointRestore {
            checkpoint_id: "cp-1".to_string(),
        };

        assert!(!requires_tool_approval(AutonomyLevel::Aggressive, &write));
        assert_eq!(tool_risk(&restore), RiskClass::Destructive);
        assert!(requires_tool_approval(AutonomyLevel::Aggressive, &restore));
    }

    #[test]
    fn safe_reads_remain_auto_approved_outside_conservative_mode() {
        let read = ToolCall::ReadRange {
            file_path: "README.md".to_string(),
            offset: None,
            limit: None,
        };
        let checkpoint = ToolCall::CheckpointCreate {
            name: "phase".to_string(),
            scope: CheckpointScope::Phase,
        };
        let memory = ToolCall::MemoryStage {
            scope: MemoryScope::Project,
            category: "decision".to_string(),
            content: "keep".to_string(),
        };

        assert_eq!(tool_risk(&read), RiskClass::SafeRead);
        assert!(!requires_tool_approval(AutonomyLevel::Balanced, &read));
        assert_eq!(tool_risk(&checkpoint), RiskClass::StatefulExec);
        assert_eq!(tool_risk(&memory), RiskClass::StatefulExec);
    }
}
