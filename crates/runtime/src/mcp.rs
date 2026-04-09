use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::McpServerEntry;

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
}

pub fn list_tools(
    servers: &[McpServerEntry],
    server_name: &str,
) -> Result<Vec<McpToolInfo>, String> {
    let server = find_server(servers, server_name)?;
    let response = stdio_roundtrip(
        server,
        vec![
            rpc_request(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "harness", "version": "0.1.0" }
                }),
            ),
            rpc_request(2, "tools/list", json!({})),
        ],
    )?;

    let tools = response
        .last()
        .and_then(|value| value.get("result"))
        .and_then(|value| value.get("tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid tools/list response".to_string())?;

    Ok(tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str()?.to_string();
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(McpToolInfo { name, description })
        })
        .collect())
}

pub fn call_tool(
    servers: &[McpServerEntry],
    server_name: &str,
    tool_name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let server = find_server(servers, server_name)?;
    let responses = stdio_roundtrip(
        server,
        vec![
            rpc_request(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "harness", "version": "0.1.0" }
                }),
            ),
            rpc_request(
                2,
                "tools/call",
                json!({
                    "name": tool_name,
                    "arguments": arguments
                }),
            ),
        ],
    )?;

    responses
        .last()
        .and_then(|value| value.get("result"))
        .cloned()
        .ok_or_else(|| "invalid tools/call response".to_string())
}

fn find_server<'a>(
    servers: &'a [McpServerEntry],
    name: &str,
) -> Result<&'a McpServerEntry, String> {
    servers
        .iter()
        .find(|server| server.name == name)
        .ok_or_else(|| format!("mcp server not found: {name}"))
}

fn stdio_roundtrip(server: &McpServerEntry, requests: Vec<Value>) -> Result<Vec<Value>, String> {
    if server.transport != "stdio" {
        return Err(format!(
            "unsupported transport for now: {}",
            server.transport
        ));
    }

    let Some(command) = server.command.as_ref() else {
        return Err("stdio server is missing command".to_string());
    };

    let mut child = Command::new("zsh")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to open MCP stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open MCP stdout".to_string())?;
    let mut reader = BufReader::new(stdout);

    let mut responses = Vec::new();
    for request in requests {
        write_message(&mut stdin, &request)?;
        responses.push(read_message(&mut reader)?);
    }

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    Ok(responses)
}

fn rpc_request(id: u64, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
}

fn write_message(writer: &mut dyn Write, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|err| err.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer
        .write_all(header.as_bytes())
        .and_then(|_| writer.write_all(&body))
        .and_then(|_| writer.flush())
        .map_err(|err| err.to_string())
}

fn read_message(reader: &mut dyn BufRead) -> Result<Value, String> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if bytes == 0 {
            return Err("unexpected EOF from MCP server".to_string());
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|err| err.to_string())?,
                );
            }
        }
    }

    let length = content_length.ok_or_else(|| "missing content-length header".to_string())?;
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|err| err.to_string())?;
    serde_json::from_slice(&body).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use serde_json::json;

    use crate::McpServerEntry;

    use super::{call_tool, list_tools};

    fn has_node() -> bool {
        Command::new("node").arg("--version").output().is_ok()
    }

    fn mock_server() -> McpServerEntry {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf();
        let script = workspace_root.join("scripts/mock_mcp_echo.js");
        McpServerEntry {
            name: "mock-echo".to_string(),
            transport: "stdio".to_string(),
            command: Some(format!("node {}", script.display())),
            url: None,
            enabled: true,
            source: "workspace".to_string(),
        }
    }

    #[test]
    fn lists_tools_from_mock_server() {
        if !has_node() {
            eprintln!("node not found; skipping MCP mock test");
            return;
        }
        let tools = list_tools(&[mock_server()], "mock-echo").unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(
            tools[0].description.as_deref(),
            Some("Return the provided arguments as structured content.")
        );
    }

    #[test]
    fn calls_tool_on_mock_server() {
        if !has_node() {
            eprintln!("node not found; skipping MCP mock test");
            return;
        }
        let result = call_tool(
            &[mock_server()],
            "mock-echo",
            "echo",
            json!({ "text": "hello" }),
        )
        .unwrap();

        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], r#"{"text":"hello"}"#);
    }
}
