use super::types::{McpServerSnapshot, McpServerStatus, McpSnapshot};
use crate::core::ToolResult;
use anyhow::{Context, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Duration, timeout};

#[derive(Debug, Default, Deserialize)]
struct McpRegistry {
    #[serde(default)]
    servers: Vec<ConfiguredMcpServer>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfiguredMcpServer {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    approval_mode: Option<String>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    tools: Vec<ConfiguredMcpTool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ConfiguredMcpTool {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        description: Option<String>,
    },
}

impl ConfiguredMcpTool {
    fn name(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::Detailed { name, description } => {
                let _ = description;
                name.clone()
            }
        }
    }
}

impl ConfiguredMcpServer {
    fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(ConfiguredMcpTool::name).collect()
    }

    fn approval_mode(&self) -> String {
        self.approval_mode
            .clone()
            .unwrap_or_else(|| "manual".to_string())
    }

    fn transport(&self) -> &str {
        self.transport.as_deref().unwrap_or("newline")
    }
}

pub fn discover_mcp_tools(workspace_root: &Path) -> McpSnapshot {
    discover_mcp_tools_with_path(workspace_root, None)
}

pub async fn refresh_mcp_snapshot(workspace_root: &Path) -> anyhow::Result<McpSnapshot> {
    let registry = load_registry_checked(workspace_root)?;
    if registry.servers.is_empty() {
        return Ok(McpSnapshot::default());
    }

    let mut tools = Vec::new();
    let mut servers = Vec::new();

    for server in registry.servers {
        let configured_tools = server.tool_names();
        let approval_mode = server.approval_mode();
        let server_name = server.name.clone();

        if server.disabled {
            for tool in &configured_tools {
                tools.push(format!("{}:{}", server_name, tool));
            }
            servers.push(McpServerSnapshot {
                name: server_name,
                status: McpServerStatus::Disconnected,
                tool_count: configured_tools.len(),
                approval_mode,
                last_error: Some("disabled".to_string()),
            });
            continue;
        }

        if !command_available(&server.command, None) {
            for tool in &configured_tools {
                tools.push(format!("{}:{}", server_name, tool));
            }
            servers.push(McpServerSnapshot {
                name: server_name,
                status: McpServerStatus::Degraded,
                tool_count: configured_tools.len(),
                approval_mode,
                last_error: Some(format!("command not found: {}", server.command)),
            });
            continue;
        }

        match probe_tools(workspace_root, &server).await {
            Ok(discovered_tools) => {
                let effective_tools = if discovered_tools.is_empty() {
                    configured_tools
                } else {
                    discovered_tools
                };
                for tool in &effective_tools {
                    tools.push(format!("{}:{}", server_name, tool));
                }
                servers.push(McpServerSnapshot {
                    name: server_name,
                    status: McpServerStatus::Connected,
                    tool_count: effective_tools.len(),
                    approval_mode,
                    last_error: None,
                });
            }
            Err(error) => {
                for tool in &configured_tools {
                    tools.push(format!("{}:{}", server_name, tool));
                }
                servers.push(McpServerSnapshot {
                    name: server_name,
                    status: McpServerStatus::Degraded,
                    tool_count: configured_tools.len(),
                    approval_mode,
                    last_error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(McpSnapshot {
        ready: servers
            .iter()
            .any(|server| server.status == McpServerStatus::Connected),
        servers,
        tools,
    })
}

pub async fn call_mcp_tool(
    workspace_root: &Path,
    server_name: &str,
    tool_name: &str,
    arguments: Value,
) -> anyhow::Result<ToolResult> {
    let registry = load_registry_checked(workspace_root)?;
    let server = registry
        .servers
        .iter()
        .find(|server| server.name == server_name)
        .with_context(|| format!("unknown MCP server: {server_name}"))?;

    if server.disabled {
        return Err(anyhow!("MCP server {server_name} is disabled"));
    }

    let mut client = McpClient::spawn(workspace_root, server).await?;
    client.initialize().await?;
    let result = client.call_tool(tool_name, arguments).await?;
    client.shutdown().await;

    Ok(ToolResult {
        success: !result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        output: render_tool_output(&result),
        error: None,
        metadata: Some(json!({
            "server": server_name,
            "tool": tool_name,
            "raw": result,
        })),
    })
}

fn discover_mcp_tools_with_path(workspace_root: &Path, path_override: Option<&str>) -> McpSnapshot {
    let registry = load_registry(workspace_root).unwrap_or_default();

    let mut tools = Vec::new();
    let servers = registry
        .servers
        .into_iter()
        .map(|server| {
            let tool_names = server.tool_names();
            let approval_mode = server.approval_mode();
            let server_name = server.name.clone();

            for tool in &tool_names {
                tools.push(format!("{}:{}", server_name, tool));
            }

            if server.disabled {
                return McpServerSnapshot {
                    name: server_name,
                    status: McpServerStatus::Disconnected,
                    tool_count: tool_names.len(),
                    approval_mode,
                    last_error: Some("disabled".to_string()),
                };
            }

            let ready = command_available(&server.command, path_override);
            McpServerSnapshot {
                name: server_name,
                status: if ready {
                    McpServerStatus::Connected
                } else {
                    McpServerStatus::Degraded
                },
                tool_count: tool_names.len(),
                approval_mode,
                last_error: if ready {
                    None
                } else {
                    Some(format!("command not found: {}", server.command))
                },
            }
        })
        .collect::<Vec<_>>();

    McpSnapshot {
        ready: servers
            .iter()
            .any(|server| server.status == McpServerStatus::Connected),
        servers,
        tools,
    }
}

fn load_registry(workspace_root: &Path) -> Option<McpRegistry> {
    let registry_path = workspace_root
        .join(".charm")
        .join("mcp")
        .join("servers.json");
    if !registry_path.exists() {
        return None;
    }

    std::fs::read_to_string(registry_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<McpRegistry>(&raw).ok())
}

fn load_registry_checked(workspace_root: &Path) -> anyhow::Result<McpRegistry> {
    let registry_path = workspace_root
        .join(".charm")
        .join("mcp")
        .join("servers.json");
    if !registry_path.exists() {
        return Ok(McpRegistry::default());
    }

    let raw = std::fs::read_to_string(&registry_path)
        .with_context(|| format!("failed to read {}", registry_path.display()))?;
    serde_json::from_str::<McpRegistry>(&raw)
        .with_context(|| format!("failed to parse {}", registry_path.display()))
}

async fn probe_tools(
    workspace_root: &Path,
    server: &ConfiguredMcpServer,
) -> anyhow::Result<Vec<String>> {
    let mut client = McpClient::spawn(workspace_root, server).await?;
    client.initialize().await?;
    let tools = client.list_tools().await?;
    client.shutdown().await;
    Ok(tools)
}

fn render_tool_output(result: &Value) -> String {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text").and_then(Value::as_str).map(str::to_string)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if !text.is_empty() {
        return text.join("\n");
    }

    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
}

fn command_available(command: &str, path_override: Option<&str>) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return PathBuf::from(command).is_file();
    }

    let path_var = path_override
        .map(OsString::from)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();

    std::env::split_paths(&path_var).any(|dir| executable_exists(&dir, command))
}

fn executable_exists(dir: &Path, command: &str) -> bool {
    let candidate = dir.join(command);
    if candidate.is_file() {
        return true;
    }

    #[cfg(windows)]
    {
        let exe = dir.join(format!("{command}.exe"));
        if exe.is_file() {
            return true;
        }
    }

    false
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: i64,
}

impl McpClient {
    async fn spawn(workspace_root: &Path, server: &ConfiguredMcpServer) -> anyhow::Result<Self> {
        if server.transport() != "newline" {
            return Err(anyhow!(
                "unsupported MCP transport for {}: {}",
                server.name,
                server.transport()
            ));
        }

        let mut command = Command::new(&server.command);
        command
            .args(&server.args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.kill_on_drop(true);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn MCP server {}", server.name))?;
        let stdin = child.stdin.take().context("missing MCP stdin")?;
        let stdout = child.stdout.take().context("missing MCP stdout")?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 0,
        })
    }

    async fn initialize(&mut self) -> anyhow::Result<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {
                        "tools": { "listChanged": true }
                    },
                    "clientInfo": {
                        "name": "charm",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    async fn list_tools(&mut self) -> anyhow::Result<Vec<String>> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
            .collect())
    }

    async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> anyhow::Result<Value> {
        self.request(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        )
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        self.wait_for_response(id).await
    }

    async fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn send(&mut self, payload: Value) -> anyhow::Result<()> {
        let message = serde_json::to_string(&payload)?;
        self.stdin.write_all(message.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn wait_for_response(&mut self, expected_id: i64) -> anyhow::Result<Value> {
        loop {
            let line = timeout(Duration::from_secs(3), self.stdout.next_line())
                .await
                .context("timed out waiting for MCP response")??
                .ok_or_else(|| anyhow!("MCP server closed stdout"))?;

            let payload: Value = serde_json::from_str(&line)
                .with_context(|| format!("invalid MCP response: {line}"))?;
            if payload.get("id").and_then(Value::as_i64) != Some(expected_id) {
                continue;
            }

            if let Some(error) = payload.get("error") {
                return Err(anyhow!("MCP request failed: {error}"));
            }

            return Ok(payload.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn shutdown(mut self) {
        drop(self.stdin);
        let _ = timeout(Duration::from_millis(250), self.child.wait()).await;
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn workspace_server_is_exposed_as_ready_snapshot() {
        let dir = tempdir().unwrap();
        let snapshot = discover_mcp_tools(dir.path());
        assert!(!snapshot.ready);
        assert_eq!(snapshot.servers.len(), 0);
    }

    #[test]
    fn loads_mcp_servers_from_registry_file() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".charm").join("mcp")).unwrap();
        fs::write(
            dir.path().join(".charm").join("mcp").join("servers.json"),
            serde_json::json!({
                "servers": [
                    {
                        "name": "workspace",
                        "command": "workspace-mcp",
                        "approval_mode": "aggressive",
                        "tools": ["read_range", "grep_search"]
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let bin_dir = tempdir().unwrap();
        let workspace_mcp = bin_dir.path().join("workspace-mcp");
        fs::write(&workspace_mcp, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&workspace_mcp).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&workspace_mcp, permissions).unwrap();

        let snapshot = discover_mcp_tools_with_path(
            dir.path(),
            Some(bin_dir.path().to_string_lossy().as_ref()),
        );

        assert!(snapshot.ready);
        assert_eq!(snapshot.servers.len(), 1);
        assert_eq!(snapshot.servers[0].name, "workspace");
        assert_eq!(snapshot.servers[0].status, McpServerStatus::Connected);
        assert_eq!(snapshot.tools.len(), 2);
    }

    #[tokio::test]
    async fn refresh_mcp_snapshot_performs_stdio_initialize_and_list() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".charm").join("mcp")).unwrap();

        let script = dir.path().join("fake-mcp.sh");
        fs::write(
            &script,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{\"tools\":{\"listChanged\":true}},\"protocolVersion\":\"2025-03-26\",\"serverInfo\":{\"name\":\"workspace\",\"version\":\"0.1.0\"}}}' ;;\n    *'\"method\":\"notifications/initialized\"'*) : ;;\n    *'\"method\":\"tools/list\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"ping\",\"inputSchema\":{\"type\":\"object\"}}]}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        fs::write(
            dir.path().join(".charm").join("mcp").join("servers.json"),
            serde_json::json!({
                "servers": [
                    {
                        "name": "workspace",
                        "command": script,
                        "transport": "newline"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let snapshot = refresh_mcp_snapshot(dir.path()).await.expect("refresh");
        assert!(snapshot.ready);
        assert_eq!(snapshot.servers.len(), 1);
        assert_eq!(snapshot.servers[0].status, McpServerStatus::Connected);
        assert!(snapshot.tools.contains(&"workspace:echo".to_string()));
    }

    #[tokio::test]
    async fn call_mcp_tool_runs_tools_call_flow() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".charm").join("mcp")).unwrap();

        let script = dir.path().join("fake-mcp.sh");
        fs::write(
            &script,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"method\":\"initialize\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"capabilities\":{\"tools\":{\"listChanged\":true}},\"protocolVersion\":\"2025-03-26\",\"serverInfo\":{\"name\":\"workspace\",\"version\":\"0.1.0\"}}}' ;;\n    *'\"method\":\"notifications/initialized\"'*) : ;;\n    *'\"method\":\"tools/call\"'*) echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"echo ok\"}],\"isError\":false}}' ;;\n  esac\ndone\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        fs::write(
            dir.path().join(".charm").join("mcp").join("servers.json"),
            serde_json::json!({
                "servers": [
                    {
                        "name": "workspace",
                        "command": script,
                        "transport": "newline"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let result = call_mcp_tool(
            dir.path(),
            "workspace",
            "echo",
            serde_json::json!({"value": "hi"}),
        )
        .await
        .expect("call");
        assert!(result.success);
        assert!(result.output.contains("echo ok"));
    }
}
