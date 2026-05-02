use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::Protocol;

const MAX_TOOL_OUTPUT_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    Auto,
    RequireApproval,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicTool {
    pub name: &'static str,
    pub description: &'static str,
    pub default_policy: ToolPolicy,
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub default_policy: ToolPolicy,
    pub input_schema: JsonValue,
}

#[derive(Debug, Clone)]
pub struct NormalizedToolCall {
    pub call_id: String,
    pub name: String,
    pub input: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingToolCall {
    pub pending_call_id: String,
    pub call_id: String,
    pub name: String,
    pub input: JsonValue,
}

#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub call_id: String,
    pub output: String,
    pub is_error: bool,
    pub error_kind: Option<String>,
}

fn truncate_tool_output(text: &str) -> String {
    let mut iter = text.chars();
    let kept: String = iter.by_ref().take(MAX_TOOL_OUTPUT_CHARS).collect();
    let omitted = iter.count();
    if omitted == 0 {
        return text.to_string();
    }
    format!("{kept}\n\n[tool output truncated: {omitted} characters omitted]")
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read",
            description: "Read a UTF-8 text file from disk by path.",
            default_policy: ToolPolicy::Auto,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to read."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "write",
            description: "Write UTF-8 text content to a file path.",
            default_policy: ToolPolicy::RequireApproval,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to write."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full UTF-8 file content to write."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "bash",
            description: "Run a shell command via `bash -lc`.",
            default_policy: ToolPolicy::RequireApproval,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory."
                    },
                    "timeout_sec": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 600,
                        "description": "Optional timeout in seconds."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        },
    ]
}

pub fn public_tools() -> Vec<PublicTool> {
    tool_definitions()
        .into_iter()
        .map(|tool| PublicTool {
            name: tool.name,
            description: tool.description,
            default_policy: tool.default_policy,
        })
        .collect()
}

pub fn default_policy(tool_name: &str) -> Option<ToolPolicy> {
    tool_definitions()
        .into_iter()
        .find(|tool| tool.name == tool_name)
        .map(|tool| tool.default_policy)
}

pub fn extract_tool_calls(
    protocol: Protocol,
    assistant_content: &JsonValue,
) -> Vec<NormalizedToolCall> {
    match protocol {
        Protocol::Anthropic => extract_anthropic_tool_calls(assistant_content),
        Protocol::Openai => extract_openai_tool_calls(assistant_content),
    }
}

fn extract_anthropic_tool_calls(assistant_content: &JsonValue) -> Vec<NormalizedToolCall> {
    let mut out = Vec::new();
    let Some(blocks) = assistant_content.as_array() else {
        return out;
    };
    for block in blocks {
        let is_tool_use = block.get("type").and_then(|v| v.as_str()) == Some("tool_use");
        if !is_tool_use {
            continue;
        }
        let Some(name) = block.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(call_id) = block.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
        out.push(NormalizedToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            input,
        });
    }
    out
}

fn extract_openai_tool_calls(assistant_content: &JsonValue) -> Vec<NormalizedToolCall> {
    let mut out = Vec::new();
    let Some(items) = assistant_content.as_array() else {
        return out;
    };
    for item in items {
        let is_function_call = item.get("type").and_then(|v| v.as_str()) == Some("function_call");
        if !is_function_call {
            continue;
        }
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let call_id = item
            .get("call_id")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("id").and_then(|v| v.as_str()));
        let Some(call_id) = call_id else {
            continue;
        };
        let input = match item.get("arguments") {
            Some(JsonValue::String(s)) => {
                serde_json::from_str::<JsonValue>(s).unwrap_or_else(|_| json!({ "raw": s }))
            }
            Some(v) => v.clone(),
            None => json!({}),
        };
        out.push(NormalizedToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            input,
        });
    }
    out
}

pub fn tool_result_entry(protocol: Protocol, results: &[ToolExecutionResult]) -> JsonValue {
    let content = match protocol {
        Protocol::Anthropic => {
            let blocks: Vec<JsonValue> = results
                .iter()
                .map(|r| {
                    let mut block = json!({
                        "type": "tool_result",
                        "tool_use_id": r.call_id,
                        "content": r.output,
                    });
                    if r.is_error {
                        block["is_error"] = json!(true);
                        if let Some(kind) = &r.error_kind {
                            block["error"] = json!({ "kind": kind });
                        }
                    }
                    block
                })
                .collect();
            JsonValue::Array(blocks)
        }
        Protocol::Openai => {
            let blocks: Vec<JsonValue> = results
                .iter()
                .map(|r| {
                    let mut block = json!({
                        "type": "function_call_output",
                        "call_id": r.call_id,
                        "output": r.output,
                    });
                    if r.is_error {
                        block["is_error"] = json!(true);
                        if let Some(kind) = &r.error_kind {
                            block["error"] = json!({ "kind": kind });
                        }
                    }
                    block
                })
                .collect();
            JsonValue::Array(blocks)
        }
    };

    json!({
        "role": "user",
        "content": content,
    })
}

pub fn match_allow_rule(rule: &str, tool_name: &str, input: &JsonValue) -> bool {
    if rule == tool_name {
        return true;
    }
    if tool_name != "bash" {
        return false;
    }
    let (prefix, suffix) = ("bash(", ")");
    if !rule.starts_with(prefix) || !rule.ends_with(suffix) {
        return false;
    }
    let pattern = &rule[prefix.len()..rule.len() - suffix.len()];
    let cmd = input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    wildcard_match(pattern, cmd)
}

pub fn derive_allow_rule(tool_name: &str, input: &JsonValue) -> String {
    if tool_name == "bash" {
        let cmd = input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        return format!("bash({cmd})");
    }
    tool_name.to_string()
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut idx = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 && !pattern.starts_with('*') {
            if !text[idx..].starts_with(part) {
                return false;
            }
            idx += part.len();
            continue;
        }
        if i == parts.len() - 1 && !pattern.ends_with('*') {
            return text[idx..].ends_with(part);
        }
        let Some(found) = text[idx..].find(part) else {
            return false;
        };
        idx += found + part.len();
    }

    if !pattern.ends_with('*')
        && let Some(last) = parts.last()
        && !last.is_empty()
    {
        return text.ends_with(last);
    }
    true
}

pub async fn execute_tool_call(
    call: &NormalizedToolCall,
    cancel_token: &CancellationToken,
) -> ToolExecutionResult {
    let res = match call.name.as_str() {
        "read" => exec_read(&call.input, cancel_token).await,
        "write" => exec_write(&call.input, cancel_token).await,
        "bash" => exec_bash(&call.input, cancel_token).await,
        _ => Err((
            "unknown_tool".to_string(),
            format!("Unknown tool '{}'", call.name),
        )),
    };

    match res {
        Ok(output) => ToolExecutionResult {
            call_id: call.call_id.clone(),
            output: truncate_tool_output(&output),
            is_error: false,
            error_kind: None,
        },
        Err((kind, message)) => ToolExecutionResult {
            call_id: call.call_id.clone(),
            output: truncate_tool_output(&message),
            is_error: true,
            error_kind: Some(kind),
        },
    }
}

async fn exec_read(
    input: &JsonValue,
    cancel_token: &CancellationToken,
) -> Result<String, (String, String)> {
    let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "read requires string field 'path'".to_string(),
        ));
    };

    tokio::select! {
        _ = cancel_token.cancelled() => Err((
            "cancelled".to_string(),
            "read cancelled by user".to_string(),
        )),
        result = tokio::fs::read_to_string(path) => result.map_err(|e| ("io_error".to_string(), format!("read failed: {e}"))),
    }
}

async fn exec_write(
    input: &JsonValue,
    cancel_token: &CancellationToken,
) -> Result<String, (String, String)> {
    let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "write requires string field 'path'".to_string(),
        ));
    };
    let Some(content) = input.get("content").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "write requires string field 'content'".to_string(),
        ));
    };
    tokio::select! {
        _ = cancel_token.cancelled() => Err((
            "cancelled".to_string(),
            "write cancelled by user".to_string(),
        )),
        result = tokio::fs::write(path, content) => result.map_err(|e| ("io_error".to_string(), format!("write failed: {e}"))),
    }?;
    Ok("ok".to_string())
}

async fn exec_bash(
    input: &JsonValue,
    cancel_token: &CancellationToken,
) -> Result<String, (String, String)> {
    let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "bash requires string field 'command'".to_string(),
        ));
    };
    let timeout_sec = input
        .get("timeout_sec")
        .and_then(|v| v.as_u64())
        .unwrap_or(60);
    let cwd = input.get("cwd").and_then(|v| v.as_str());

    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command);
    cmd.kill_on_drop(true);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    let run = async {
        tokio::time::timeout(Duration::from_secs(timeout_sec), cmd.output())
            .await
            .map_err(|_| {
                (
                    "timeout".to_string(),
                    format!("bash timed out after {timeout_sec}s"),
                )
            })?
            .map_err(|e| {
                (
                    "spawn_error".to_string(),
                    format!("bash failed to start: {e}"),
                )
            })
    };
    let output = tokio::select! {
        _ = cancel_token.cancelled() => Err((
            "cancelled".to_string(),
            "bash cancelled by user".to_string(),
        )),
        result = run => result,
    }?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut text = String::new();
    if !stdout.is_empty() {
        text.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }

    if output.status.success() {
        Ok(text)
    } else {
        Err((
            "non_zero_exit".to_string(),
            format!(
                "bash exited with code {:?}\n{}",
                output.status.code(),
                text.trim_end()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_TOOL_OUTPUT_CHARS, truncate_tool_output, wildcard_match};

    #[test]
    fn wildcard_full_match() {
        assert!(wildcard_match("cargo check *", "cargo check --workspace"));
        assert!(wildcard_match("* --help", "cargo check --help"));
        assert!(!wildcard_match("cargo check *", "cargo build"));
    }

    #[test]
    fn truncate_tool_output_keeps_small_output() {
        let s = "hello\nworld";
        assert_eq!(truncate_tool_output(s), s);
    }

    #[test]
    fn truncate_tool_output_limits_large_output() {
        let input = "a".repeat(MAX_TOOL_OUTPUT_CHARS + 123);
        let out = truncate_tool_output(&input);
        assert!(out.starts_with(&"a".repeat(MAX_TOOL_OUTPUT_CHARS)));
        assert!(out.contains("[tool output truncated: 123 characters omitted]"));
    }
}
