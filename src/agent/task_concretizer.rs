use serde::{Deserialize, Serialize};

/// Task contract that concretizes a vague user request into actionable work
/// Spec: docs/charm-strategy.md Task contract
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContract {
    /// Abstraction score used to choose grounding depth.
    #[serde(default)]
    pub abstraction_score: f64,
    /// What must be accomplished
    pub objective: String,
    /// What can be touched (files, modules, areas)
    pub scope: Vec<String>,
    /// Relevant files, symbols, tests, commands
    pub repo_anchors: Vec<RepoAnchor>,
    /// What success means
    pub acceptance: Vec<String>,
    /// How to prove success
    pub verification: Vec<String>,
    /// Impacted areas and risks
    pub side_effects: Vec<String>,
    /// What Charm will assume automatically
    pub assumptions: Vec<String>,
    /// What must be asked before proceeding
    pub open_questions: Vec<String>,
    /// shallow / normal / deep / exhaustive
    pub depth: ExecutionDepth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoAnchor {
    pub file_path: Option<String>,
    pub symbol: Option<String>,
    pub test_command: Option<String>,
    pub related_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDepth {
    Shallow,
    Normal,
    Deep,
    Exhaustive,
}

/// Concretizer that transforms vague requests into task contracts
pub struct TaskConcretizer;

impl TaskConcretizer {
    pub fn new() -> Self {
        Self
    }

    /// Analyze user request and compute abstraction score
    /// Returns score between 0.0 (concrete) and 1.0 (abstract)
    pub fn score_abstraction(request: &str) -> f64 {
        let mut score = 0.0f64;
        let lower = request.to_lowercase();

        // Keywords indicating high abstraction (vague requests)
        if lower.contains("make better") || lower.contains("improve everything") {
            return 0.85;
        }

        // Keywords indicating moderate abstraction
        if lower.contains("refactor") || lower.contains("improve") || lower.contains("optimize") {
            score += 0.4;
        }
        if lower.contains("everywhere") || lower.contains("all over") {
            score += 0.4;
        }

        // Keywords indicating low abstraction (concrete requests)
        if lower.contains("fix") || lower.contains("update") {
            score += 0.1;
        }

        // Longer requests tend to be more concrete
        let word_count = request.split_whitespace().count();
        if word_count < 5 {
            score += 0.15;
        }

        // Specific file mentions reduce abstraction significantly
        if lower.contains(".rs")
            || lower.contains(".py")
            || lower.contains(".js")
            || lower.contains(".md")
        {
            score -= 0.3;
        }

        // Line numbers or function names indicate concrete requests
        if request.contains("line ") || request.contains("::") {
            score -= 0.2;
        }

        score.clamp(0.0, 1.0)
    }

    /// Decide action based on abstraction score
    pub fn decide_action(score: f64) -> ConcretizationAction {
        match score {
            s if s < 0.35 => ConcretizationAction::Proceed,
            s if s < 0.70 => ConcretizationAction::InspectDeeper,
            _ => ConcretizationAction::AskClarification,
        }
    }

    /// Generate task contract from user request
    pub fn concretize(&self, request: &str) -> ConcretizationResult {
        let score = Self::score_abstraction(request);
        let action = Self::decide_action(score);

        match action {
            ConcretizationAction::Proceed => {
                let contract = self.build_contract(request, score);
                ConcretizationResult::Ready(contract)
            }
            ConcretizationAction::InspectDeeper => {
                // In auto mode, make conservative assumptions
                let contract = self.build_contract_with_assumptions(request, score);
                ConcretizationResult::Ready(contract)
            }
            ConcretizationAction::AskClarification => {
                let question = self.generate_high_leverage_question(request);
                ConcretizationResult::NeedsClarification(question)
            }
        }
    }

    pub fn concretize_for_auto(&self, request: &str) -> TaskContract {
        match self.concretize(request) {
            ConcretizationResult::Ready(contract) => contract,
            ConcretizationResult::NeedsClarification(question) => {
                let score = Self::score_abstraction(request);
                let mut contract = self.build_contract_with_assumptions(request, score);
                contract.open_questions.push(question);
                contract.depth = ExecutionDepth::Deep;
                contract
            }
        }
    }

    fn build_contract(&self, request: &str, score: f64) -> TaskContract {
        let lower = request.to_lowercase();
        let verification = if lower.contains("test") || lower.contains("verify") {
            vec!["Run relevant tests".to_string(), "Build passes".to_string()]
        } else {
            vec![
                "Inspect relevant workspace context".to_string(),
                "Verify with the narrowest available check".to_string(),
            ]
        };

        TaskContract {
            abstraction_score: score,
            objective: request.to_string(),
            scope: vec!["To be determined from repo inspection".to_string()],
            repo_anchors: Vec::new(),
            acceptance: vec!["User request is addressed with grounded evidence".to_string()],
            verification,
            side_effects: Vec::new(),
            assumptions: vec!["Assuming standard project structure".to_string()],
            open_questions: Vec::new(),
            depth: ExecutionDepth::Normal,
        }
    }

    fn build_contract_with_assumptions(&self, request: &str, score: f64) -> TaskContract {
        TaskContract {
            abstraction_score: score,
            objective: request.to_string(),
            scope: vec!["Conservative scope - will expand after initial inspection".to_string()],
            repo_anchors: Vec::new(),
            acceptance: vec!["Minimal viable improvement".to_string()],
            verification: vec!["Build passes".to_string(), "Basic smoke test".to_string()],
            side_effects: vec!["May affect related code paths".to_string()],
            assumptions: vec![
                "Assuming common patterns".to_string(),
                "Will verify before committing".to_string(),
            ],
            open_questions: vec!["Exact scope to be determined".to_string()],
            depth: ExecutionDepth::Normal,
        }
    }

    fn generate_high_leverage_question(&self, request: &str) -> String {
        // Generate a question that maximally reduces ambiguity
        if request.to_lowercase().contains("refactor") || request.to_lowercase().contains("improve")
        {
            "Which specific file or module should I focus on first?".to_string()
        } else if request.to_lowercase().contains("fix") {
            "Can you provide the error message or describe the bug symptoms?".to_string()
        } else if request.to_lowercase().contains("add")
            || request.to_lowercase().contains("implement")
        {
            "What is the expected behavior or API interface?".to_string()
        } else {
            "Could you specify which files or components are involved?".to_string()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConcretizationAction {
    Proceed,
    InspectDeeper,
    AskClarification,
}

#[derive(Debug, Clone)]
pub enum ConcretizationResult {
    Ready(TaskContract),
    NeedsClarification(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_abstraction_score_concrete() {
        let request = "Fix the off-by-one error in src/parser.rs line 42";
        let score = TaskConcretizer::score_abstraction(request);
        assert!(
            score < 0.35,
            "Expected low score for concrete request, got {}",
            score
        );
    }

    #[test]
    fn test_abstraction_score_vague() {
        let request = "Improve everything everywhere";
        let score = TaskConcretizer::score_abstraction(request);
        assert!(
            score > 0.70,
            "Expected high score for vague request, got {}",
            score
        );
    }

    #[test]
    fn test_abstraction_score_moderate() {
        let request = "Refactor error handling";
        let score = TaskConcretizer::score_abstraction(request);
        assert!(
            score >= 0.35 && score < 0.70,
            "Expected moderate score, got {}",
            score
        );
    }

    #[test]
    fn test_decide_action() {
        assert_eq!(
            TaskConcretizer::decide_action(0.2),
            ConcretizationAction::Proceed
        );
        assert_eq!(
            TaskConcretizer::decide_action(0.5),
            ConcretizationAction::InspectDeeper
        );
        assert_eq!(
            TaskConcretizer::decide_action(0.8),
            ConcretizationAction::AskClarification
        );
    }

    #[test]
    fn test_concretize_concrete() {
        let concretizer = TaskConcretizer::new();
        let result = concretizer.concretize("Fix typo in README.md line 10");
        match result {
            ConcretizationResult::Ready(contract) => {
                assert_eq!(contract.objective, "Fix typo in README.md line 10");
            }
            ConcretizationResult::NeedsClarification(_) => {
                panic!("Expected Ready for concrete request");
            }
        }
    }

    #[test]
    fn test_concretize_abstract() {
        let concretizer = TaskConcretizer::new();
        let result = concretizer.concretize("Improve everything everywhere");
        match result {
            ConcretizationResult::NeedsClarification(_) => {}
            ConcretizationResult::Ready(_) => {
                panic!("Expected NeedsClarification for abstract request");
            }
        }
    }
}
