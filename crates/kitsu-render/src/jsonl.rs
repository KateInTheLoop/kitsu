//! Fallible, serialized JSON Lines output.
//!
//! A record is completely serialized in memory before any bytes are written,
//! then written with exactly one trailing line feed and flushed. Once an emit
//! fails the emitter is terminal: continuing after a partial write could make
//! a corrupt stream look trustworthy to an automation consumer.

use std::fmt;
use std::io::{self, Write};

use serde::Serialize;

/// The stage at which a JSONL emission became terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFailureStage {
    Serialize,
    Write,
    Flush,
}

impl fmt::Display for JsonlFailureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Serialize => "serialize",
            Self::Write => "write",
            Self::Flush => "flush",
        })
    }
}

/// A durable summary of an output failure.
///
/// `std::io::Error` is not cloneable, so the emitter retains the error kind and
/// message. Every later call returns the same summary without touching the
/// writer again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlEmitError {
    stage: JsonlFailureStage,
    io_kind: Option<io::ErrorKind>,
    message: String,
}

impl JsonlEmitError {
    fn serialization(error: serde_json::Error) -> Self {
        Self {
            stage: JsonlFailureStage::Serialize,
            io_kind: None,
            message: error.to_string(),
        }
    }

    fn io(stage: JsonlFailureStage, error: io::Error) -> Self {
        Self {
            stage,
            io_kind: Some(error.kind()),
            message: error.to_string(),
        }
    }

    pub fn stage(&self) -> JsonlFailureStage {
        self.stage
    }

    /// Preserves `BrokenPipe` and other actionable OS-independent I/O kinds.
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        self.io_kind
    }
}

impl fmt::Display for JsonlEmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSONL {} failed: {}", self.stage, self.message)
    }
}

impl std::error::Error for JsonlEmitError {}

/// A single-writer JSONL stream with a terminal failure state.
pub struct JsonlEmitter<W> {
    writer: W,
    schema: String,
    run_id: String,
    failure: Option<JsonlEmitError>,
}

impl<W: Write> JsonlEmitter<W> {
    pub fn new(writer: W, schema: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            writer,
            schema: schema.into(),
            run_id: run_id.into(),
            failure: None,
        }
    }

    /// Serialize, write, and flush one complete event.
    ///
    /// Object payloads are promoted into the envelope. Scalar/array payloads
    /// are stored under `value`. Reserved envelope keys always win.
    pub fn emit<T: Serialize>(&mut self, kind: &str, payload: T) -> Result<(), JsonlEmitError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }

        let value = serde_json::to_value(payload)
            .map_err(JsonlEmitError::serialization)
            .map_err(|error| self.fail(error))?;
        let mut object = match value {
            serde_json::Value::Object(object) => object,
            value => {
                let mut object = serde_json::Map::new();
                object.insert("value".to_string(), value);
                object
            }
        };
        object.insert(
            "schema".to_string(),
            serde_json::Value::String(self.schema.clone()),
        );
        object.insert(
            "type".to_string(),
            serde_json::Value::String(kind.to_string()),
        );
        object.insert(
            "run_id".to_string(),
            serde_json::Value::String(self.run_id.clone()),
        );

        let mut record = Vec::new();
        serde_json::to_writer(&mut record, &object)
            .map_err(JsonlEmitError::serialization)
            .map_err(|error| self.fail(error))?;
        record.push(b'\n');

        self.writer
            .write_all(&record)
            .map_err(|error| JsonlEmitError::io(JsonlFailureStage::Write, error))
            .map_err(|error| self.fail(error))?;
        self.writer
            .flush()
            .map_err(|error| JsonlEmitError::io(JsonlFailureStage::Flush, error))
            .map_err(|error| self.fail(error))?;
        Ok(())
    }

    pub fn failure(&self) -> Option<&JsonlEmitError> {
        self.failure.as_ref()
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn fail(&mut self, error: JsonlEmitError) -> JsonlEmitError {
        self.failure = Some(error.clone());
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::Error as _;

    #[derive(Default)]
    struct ShortWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let count = input.len().min(3);
            self.bytes.extend_from_slice(&input[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn emits_exactly_one_parseable_line_and_flushes() {
        let mut emitter = JsonlEmitter::new(ShortWriter::default(), "kitsu.chat.v1", "run-7");
        emitter
            .emit(
                "assistant",
                serde_json::json!({
                    "text": "first\nsecond",
                    "schema": "untrusted",
                    "type": "untrusted",
                }),
            )
            .unwrap();

        let writer = emitter.into_inner();
        assert_eq!(writer.flushes, 1);
        assert_eq!(writer.bytes.last(), Some(&b'\n'));
        assert_eq!(
            writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        let event: serde_json::Value = serde_json::from_slice(&writer.bytes).unwrap();
        assert_eq!(event["schema"], "kitsu.chat.v1");
        assert_eq!(event["type"], "assistant");
        assert_eq!(event["run_id"], "run-7");
        assert_eq!(event["text"], "first\nsecond");
    }

    struct BrokenPipeWriter {
        writes: usize,
        flushes: usize,
    }

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _input: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "consumer closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_is_propagated_and_makes_the_emitter_terminal() {
        let writer = BrokenPipeWriter {
            writes: 0,
            flushes: 0,
        };
        let mut emitter = JsonlEmitter::new(writer, "schema", "run");

        let first = emitter.emit("one", serde_json::json!({})).unwrap_err();
        let second = emitter.emit("two", serde_json::json!({})).unwrap_err();
        assert_eq!(first.stage(), JsonlFailureStage::Write);
        assert_eq!(first.io_kind(), Some(io::ErrorKind::BrokenPipe));
        assert_eq!(second, first);
        let writer = emitter.into_inner();
        assert_eq!(writer.writes, 1, "a terminal emitter must not write again");
        assert_eq!(writer.flushes, 0, "a failed write must not be flushed");
    }

    struct FlushFailureWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for FlushFailureWriter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Err(io::Error::new(io::ErrorKind::WriteZero, "flush failed"))
        }
    }

    #[test]
    fn flush_failure_is_propagated_and_terminal() {
        let writer = FlushFailureWriter {
            bytes: Vec::new(),
            flushes: 0,
        };
        let mut emitter = JsonlEmitter::new(writer, "schema", "run");
        let error = emitter.emit("one", serde_json::json!({})).unwrap_err();
        assert_eq!(error.stage(), JsonlFailureStage::Flush);
        assert_eq!(error.io_kind(), Some(io::ErrorKind::WriteZero));
        assert_eq!(
            emitter.emit("two", serde_json::json!({})).unwrap_err(),
            error
        );
        let writer = emitter.into_inner();
        assert_eq!(writer.flushes, 1);
        assert_eq!(
            writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
    }

    struct SerializationFailure;

    impl Serialize for SerializationFailure {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(S::Error::custom("deliberate serialization failure"))
        }
    }

    #[test]
    fn serialization_failure_writes_nothing_and_is_terminal() {
        let mut emitter = JsonlEmitter::new(Vec::<u8>::new(), "schema", "run");
        let error = emitter.emit("bad", SerializationFailure).unwrap_err();
        assert_eq!(error.stage(), JsonlFailureStage::Serialize);
        assert_eq!(error.io_kind(), None);
        assert!(emitter.emit("later", serde_json::json!({})).is_err());
        assert!(emitter.into_inner().is_empty());
    }

    #[test]
    fn published_schema_keeps_command_payload_and_event_definitions_distinct() {
        let source = include_str!("../../../docs/schemas/kitsu-chat-v1.schema.json");
        let schema: serde_json::Value = serde_json::from_str(source).unwrap();
        let defs = schema["$defs"].as_object().expect("schema definitions");

        assert!(defs["command_execution_result"].is_object());
        assert!(defs["command_result"]["allOf"].is_array());
        assert_eq!(
            defs["run_started"]["allOf"][1]["properties"]["provider_route"]["$ref"],
            "#/$defs/provider_route_receipt"
        );
        assert_eq!(
            defs["provider_route_receipt"]["properties"]["receipt_revision"]["const"],
            1
        );
        assert_eq!(
            defs["provider_route_identity"]["properties"]["generation"]["minimum"],
            1
        );
        assert_eq!(
            defs["model_catalog_identity"]["properties"]["operation_id"]["pattern"],
            "^models:[1-9][0-9]*$"
        );
        assert!(
            defs["provider_request"]["allOf"][2]["properties"]["purpose"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|purpose| purpose == "model_catalog")
        );
        assert_eq!(
            defs["provider_request"]["allOf"][2]["properties"]["model"]["type"],
            "string"
        );
        assert!(
            defs["provider_attempt"]["allOf"][2]["properties"]["purpose"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|purpose| purpose == "model_catalog")
        );
        assert_eq!(
            defs["review_disclosure"]["allOf"][1]["properties"]["source"]["const"],
            "top_level_review"
        );
        assert_eq!(
            defs["output_last_message"]["allOf"][2]["properties"]["assistant_committed"]["const"],
            true
        );
        assert_eq!(
            defs["output_last_message"]["allOf"][2]["properties"]["model_write_authority"]["const"],
            false
        );
        assert_eq!(
            defs["structured_finalization_deferred"]["allOf"][2]["properties"]["cause"]["const"],
            "required_write_sha256_mismatch"
        );
        assert_eq!(
            defs["structured_finalization_deferred"]["allOf"][2]["properties"]["targets"]["items"]
                ["properties"]["actual_sha256"]["pattern"],
            "^[0-9a-f]{64}$"
        );
        assert_eq!(
            defs["structured_finalization_deferred"]["allOf"][2]["properties"]
                ["write_authority_granted"]["const"],
            false
        );
        assert_eq!(
            defs["error"]["allOf"][1]["properties"]["diagnostic_code"]["enum"][0],
            "local_ollama_unreachable"
        );
        assert_eq!(
            defs["error"]["allOf"][1]["dependentRequired"]["diagnostic_code"][0],
            "recovery_actions"
        );
        assert_eq!(
            defs["error"]["allOf"][1]["properties"]["recovery_actions"]["items"]["enum"],
            serde_json::json!([
                "start_ollama_serve",
                "verify_ollama_serve",
                "select_another_gateway"
            ])
        );
        assert_eq!(
            defs["error"]["allOf"][1]["properties"]["schema_sha256"]["pattern"],
            "^[0-9a-f]{64}$"
        );
        assert_eq!(
            defs["final_summary"]["allOf"][1]["properties"]["output_schema_failures"]["minimum"],
            0
        );
        assert!(!defs["final_summary"]["allOf"][1]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "output_schema_failures"));
        assert_eq!(
            defs["tool_finished"]["allOf"][2]["properties"]["command"]["$ref"],
            "#/$defs/command_execution_result"
        );
        assert_eq!(
            source.matches("    \"command_result\": {").count(),
            1,
            "duplicate JSON object keys silently replace an earlier schema definition"
        );

        let summary = &defs["final_summary"]["allOf"][1];
        assert_eq!(
            summary["properties"]["session_snapshot_state"]["enum"],
            serde_json::json!(["disabled", "pending_until_first_success", "resumable"])
        );
        assert_eq!(
            summary["properties"]["session_id"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert!(
            !summary["required"]
                .as_array()
                .expect("final summary required fields")
                .iter()
                .any(|field| field == "session_snapshot_state"),
            "additive v1 fields must not invalidate historical summaries"
        );
        assert_eq!(
            summary["properties"]["outcome"]["enum"],
            serde_json::json!([
                "succeeded",
                "partially_succeeded",
                "user_fault",
                "provider_fault",
                "tool_fault",
                "cancelled",
                "interrupted"
            ])
        );
        assert_eq!(
            summary["properties"]["assistant"]["enum"],
            serde_json::json!(["committed", "committed_partial", "discarded", "absent"])
        );
        assert!(
            !summary["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "outcome" || field == "assistant"),
            "new v1 outcome fields must remain additive"
        );

        let exclusions = defs["forward_compatible_event"]["allOf"][1]["properties"]["type"]["not"]
            ["enum"]
            .as_array()
            .expect("known-event exclusion list");
        assert!(
            exclusions
                .iter()
                .any(|value| value.as_str() == Some("command_result")),
            "a known command_result must not also match the forward-compatible branch"
        );
        assert!(
            exclusions
                .iter()
                .any(|value| value.as_str() == Some("review_disclosure")),
            "a known review_disclosure must not also match the forward-compatible branch"
        );
        assert!(
            exclusions
                .iter()
                .any(|value| value.as_str() == Some("output_last_message")),
            "a known output_last_message must not also match the forward-compatible branch"
        );
        assert!(
            exclusions.iter().any(|value| {
                value.as_str() == Some("structured_finalization_deferred")
            }),
            "a known structured-finalization deferral must not also match the forward-compatible branch"
        );
    }
}
