use super::types::{McpServerSnapshot, McpServerStatus, McpSnapshot};

pub fn discover_mcp_tools(_workspace_root: &std::path::Path) -> McpSnapshot {
    McpSnapshot {
        ready: true,
        servers: vec![McpServerSnapshot {
            name: "workspace".to_string(),
            status: McpServerStatus::Connected,
            tool_count: 13,
            approval_mode: "aggressive".to_string(),
            last_error: None,
        }],
        tools: vec![
            "read_range".to_string(),
            "grep_search".to_string(),
            "glob_search".to_string(),
            "semantic_search".to_string(),
            "parallel_search".to_string(),
            "list_dir".to_string(),
            "edit_patch".to_string(),
            "write_file".to_string(),
            "run_command".to_string(),
            "poll_command".to_string(),
            "checkpoint_create".to_string(),
            "checkpoint_restore".to_string(),
            "plan_update".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn workspace_server_is_exposed_as_ready_snapshot() {
        let dir = tempdir().unwrap();
        let snapshot = discover_mcp_tools(dir.path());
        assert!(snapshot.ready);
        assert_eq!(snapshot.servers.len(), 1);
        assert!(snapshot.tools.contains(&"run_command".to_string()));
    }
}
