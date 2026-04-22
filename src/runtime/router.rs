use crate::core::RiskClass;

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
