use crate::agent::runner::AgentRunner;
use crate::orchestrator::types::{
    Subtask, SubtaskResult, TaskDecomposition, Verdict, Verification,
};
use std::collections::{HashMap, HashSet, VecDeque};

pub struct Broker {
    max_retries: usize,
}

impl Broker {
    pub fn new() -> Self {
        Self { max_retries: 2 }
    }

    pub async fn execute_plan(
        &self,
        decomposition: TaskDecomposition,
        runner: &mut AgentRunner,
    ) -> anyhow::Result<Vec<SubtaskResult>> {
        let order = Self::topological_sort(&decomposition.subtasks)?;
        let mut results: HashMap<String, SubtaskResult> = HashMap::new();

        for subtask_id in &order {
            let subtask = decomposition
                .subtasks
                .iter()
                .find(|s| s.id == *subtask_id)
                .unwrap();
            println!(
                "\n[Broker] Executing subtask {}: {}",
                subtask.id, subtask.description
            );

            let mut retries = 0;
            loop {
                let task_text = format!("[Subtask {}] {}", subtask.id, subtask.description);
                let subtask_results = runner.run_task(&task_text).await?;

                let files_changed = Self::extract_files_changed(&subtask_results);
                let output = subtask_results
                    .iter()
                    .map(|r| {
                        if r.success {
                            r.output.clone()
                        } else {
                            r.error.clone().unwrap_or_default()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let result = SubtaskResult {
                    subtask_id: subtask.id.clone(),
                    success: subtask_results.iter().all(|r| r.success),
                    output,
                    files_changed,
                };

                results.insert(subtask.id.clone(), result);
                break;
            }
        }

        Ok(order
            .into_iter()
            .filter_map(|id| results.get(&id).cloned())
            .collect())
    }

    fn topological_sort(subtasks: &[Subtask]) -> anyhow::Result<Vec<String>> {
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();

        for st in subtasks {
            in_degree.entry(st.id.clone()).or_insert(0);
            for dep in &st.dependencies {
                adj.entry(dep.clone()).or_default().push(st.id.clone());
                *in_degree.entry(st.id.clone()).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(id, _)| id.clone())
            .collect();

        let mut order = Vec::new();

        while let Some(id) = queue.pop_front() {
            order.push(id.clone());
            if let Some(children) = adj.get(&id) {
                for child in children {
                    if let Some(deg) = in_degree.get_mut(child) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(child.clone());
                        }
                    }
                }
            }
        }

        if order.len() != subtasks.len() {
            return Err(anyhow::anyhow!("Dependency cycle detected in subtasks"));
        }

        Ok(order)
    }

    fn extract_files_changed(results: &[crate::core::ToolResult]) -> Vec<String> {
        let mut files = HashSet::new();
        for result in results {
            if let Some(meta) = &result.metadata {
                if let Some(fp) = meta.get("file_path").and_then(|v| v.as_str()) {
                    files.insert(fp.to_string());
                }
            }
        }
        files.into_iter().collect()
    }
}
