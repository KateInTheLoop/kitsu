use kitsu_providers::{ChatDelta, FinishReason, ToolCall, Usage};
use serde_json::{json, Value};

pub const OPENAI_CHAT_SPEC: &str =
    "https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create";
pub const ANTHROPIC_MESSAGES_SPEC: &str = "https://platform.claude.com/docs/en/api/messages/create";
pub const ANTHROPIC_STREAMING_SPEC: &str =
    "https://platform.claude.com/docs/en/build-with-claude/streaming";
pub const ANTHROPIC_ERRORS_SPEC: &str = "https://platform.claude.com/docs/en/api/errors";
pub const ANTHROPIC_TOOL_SPEC: &str =
    "https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls";
pub const PRIMARY_SPEC_REVIEWED_UTC: &str = "2026-08-19";

pub const KITSU_ERROR_BODY_LIMIT_BYTES: usize = 64 * 1024;
pub const KITSU_SSE_LINE_LIMIT_BYTES: usize = 4 * 1024 * 1024;
pub const KITSU_SSE_STREAM_LIMIT_BYTES: usize = 16 * 1024 * 1024;
pub const KITSU_TOOL_ARGUMENT_LIMIT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDialect {
    OpenAiChatCompletionsV1,
    AnthropicMessages2023_06_01,
}

#[derive(Clone, Debug)]
pub struct WireFixture {
    pub dialect: WireDialect,
    pub path: &'static str,
    pub version_header: Option<&'static str>,
    pub request: Value,
    pub stream: String,
    pub expected_deltas: Vec<ChatDelta>,
}

#[derive(Clone, Debug)]
pub struct HttpFaultFixture {
    pub path: &'static str,
    pub status: u16,
    pub body: String,
}

fn expected_text_deltas() -> Vec<ChatDelta> {
    vec![
        ChatDelta::Text("hello".to_string()),
        ChatDelta::Usage(Usage {
            prompt_tokens: 8,
            completion_tokens: 3,
            total_tokens: 11,
            prompt_tokens_details: None,
        }),
        ChatDelta::Done(FinishReason::Stop),
    ]
}

fn expected_tool_deltas() -> Vec<ChatDelta> {
    vec![
        ChatDelta::ToolCall(ToolCall {
            tool_name: "get_time".to_string(),
            args: r#"{"zone":"America/Los_Angeles"}"#.to_string(),
            call_id: "toolu_next".to_string(),
        }),
        ChatDelta::Usage(Usage {
            prompt_tokens: 20,
            completion_tokens: 7,
            total_tokens: 27,
            prompt_tokens_details: None,
        }),
        ChatDelta::Done(FinishReason::ToolCalls),
    ]
}

fn tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"zone": {"type": "string"}},
        "required": ["zone"],
        "additionalProperties": false
    })
}

pub fn openai_chat_text() -> WireFixture {
    let stream = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":8,"completion_tokens":3,"total_tokens":11}}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]
    .join("\n\n");

    WireFixture {
        dialect: WireDialect::OpenAiChatCompletionsV1,
        path: "/v1/chat/completions",
        version_header: None,
        request: json!({
            "model": "fixture-model-v1",
            "messages": [
                {"role": "system", "content": "You are concise."},
                {"role": "user", "content": "hello"}
            ],
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": 64
        }),
        stream: format!("{stream}\n\n"),
        expected_deltas: expected_text_deltas(),
    }
}

pub fn anthropic_messages_text() -> WireFixture {
    let stream = [
        concat!(
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"id":"msg_fixture","type":"message","role":"assistant","content":[],"model":"fixture-model-v1","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":8,"output_tokens":0}}}"#
        ),
        concat!(
            "event: content_block_start\n",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#
        ),
        concat!(
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#
        ),
        concat!(
            "event: content_block_stop\n",
            r#"data: {"type":"content_block_stop","index":0}"#
        ),
        concat!(
            "event: message_delta\n",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}"#
        ),
        concat!(
            "event: message_stop\n",
            r#"data: {"type":"message_stop"}"#
        ),
    ]
    .join("\n\n");

    WireFixture {
        dialect: WireDialect::AnthropicMessages2023_06_01,
        path: "/v1/messages",
        version_header: Some("2023-06-01"),
        request: json!({
            "model": "fixture-model-v1",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
            "system": "You are concise."
        }),
        stream: format!("{stream}\n\n"),
        expected_deltas: expected_text_deltas(),
    }
}

pub fn openai_chat_tool_round() -> WireFixture {
    let stream = [
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"toolu_next","type":"function","function":{"name":"get_time","arguments":"{\"zone\":"}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"America/Los_Angeles\"}"}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":7,"total_tokens":27}}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]
    .join("\n\n");

    WireFixture {
        dialect: WireDialect::OpenAiChatCompletionsV1,
        path: "/v1/chat/completions",
        version_header: None,
        request: json!({
            "model": "fixture-model-v1",
            "messages": [
                {"role": "system", "content": "You are concise."},
                {"role": "user", "content": "Check the time."},
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "toolu_previous",
                        "type": "function",
                        "function": {
                            "name": "get_time",
                            "arguments": r#"{"zone":"UTC"}"#
                        }
                    }]
                },
                {
                    "role": "tool",
                    "content": r#"{"time":"12:00"}"#,
                    "tool_call_id": "toolu_previous"
                }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_time",
                    "description": "Get the time for a zone.",
                    "parameters": tool_schema()
                }
            }],
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": 64
        }),
        stream: format!("{stream}\n\n"),
        expected_deltas: expected_tool_deltas(),
    }
}

pub fn anthropic_messages_tool_round() -> WireFixture {
    let stream = [
        concat!(
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"id":"msg_tool_fixture","type":"message","role":"assistant","content":[],"model":"fixture-model-v1","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":0}}}"#
        ),
        concat!(
            "event: content_block_start\n",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_next","name":"get_time","input":{}}}"#
        ),
        concat!(
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"zone\":"}}"#
        ),
        concat!(
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"America/Los_Angeles\"}"}}"#
        ),
        concat!(
            "event: content_block_stop\n",
            r#"data: {"type":"content_block_stop","index":0}"#
        ),
        concat!(
            "event: message_delta\n",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":7}}"#
        ),
        concat!(
            "event: message_stop\n",
            r#"data: {"type":"message_stop"}"#
        ),
    ]
    .join("\n\n");

    WireFixture {
        dialect: WireDialect::AnthropicMessages2023_06_01,
        path: "/v1/messages",
        version_header: Some("2023-06-01"),
        request: json!({
            "model": "fixture-model-v1",
            "max_tokens": 64,
            "messages": [
                {"role": "user", "content": "Check the time."},
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_previous",
                        "name": "get_time",
                        "input": {"zone": "UTC"}
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_previous",
                        "content": r#"{"time":"12:00"}"#
                    }]
                }
            ],
            "stream": true,
            "system": "You are concise.",
            "tools": [{
                "name": "get_time",
                "description": "Get the time for a zone.",
                "input_schema": tool_schema()
            }]
        }),
        stream: format!("{stream}\n\n"),
        expected_deltas: expected_tool_deltas(),
    }
}

pub fn openai_chat_rate_limit_error() -> HttpFaultFixture {
    HttpFaultFixture {
        path: "/v1/chat/completions",
        status: 429,
        body: json!({
            "error": {
                "message": "fixture rate limit",
                "type": "rate_limit_error",
                "code": "rate_limit_exceeded"
            }
        })
        .to_string(),
    }
}

pub fn anthropic_messages_overloaded_error() -> HttpFaultFixture {
    HttpFaultFixture {
        path: "/v1/messages",
        status: 529,
        body: json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": "fixture overloaded"
            },
            "request_id": "req_fixture"
        })
        .to_string(),
    }
}

pub fn openai_chat_malformed_event() -> WireFixture {
    let mut fixture = openai_chat_text();
    fixture.stream = "data: {\"choices\":[\n\n".to_string();
    fixture.expected_deltas.clear();
    fixture
}

pub fn anthropic_messages_malformed_event() -> WireFixture {
    let mut fixture = anthropic_messages_text();
    fixture.stream = "event: content_block_delta\ndata: {\"type\":\n\n".to_string();
    fixture.expected_deltas.clear();
    fixture
}

pub fn openai_chat_midstream_error() -> WireFixture {
    let mut fixture = openai_chat_text();
    fixture.stream = concat!(
        r#"data: {"error":{"message":"fixture stream error","type":"server_error"}}"#,
        "\n\n"
    )
    .to_string();
    fixture.expected_deltas.clear();
    fixture
}

pub fn anthropic_messages_midstream_error() -> WireFixture {
    let mut fixture = anthropic_messages_text();
    fixture.stream = concat!(
        "event: error\n",
        r#"data: {"type":"error","error":{"type":"overloaded_error","message":"fixture overloaded"},"request_id":"req_fixture"}"#,
        "\n\n"
    )
    .to_string();
    fixture.expected_deltas.clear();
    fixture
}

fn oversized_error(path: &'static str) -> HttpFaultFixture {
    HttpFaultFixture {
        path,
        status: 413,
        body: "x".repeat(KITSU_ERROR_BODY_LIMIT_BYTES + 1),
    }
}

pub fn openai_chat_oversized_error() -> HttpFaultFixture {
    oversized_error("/v1/chat/completions")
}

pub fn anthropic_messages_oversized_error() -> HttpFaultFixture {
    oversized_error("/v1/messages")
}

fn oversized_sse_line(mut fixture: WireFixture, event: Option<&str>) -> WireFixture {
    let oversized_data = format!("data: {}\n\n", "x".repeat(KITSU_SSE_LINE_LIMIT_BYTES));
    fixture.stream = match event {
        Some(event) => format!("event: {event}\n{oversized_data}"),
        None => oversized_data,
    };
    fixture.expected_deltas.clear();
    fixture
}

pub fn openai_chat_oversized_sse_line() -> WireFixture {
    oversized_sse_line(openai_chat_text(), None)
}

pub fn anthropic_messages_oversized_sse_line() -> WireFixture {
    oversized_sse_line(anthropic_messages_text(), Some("content_block_delta"))
}

/// Current compatible-Chat decoding is selected by the configured adapter,
/// not by the URL spelling. The comment-only body is also dialect-neutral
/// input for exercising the native Messages aggregate ceiling.
pub fn openai_chat_oversized_aggregate(path: &'static str) -> WireFixture {
    let mut fixture = openai_chat_text();
    fixture.path = path;
    let record = format!(": {}\n\n", "x".repeat(1024 * 1024));
    fixture.stream = record.repeat(17);
    fixture.expected_deltas.clear();
    fixture
}

/// Compatible-Chat tool arguments exceed their independent 1 MiB ceiling
/// while the containing SSE line stays below the 4 MiB line ceiling.
pub fn openai_chat_oversized_tool_arguments(path: &'static str) -> WireFixture {
    let mut fixture = openai_chat_tool_round();
    fixture.path = path;
    let chunk = json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "toolu_oversized",
                    "type": "function",
                    "function": {
                        "name": "get_time",
                        "arguments": "x".repeat(KITSU_TOOL_ARGUMENT_LIMIT_BYTES + 1),
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    fixture.stream = format!("data: {chunk}\n\n");
    fixture.expected_deltas.clear();
    fixture
}

/// The native decoder must reject this structurally distinct Messages
/// `input_json_delta` as `ToolArguments`, not as an SSE-line overflow and not
/// after emitting a partial neutral call.
pub fn anthropic_messages_oversized_tool_arguments() -> WireFixture {
    let mut fixture = anthropic_messages_tool_round();
    let chunk = json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {
            "type": "input_json_delta",
            "partial_json": "x".repeat(KITSU_TOOL_ARGUMENT_LIMIT_BYTES + 1),
        }
    });
    fixture.stream = format!("event: content_block_delta\ndata: {chunk}\n\n");
    fixture.expected_deltas.clear();
    fixture
}
