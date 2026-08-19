use kitsu_providers::{ChatDelta, FinishReason, Usage};
use serde_json::{json, Value};

pub const OPENAI_CHAT_SPEC: &str =
    "https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create";
pub const ANTHROPIC_MESSAGES_SPEC: &str = "https://platform.claude.com/docs/en/api/messages/create";
pub const ANTHROPIC_STREAMING_SPEC: &str =
    "https://platform.claude.com/docs/en/build-with-claude/streaming";
pub const ANTHROPIC_ERRORS_SPEC: &str = "https://platform.claude.com/docs/en/api/errors";
pub const PRIMARY_SPEC_REVIEWED_UTC: &str = "2026-08-19";

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
