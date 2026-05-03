//! Tool definitions, permission resolution helpers, and tool execution.
//!
//! This module implements the tool-use subsystem for the agentic loop. The LLM
//! can invoke three built-in tools -- `read`, `write`, `bash` -- and every call
//! passes through a **three-layer permission resolution** pipeline:
//!
//! 1. **Tool existence check** -- unknown tool names produce a synthetic error
//!    so the LLM can recover without crashing the loop.
//! 2. **Session allow-rules** -- user-persisted rules (bare name or
//!    `bash(pattern*)` wildcards) that auto-approve matching calls.
//! 3. **Default tool policy** -- each tool has a built-in default: `read` is
//!    `Auto` (safe, read-only), while `write` and `bash` are
//!    `RequireApproval` because they mutate state.
//!
//! If none of the layers auto-approves, the loop pauses and the user decides
//! via the SSE-based approval flow (Allow / AllowAlways / Deny).
//!
//! Key types in this module:
//! - [`ToolPolicy`] -- whether a tool auto-executes or needs human approval.
//! - [`ToolDefinition`] -- schema + policy for a single tool.
//! - [`NormalizedToolCall`] -- protocol-agnostic representation of an LLM tool
//!   invocation, extracted from provider-specific JSON.
//! - [`PendingToolCall`] -- a tool call awaiting user approval, using a dual-ID
//!   scheme (`pending_call_id` vs `call_id`).
//! - [`ToolExecutionResult`] -- the outcome of running a tool, including
//!   synthetic error kinds for denied/unknown/cancelled calls.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::Protocol;

/// Maximum characters kept from a tool's stdout/stderr before truncation.
///
/// Tool output is persisted in the database as part of turn content. Without
/// truncation, a `bash` command could produce megabytes of output, bloating
/// the DB and slowing down subsequent message construction. 20k characters is
/// generous enough to capture meaningful output while keeping DB rows compact.
const MAX_TOOL_OUTPUT_CHARS: usize = 20_000;

/// Determines whether a tool call executes immediately or needs human approval.
///
/// - `Auto`: The tool is considered safe to run without user confirmation.
///   Currently only `read` uses this policy because file reads have no side
///   effects.
/// - `RequireApproval`: The tool can mutate filesystem state or execute
///   arbitrary code, so the user must explicitly approve each call (or create
///   a session-scoped allow-rule via AllowAlways).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    Auto,
    RequireApproval,
}

/// A tool definition stripped of its JSON schema, suitable for the public
/// `GET /api/config` endpoint. The frontend uses this to display policy
/// badges and decide which UI affordances to show per tool.
#[derive(Debug, Clone, Serialize)]
pub struct PublicTool {
    /// Stable tool identifier (e.g. `"read"`, `"write"`, `"bash"`).
    pub name: &'static str,
    /// Human-readable description shown in the UI tooltip.
    pub description: &'static str,
    /// Whether this tool auto-approves or requires user confirmation by default.
    pub default_policy: ToolPolicy,
}

/// Full definition of a built-in tool, including its JSON Schema.
///
/// Each definition is sent to the LLM as part of the tools array so the model
/// knows what tools exist and what arguments they accept. The `default_policy`
/// is NOT sent to the LLM -- it's used only by the server-side permission
/// resolution logic.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Stable tool identifier matching the `name` the LLM will use.
    pub name: &'static str,
    /// Description provided to the LLM in the tool definition so it understands
    /// when to use this tool.
    pub description: &'static str,
    /// Server-side default permission policy (never sent to the LLM).
    pub default_policy: ToolPolicy,
    /// JSON Schema object describing the tool's input parameters. This is
    /// included verbatim in the tools array sent to the provider API.
    pub input_schema: JsonValue,
}

/// Protocol-agnostic representation of a single tool invocation by the LLM.
///
/// Anthropic and OpenAI encode tool calls differently (Anthropic uses
/// `tool_use` content blocks, OpenAI uses `function_call` items), but the
/// agentic loop needs a single uniform type. [`extract_tool_calls`] handles
/// the provider-specific parsing and produces a `Vec<NormalizedToolCall>`.
#[derive(Debug, Clone)]
pub struct NormalizedToolCall {
    /// Provider-assigned ID for this tool call (e.g. `"toolu_01ABC..."` for
    /// Anthropic, `"call_0abc..."` for OpenAI). Used to correlate the tool
    /// result back to the correct call in the conversation history.
    pub call_id: String,
    /// Name of the tool the LLM requested (e.g. `"read"`, `"bash"`).
    pub name: String,
    /// Parsed JSON arguments from the LLM. Guaranteed to be a JSON object
    /// (the extraction functions default to `{}` if the model omits input).
    pub input: JsonValue,
}

/// A tool call that is paused pending user approval.
///
/// This type uses a **dual-ID scheme** to bridge two different identity
/// contexts:
///
/// - `pending_call_id`: A stable ID generated by our server (a UUID). The
///   frontend uses this ID when submitting approval decisions via
///   `POST /approve`. It is guaranteed unique and never changes.
/// - `call_id`: The provider's original ID for the tool call. This must be
///   preserved verbatim so that the tool result can be correctly correlated
///   when sent back to the LLM API.
///
/// Both IDs are needed because the approval flow is asynchronous -- the loop
/// pauses, the user decides, and when the loop resumes it needs the provider's
/// original `call_id` to construct a valid follow-up message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingToolCall {
    /// Server-generated stable ID used by the frontend to reference this
    /// pending call in approval/denial requests.
    pub pending_call_id: String,
    /// Provider-assigned ID preserved for round-trip fidelity when sending
    /// the tool result back to the LLM API.
    pub call_id: String,
    /// Tool name (e.g. `"bash"`).
    pub name: String,
    /// Parsed tool arguments.
    pub input: JsonValue,
}

/// The result of executing (or failing to execute) a single tool call.
///
/// Not all results come from actual tool execution. The permission system
/// produces **synthetic errors** for cases where the tool was never run:
///
/// - `"unknown_tool"`: The LLM hallucinated a tool name that doesn't exist.
/// - `"denied"`: The user explicitly denied the tool call via the approval UI.
/// - `"cancelled"`: The turn was cancelled while the tool was executing.
///
/// These synthetic errors are sent back to the LLM as tool results with
/// `is_error = true`, allowing the model to see the failure and try a
/// different approach within the same turn. This is critical for good agentic
/// behavior -- denial is not terminal.
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    /// The provider's tool call ID, used to correlate this result with the
    /// original request in the conversation history.
    pub call_id: String,
    /// The tool's stdout/stderr text, or an error message for synthetic errors.
    /// Always truncated to [`MAX_TOOL_OUTPUT_CHARS`] to prevent oversized DB
    /// writes.
    pub output: String,
    /// `true` if this result represents an error (either a real execution
    /// failure or a synthetic permission/cancellation error). The LLM API
    /// expects this flag so it can present the result differently.
    pub is_error: bool,
    /// Machine-readable error category for synthetic errors. Values include
    /// `"unknown_tool"`, `"denied"`, `"cancelled"`, `"io_error"`,
    /// `"non_zero_exit"`, `"timeout"`, `"spawn_error"`, `"invalid_arguments"`.
    /// `None` for successful results.
    pub error_kind: Option<String>,
}

/// Truncates tool output to [`MAX_TOOL_OUTPUT_CHARS`] and appends an
/// informational suffix when characters were dropped.
///
/// Uses char-based counting (not byte-based) so we don't cut in the middle of
/// a multi-byte UTF-8 codepoint. This matters because bash output and file
/// contents can contain non-ASCII text.
///
/// The truncation suffix tells the LLM that output was cut off, so it can
/// decide whether to request the full output via a different method (e.g.
/// reading specific line ranges with bash `head`/`tail`).
fn truncate_tool_output(text: &str) -> String {
    // Take up to MAX_TOOL_OUTPUT_CHARS Unicode characters (not bytes).
    let mut iter = text.chars();
    let kept: String = iter.by_ref().take(MAX_TOOL_OUTPUT_CHARS).collect();

    // Count how many chars were beyond the limit, if any.
    let omitted = iter.count();
    if omitted == 0 {
        // Output fits within the limit -- return it unchanged.
        return text.to_string();
    }

    // Append a notice so the LLM knows output was truncated. The LLM can use
    // this information to decide whether it needs the full output.
    format!("{kept}\n\n[tool output truncated: {omitted} characters omitted]")
}

/// Returns the full definitions for all built-in tools.
///
/// These definitions serve two purposes:
/// 1. The `input_schema` for each tool is sent to the LLM as part of the
///    tools array so the model knows what parameters to provide.
/// 2. The `default_policy` is used server-side during permission resolution
///    (Layer 3) when no session allow-rule matches.
///
/// Tool policies:
/// - `read` is `Auto` because reading files has no side effects -- it's safe
///   to let the LLM read any file without prompting the user.
/// - `write` is `RequireApproval` because it can overwrite or create files,
///   which is potentially destructive.
/// - `bash` is `RequireApproval` because arbitrary shell commands can do
///   anything (delete files, install packages, access the network, etc.).
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read",
            description: "Read a UTF-8 text file from disk by path.",
            // Safe: reading files has no side effects.
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
                // Reject extra fields so the LLM can't sneak in parameters
                // we don't handle.
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "write",
            description: "Write UTF-8 text content to a file path.",
            // Dangerous: can create or overwrite files on disk.
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
            // Dangerous: arbitrary shell execution -- the LLM could run
            // anything, including destructive commands.
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
                        // Cap at 10 minutes to prevent runaway processes.
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

/// Returns tool metadata without JSON schemas, for the public config API.
///
/// The frontend doesn't need the full input schemas -- it only needs to know
/// which tools exist and their default policies so it can render appropriate
/// UI hints (e.g. "this tool requires approval").
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

/// Looks up the default policy for a named tool.
///
/// Returns `None` for unknown tool names. The caller (turn lifecycle) uses
/// this as Layer 3 of the permission resolution: if `None`, the tool doesn't
/// exist and a synthetic `"unknown_tool"` error is produced instead.
pub fn default_policy(tool_name: &str) -> Option<ToolPolicy> {
    tool_definitions()
        .into_iter()
        .find(|tool| tool.name == tool_name)
        .map(|tool| tool.default_policy)
}

/// Extracts tool calls from an LLM assistant message, dispatching to the
/// correct parser based on the provider protocol.
///
/// Anthropic and OpenAI represent tool calls in fundamentally different shapes:
///
/// - **Anthropic**: The assistant's `content` is an array of typed blocks.
///   Tool calls appear as `{ "type": "tool_use", "id": "...", "name": "...",
///   "input": { ... } }` blocks interspersed with text blocks.
///
/// - **OpenAI**: The assistant's content is an array of items where tool calls
///   appear as `{ "type": "function_call", "call_id": "...", "name": "...",
///   "arguments": "<JSON string>" }` items. Notably, OpenAI sends arguments
///   as a **JSON string** that must be parsed, rather than a native JSON
///   object.
///
/// Both paths produce the same [`NormalizedToolCall`] output so the rest of
/// the system is protocol-agnostic.
pub fn extract_tool_calls(
    protocol: Protocol,
    assistant_content: &JsonValue,
) -> Vec<NormalizedToolCall> {
    match protocol {
        Protocol::Anthropic => extract_anthropic_tool_calls(assistant_content),
        Protocol::Openai => extract_openai_tool_calls(assistant_content),
    }
}

/// Parses tool calls from an Anthropic-format assistant message.
///
/// Anthropic's content blocks are a mixed array: text blocks (`"type":
/// "text"`) and tool-use blocks (`"type": "tool_use"`). We filter for
/// `tool_use` blocks and extract the three required fields.
///
/// Malformed blocks (missing `name`, `id`, or `input`) are silently skipped
/// rather than causing a panic, because LLM output is not fully trustworthy.
fn extract_anthropic_tool_calls(assistant_content: &JsonValue) -> Vec<NormalizedToolCall> {
    let mut out = Vec::new();

    // Anthropic content is always an array of blocks.
    let Some(blocks) = assistant_content.as_array() else {
        return out;
    };

    for block in blocks {
        // Only process tool_use blocks; skip text and other block types.
        let is_tool_use = block.get("type").and_then(|v| v.as_str()) == Some("tool_use");
        if !is_tool_use {
            continue;
        }

        // Extract tool name -- required for a valid tool call.
        let Some(name) = block.get("name").and_then(|v| v.as_str()) else {
            continue;
        };

        // Extract the provider-assigned call ID (e.g. "toolu_01ABC...").
        let Some(call_id) = block.get("id").and_then(|v| v.as_str()) else {
            continue;
        };

        // Anthropic provides input as a native JSON object.
        // Default to empty object if the model omits it (rare but possible).
        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));

        out.push(NormalizedToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            input,
        });
    }

    out
}

/// Parses tool calls from an OpenAI-format assistant message.
///
/// OpenAI's Responses API represents tool calls as items with `"type":
/// "function_call"`. Key differences from Anthropic:
///
/// - Arguments are a **JSON string** (`"arguments": "{\"path\": \"/foo\"}"`)
///   rather than a native JSON object. We parse this string, falling back to
///   a `{"raw": ...}` wrapper if parsing fails (so we don't lose the data).
/// - The call ID may be in `"call_id"` or `"id"` depending on the API version.
///   We try both fields for compatibility.
fn extract_openai_tool_calls(assistant_content: &JsonValue) -> Vec<NormalizedToolCall> {
    let mut out = Vec::new();

    // OpenAI content is an array of items (text, function_call, etc.).
    let Some(items) = assistant_content.as_array() else {
        return out;
    };

    for item in items {
        // Only process function_call items; skip text and other types.
        let is_function_call = item.get("type").and_then(|v| v.as_str()) == Some("function_call");
        if !is_function_call {
            continue;
        }

        // Extract tool name -- required for a valid function call.
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };

        // OpenAI puts the ID in "call_id" (Responses API) or "id" (older
        // Chat Completions format). Try both for forward/backward compat.
        let call_id = item
            .get("call_id")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("id").and_then(|v| v.as_str()));
        let Some(call_id) = call_id else {
            continue;
        };

        // OpenAI sends arguments as a JSON string that must be parsed.
        // If it's already a JSON value (unlikely but defensive), use as-is.
        // If parsing the string fails, wrap the raw string so we don't
        // silently lose the LLM's intent.
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

/// Wraps tool execution results into a protocol-native "user" message that
/// carries the tool outputs back to the LLM.
///
/// This is critical for **round-trip fidelity**: the tool result message must
/// use the exact JSON shape that the provider expects, otherwise the API will
/// reject it. Anthropic and OpenAI have different formats:
///
/// - **Anthropic**: Each result is a `tool_result` block with `tool_use_id`
///   (matching the original `tool_use` block's `id`), plus optional
///   `is_error` and `error` fields.
/// - **OpenAI**: Each result is a `function_call_output` block with `call_id`
///   (matching the original `function_call`'s `call_id`), plus optional
///   `is_error` and `error` fields.
///
/// The resulting message always has `"role": "user"` because both APIs treat
/// tool results as user-turn content.
pub fn tool_result_entry(protocol: Protocol, results: &[ToolExecutionResult]) -> JsonValue {
    let content = match protocol {
        Protocol::Anthropic => {
            // Anthropic format: array of tool_result blocks.
            let blocks: Vec<JsonValue> = results
                .iter()
                .map(|r| {
                    let mut block = json!({
                        "type": "tool_result",
                        // Correlate with the original tool_use block by ID.
                        "tool_use_id": r.call_id,
                        "content": r.output,
                    });
                    // Synthetic and real errors both carry is_error so the
                    // Anthropic API treats them as failures.
                    if r.is_error {
                        block["is_error"] = json!(true);
                        // Include the error kind so the LLM knows *why* the
                        // tool failed (denied, unknown, cancelled, etc.).
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
            // OpenAI Responses API format: array of function_call_output items.
            let blocks: Vec<JsonValue> = results
                .iter()
                .map(|r| {
                    let mut block = json!({
                        "type": "function_call_output",
                        // Correlate with the original function_call by ID.
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

    // Both protocols wrap tool results in a "user" role message.
    json!({
        "role": "user",
        "content": content,
    })
}

/// Tests whether a session allow-rule matches a tool call.
///
/// This is Layer 2 of the three-layer permission resolution. Rules come in
/// three forms:
///
/// 1. **Exact tool name** (e.g. `"read"`): Matches any call to that tool
///    regardless of arguments. This is the simplest and most common rule.
///
/// 2. **Parameterized bash rule** (e.g. `"bash(cargo check *)"`): Only
///    applicable to `bash` calls. The pattern inside the parentheses is
///    matched against the command string using [`wildcard_match`], which
///    supports `*` wildcards for prefix/suffix/infix matching.
///
/// 3. **Non-bash tool with bash-style rule** (e.g. `"read(something)"`):
///    Never matches -- parameterized rules only work for `bash` because
///    it's the only tool where argument-level granularity matters.
pub fn match_allow_rule(rule: &str, tool_name: &str, input: &JsonValue) -> bool {
    // Mode 1: Exact tool name match. "read" matches any read call.
    if rule == tool_name {
        return true;
    }

    // Parameterized rules only apply to bash -- other tools don't need
    // argument-level granularity.
    if tool_name != "bash" {
        return false;
    }

    // Mode 2: Parameterized bash rule. Must be in the form "bash(pattern)".
    let (prefix, suffix) = ("bash(", ")");
    if !rule.starts_with(prefix) || !rule.ends_with(suffix) {
        return false;
    }

    // Extract the wildcard pattern from between "bash(" and ")".
    let pattern = &rule[prefix.len()..rule.len() - suffix.len()];

    // Get the command string from the tool input for matching.
    let cmd = input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    wildcard_match(pattern, cmd)
}

/// Derives an allow-rule string from a tool call for the "AllowAlways" decision.
///
/// When the user clicks "Always allow this tool", we need to persist a rule
/// that will match the same call in the future. The rule format depends on
/// the tool:
///
/// - For `bash`: `"bash(<exact command>)"` -- captures the full command so
///   that an AllowAlways for `cargo check` only allows `cargo check`, not
///   `cargo check && rm -rf /`. The user can edit the rule in preferences
///   to add wildcards if desired.
/// - For other tools: bare tool name (e.g. `"write"`) since those tools
///   don't need argument-level granularity.
pub fn derive_allow_rule(tool_name: &str, input: &JsonValue) -> String {
    if tool_name == "bash" {
        // Capture the exact command as the rule pattern.
        // e.g. AllowAlways for "cargo check --workspace" becomes
        // "bash(cargo check --workspace)".
        let cmd = input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        return format!("bash({cmd})");
    }

    // Non-bash tools: the bare name is sufficient as a rule.
    tool_name.to_string()
}

/// Matches a wildcard pattern against a text string.
///
/// Supports a single `*` wildcard that can appear at the start, end, or
/// middle of the pattern:
///
/// - `"cargo check *"` -- prefix match: text must start with "cargo check "
///   and the `*` matches anything (or nothing) after it.
/// - `"* --help"` -- suffix match: text must end with " --help".
/// - `"cargo * check"` -- infix match: text must start with "cargo ", then
///   anything, then end with " check".
/// - `"*"` -- matches everything (universal allow).
/// - `"exact"` -- no wildcard, requires exact equality.
///
/// The algorithm splits on `*` and then walks the parts against the text:
/// - First part (if pattern doesn't start with `*`): must be a prefix.
/// - Last part (if pattern doesn't end with `*`): must be a suffix.
/// - Middle parts: found via linear search (first occurrence).
///
/// This is intentionally simple -- no regex, no glob `**`, no character
/// classes. The goal is to be understandable by users who edit rules.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    // Fast path: "*" matches everything.
    if pattern == "*" {
        return true;
    }

    // Split on the wildcard to get literal segments.
    // e.g. "cargo check *" -> ["cargo check ", ""]
    // e.g. "cargo * check" -> ["cargo ", " check"]
    let parts: Vec<&str> = pattern.split('*').collect();

    // No wildcard means exact match.
    if parts.len() == 1 {
        return pattern == text;
    }

    // Walk through the text, consuming it segment by segment.
    let mut idx = 0usize;
    for (i, part) in parts.iter().enumerate() {
        // Empty segments come from leading/trailing/consecutive wildcards.
        if part.is_empty() {
            continue;
        }

        // First segment and pattern doesn't start with '*': must be a prefix.
        // e.g. "cargo check *" -> first part "cargo check " must start the text.
        if i == 0 && !pattern.starts_with('*') {
            if !text[idx..].starts_with(part) {
                return false;
            }
            idx += part.len();
            continue;
        }

        // Last segment and pattern doesn't end with '*': must be a suffix.
        // e.g. "* --help" -> last part " --help" must end the text.
        if i == parts.len() - 1 && !pattern.ends_with('*') {
            return text[idx..].ends_with(part);
        }

        // Middle segment (or segment after leading wildcard): find first
        // occurrence anywhere in the remaining text.
        // e.g. "cargo * check" -> " check" must appear somewhere after
        // "cargo " was consumed.
        let Some(found) = text[idx..].find(part) else {
            return false;
        };
        idx += found + part.len();
    }

    // Final check: if the pattern doesn't end with '*', the last non-empty
    // segment must be a true suffix. This handles edge cases where the loop
    // above ended on an empty trailing segment.
    if !pattern.ends_with('*')
        && let Some(last) = parts.last()
        && !last.is_empty()
    {
        return text.ends_with(last);
    }

    true
}

/// Executes a single tool call, handling both success and error paths.
///
/// The dispatch is straightforward: match on the tool name and call the
/// appropriate executor. Unknown tool names produce an error immediately --
/// this is the last line of defense for hallucinated tool names (Layer 1 of
/// the permission system should have caught them earlier, but defense-in-depth
/// is important).
///
/// All errors (both execution failures and synthetic errors) are captured as
/// `(error_kind, message)` tuples and wrapped in a [`ToolExecutionResult`]
/// with `is_error = true`. This ensures the LLM always gets a well-formed
/// result it can reason about, even for denied or cancelled calls.
///
/// The `cancel_token` is threaded through to each executor so that in-flight
/// operations (file I/O, running processes) can be cooperatively cancelled
/// when the user cancels a turn.
pub async fn execute_tool_call(
    call: &NormalizedToolCall,
    cancel_token: &CancellationToken,
) -> ToolExecutionResult {
    // Dispatch to the appropriate tool executor.
    let res = match call.name.as_str() {
        "read" => exec_read(&call.input, cancel_token).await,
        "write" => exec_write(&call.input, cancel_token).await,
        "bash" => exec_bash(&call.input, cancel_token).await,
        // Unknown tool -- should have been caught by Layer 1 (tool existence
        // check), but we handle it here as defense-in-depth.
        _ => Err((
            "unknown_tool".to_string(),
            format!("Unknown tool '{}'", call.name),
        )),
    };

    // Unify success and error into a ToolExecutionResult.
    match res {
        Ok(output) => ToolExecutionResult {
            call_id: call.call_id.clone(),
            output: truncate_tool_output(&output),
            is_error: false,
            error_kind: None,
        },
        Err((kind, message)) => ToolExecutionResult {
            call_id: call.call_id.clone(),
            // Error messages are also truncated to prevent oversized DB writes
            // in case the LLM provides a huge arguments string.
            output: truncate_tool_output(&message),
            is_error: true,
            error_kind: Some(kind),
        },
    }
}

/// Reads a UTF-8 text file from disk.
///
/// Uses `tokio::select!` to race the file read against cancellation. If the
/// user cancels the turn while we're waiting on disk I/O, the cancellation
/// branch wins and we return a synthetic `"cancelled"` error immediately.
/// The file read is not aborted (OS-level I/O can't be cancelled), but we
/// stop waiting for it.
async fn exec_read(
    input: &JsonValue,
    cancel_token: &CancellationToken,
) -> Result<String, (String, String)> {
    // Validate required "path" argument.
    let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "read requires string field 'path'".to_string(),
        ));
    };

    // Race the file read against cooperative cancellation.
    tokio::select! {
        // Cancellation branch: user cancelled the turn.
        _ = cancel_token.cancelled() => Err((
            "cancelled".to_string(),
            "read cancelled by user".to_string(),
        )),
        // File read branch: read the entire file as UTF-8.
        result = tokio::fs::read_to_string(path) => result.map_err(|e| ("io_error".to_string(), format!("read failed: {e}"))),
    }
}

/// Writes UTF-8 text content to a file on disk.
///
/// Like `exec_read`, uses `tokio::select!` for cooperative cancellation.
/// The write is not atomic -- if cancelled mid-write, the file may be
/// partially written. This is acceptable for the current use case (agent
/// tool calls) since the user explicitly approved the write.
async fn exec_write(
    input: &JsonValue,
    cancel_token: &CancellationToken,
) -> Result<String, (String, String)> {
    // Validate required "path" argument.
    let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "write requires string field 'path'".to_string(),
        ));
    };

    // Validate required "content" argument.
    let Some(content) = input.get("content").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "write requires string field 'content'".to_string(),
        ));
    };

    // Race the file write against cooperative cancellation.
    tokio::select! {
        _ = cancel_token.cancelled() => Err((
            "cancelled".to_string(),
            "write cancelled by user".to_string(),
        )),
        result = tokio::fs::write(path, content) => result.map_err(|e| ("io_error".to_string(), format!("write failed: {e}"))),
    }?;

    // Return a simple "ok" on success -- the LLM doesn't need more detail.
    Ok("ok".to_string())
}

/// Executes a shell command via `bash -lc`.
///
/// This is the most dangerous and complex tool. Key design decisions:
///
/// - **`kill_on_drop(true)`**: When the `tokio::process::Command` future is
///   dropped (e.g. due to cancellation or timeout), the spawned child process
///   is immediately killed via SIGKILL. Without this, the process would
///   continue running in the background as an orphan, potentially consuming
///   resources indefinitely.
///
/// - **Timeout**: Commands run with a configurable timeout (default 60s, max
///   600s). The timeout wraps the entire `cmd.output()` future, so if the
///   process doesn't exit within the limit, we get a `timeout` error and the
///   child is killed via `kill_on_drop`.
///
/// - **`bash -lc`**: Uses a login shell (`-l`) so the command inherits the
///   user's PATH and other environment variables. The `-c` flag passes the
///   command string directly.
///
/// - **stdout + stderr merging**: Both streams are captured and concatenated
///   (stdout first, then stderr separated by a newline). This gives the LLM
///   the full picture of what happened, including error output.
///
/// - **Cooperative cancellation**: Uses `tokio::select!` to race the command
///   execution against the turn's cancellation token. When cancelled, the
///   `kill_on_drop` ensures the child process is terminated.
async fn exec_bash(
    input: &JsonValue,
    cancel_token: &CancellationToken,
) -> Result<String, (String, String)> {
    // Validate required "command" argument.
    let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
        return Err((
            "invalid_arguments".to_string(),
            "bash requires string field 'command'".to_string(),
        ));
    };

    // Optional timeout with a 60-second default. The LLM can request up to
    // 600 seconds (10 minutes) for long-running builds or test suites.
    let timeout_sec = input
        .get("timeout_sec")
        .and_then(|v| v.as_u64())
        .unwrap_or(60);

    // Optional working directory override.
    let cwd = input.get("cwd").and_then(|v| v.as_str());

    // Build the bash command. "-l" gives us a login shell (PATH, env vars),
    // "-c" passes the command string directly.
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command);

    // Critical: kill the child process when the Command handle is dropped.
    // Without this, cancellation would stop waiting for output but the
    // process would keep running as an orphan.
    cmd.kill_on_drop(true);

    // Apply optional working directory if provided.
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    // Wrap the command execution in a timeout. This creates a nested future
    // that resolves with an error if the process doesn't finish in time.
    let run = async {
        tokio::time::timeout(Duration::from_secs(timeout_sec), cmd.output())
            .await
            .map_err(|_| {
                // Timeout elapsed -- process will be killed by kill_on_drop
                // when the Command future is dropped.
                (
                    "timeout".to_string(),
                    format!("bash timed out after {timeout_sec}s"),
                )
            })?
            .map_err(|e| {
                // Process failed to start (e.g. bash not found, permission
                // denied). This is distinct from a non-zero exit code.
                (
                    "spawn_error".to_string(),
                    format!("bash failed to start: {e}"),
                )
            })
    };

    // Race the command execution against cooperative cancellation.
    let output = tokio::select! {
        // Cancellation branch: user cancelled the turn.
        _ = cancel_token.cancelled() => Err((
            "cancelled".to_string(),
            "bash cancelled by user".to_string(),
        )),
        // Execution branch: run the command with timeout.
        result = run => result,
    }?;

    // Merge stdout and stderr into a single string for the LLM.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut text = String::new();

    // stdout first (typically the useful output).
    if !stdout.is_empty() {
        text.push_str(&stdout);
    }

    // stderr after a newline separator (if both streams produced output).
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }

    // Distinguish between success and failure by the process exit code.
    if output.status.success() {
        Ok(text)
    } else {
        // Non-zero exit code -- return as an error so the LLM knows the
        // command failed. Include the exit code and trimmed output so the
        // model can diagnose the issue.
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
        // Prefix match: "cargo check *" matches commands starting with "cargo check ".
        assert!(wildcard_match("cargo check *", "cargo check --workspace"));
        // Suffix match: "* --help" matches commands ending with " --help".
        assert!(wildcard_match("* --help", "cargo check --help"));
        // Non-matching: "cargo check *" does not match "cargo build".
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
