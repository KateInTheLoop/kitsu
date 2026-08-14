//! Bounded, provider-backed structured final output for automation.
//!
//! The provider receives the JSON Schema through OpenAI-compatible
//! `response_format`. Kitsu independently validates the terminal assistant
//! text before committing it, so a gateway that ignores the request cannot
//! turn malformed output into successful automation.

use std::collections::HashSet;
use std::path::Path;

use serde_json::{Number, Value};

use crate::{session, skills};

pub(crate) const MAX_OUTPUT_SCHEMA_BYTES: u64 = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputValidationError {
    pub(crate) instance_path: String,
    pub(crate) validation_keyword: &'static str,
    message: String,
}

impl OutputValidationError {
    fn new(
        instance_path: impl Into<String>,
        validation_keyword: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            instance_path: instance_path.into(),
            validation_keyword,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for OutputValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn json_value_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => json_number_equal(left, right),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_value_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| json_value_equal(left, right))
                })
        }
        _ => left == right,
    }
}

fn json_number_equal(left: &Number, right: &Number) -> bool {
    if left == right {
        return true;
    }
    let integer = |number: &Number| {
        number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
    };
    let float_equals_integer = |number: f64, integer: i128| {
        if !number.is_finite() || number.fract() != 0.0 {
            return false;
        }
        if integer < 0 {
            (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number)
                && (number as i128) == integer
        } else {
            (0.0..18_446_744_073_709_551_616.0).contains(&number)
                && (number as u128) == integer as u128
        }
    };
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => left == right,
        (Some(integer), None) => right
            .as_f64()
            .is_some_and(|number| float_equals_integer(number, integer)),
        (None, Some(integer)) => left
            .as_f64()
            .is_some_and(|number| float_equals_integer(number, integer)),
        (None, None) => left
            .as_f64()
            .zip(right.as_f64())
            .is_some_and(|(left, right)| left == right),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OutputSchema {
    pub(crate) value: Value,
    pub(crate) bytes: usize,
    pub(crate) sha256: String,
}

impl OutputSchema {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bytes =
            session::read_bounded_host_file(path, MAX_OUTPUT_SCHEMA_BYTES, "output-schema")?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "--output-schema file '{}' is not valid JSON: {error}",
                path.display()
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            format!(
                "--output-schema file '{}' must contain a JSON Schema object",
                path.display()
            )
        })?;
        if object.get("type").and_then(Value::as_str) != Some("object") {
            return Err(format!(
                "--output-schema file '{}' must have root type \"object\"",
                path.display()
            ));
        }
        validate_schema_shape(&value, &value, "$", 0).map_err(|error| {
            format!(
                "unsupported --output-schema file '{}': {error}",
                path.display()
            )
        })?;
        Ok(Self {
            sha256: skills::content_sha256_hex(&bytes),
            bytes: bytes.len(),
            value,
        })
    }

    pub(crate) fn validate_text(&self, text: &str) -> Result<(), OutputValidationError> {
        let instance: Value = serde_json::from_str(text).map_err(|error| {
            OutputValidationError::new(
                "$",
                "json_syntax",
                format!("final assistant text is not valid JSON: {error}"),
            )
        })?;
        validate_instance(&self.value, &self.value, &instance, "$", 0).map_err(|mut error| {
            error.message = format!(
                "final assistant JSON does not match the schema: {}",
                error.message
            );
            error
        })
    }
}

fn supported_keyword(keyword: &str) -> bool {
    matches!(
        keyword,
        "$schema"
            | "$id"
            | "$comment"
            | "$defs"
            | "definitions"
            | "$ref"
            | "title"
            | "description"
            | "default"
            | "examples"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
            | "type"
            | "const"
            | "enum"
            | "allOf"
            | "anyOf"
            | "oneOf"
            | "not"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "prefixItems"
            | "minItems"
            | "maxItems"
            | "uniqueItems"
            | "minLength"
            | "maxLength"
            | "pattern"
            | "minimum"
            | "maximum"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
            | "multipleOf"
    )
}

fn schema_type_names<'a>(value: &'a Value, at: &str) -> Result<Vec<&'a str>, String> {
    let Some(value) = value.get("type") else {
        return Ok(Vec::new());
    };
    let names = match value {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) if !names.is_empty() => names
            .iter()
            .map(|name| {
                name.as_str()
                    .ok_or_else(|| format!("{at}/type entries must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(format!("{at}/type must be a string or non-empty array")),
    };
    let mut unique = HashSet::new();
    for name in &names {
        if !matches!(
            *name,
            "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
        ) {
            return Err(format!("{at}/type contains unsupported type '{name}'"));
        }
        if !unique.insert(*name) {
            return Err(format!("{at}/type repeats '{name}'"));
        }
    }
    Ok(names)
}

fn nonnegative_integer(
    object: &serde_json::Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<(), String> {
    if object
        .get(key)
        .is_some_and(|value| value.as_u64().is_none())
    {
        return Err(format!("{at}/{key} must be a non-negative integer"));
    }
    Ok(())
}

fn numeric_keyword(
    object: &serde_json::Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<(), String> {
    if object.get(key).is_some_and(|value| !value.is_number()) {
        return Err(format!("{at}/{key} must be a number"));
    }
    Ok(())
}

fn validate_schema_array(
    root: &Value,
    value: Option<&Value>,
    key: &str,
    at: &str,
    depth: usize,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let schemas = value
        .as_array()
        .filter(|schemas| !schemas.is_empty())
        .ok_or_else(|| format!("{at}/{key} must be a non-empty schema array"))?;
    for (index, schema) in schemas.iter().enumerate() {
        validate_schema_shape(root, schema, &format!("{at}/{key}/{index}"), depth + 1)?;
    }
    Ok(())
}

fn validate_schema_shape(
    root: &Value,
    schema: &Value,
    at: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!(
            "{at} exceeds the {MAX_SCHEMA_DEPTH}-level schema limit"
        ));
    }
    if schema.is_boolean() {
        return Ok(());
    }
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{at} must be a schema object or Boolean"))?;
    for keyword in object.keys() {
        if !supported_keyword(keyword) {
            return Err(format!("{at} uses unsupported keyword '{keyword}'"));
        }
    }
    for key in ["$schema", "$id", "$comment", "title", "description"] {
        if object.get(key).is_some_and(|value| !value.is_string()) {
            return Err(format!("{at}/{key} must be a string"));
        }
    }
    for key in ["deprecated", "readOnly", "writeOnly"] {
        if object.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("{at}/{key} must be Boolean"));
        }
    }
    if object
        .get("examples")
        .is_some_and(|value| !value.is_array())
    {
        return Err(format!("{at}/examples must be an array"));
    }
    let types = schema_type_names(schema, at)?;

    if let Some(reference) = object.get("$ref") {
        let reference = reference
            .as_str()
            .ok_or_else(|| format!("{at}/$ref must be a string"))?;
        let supported_location = ["#/$defs/", "#/definitions/", "#/properties/"]
            .iter()
            .any(|prefix| reference.starts_with(prefix));
        let target = supported_location
            .then(|| root.pointer(&reference[1..]))
            .flatten();
        if !target.is_some_and(|target| target.is_object() || target.is_boolean()) {
            return Err(format!(
                "{at}/$ref must resolve to a schema below $defs, definitions, or properties"
            ));
        }
    }
    if let Some(enumeration) = object.get("enum") {
        let values = enumeration
            .as_array()
            .filter(|values| !values.is_empty())
            .ok_or_else(|| format!("{at}/enum must be a non-empty array"))?;
        if values.iter().enumerate().any(|(index, value)| {
            values[..index]
                .iter()
                .any(|previous| json_value_equal(previous, value))
        }) {
            return Err(format!("{at}/enum values must be unique"));
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        validate_schema_array(root, object.get(key), key, at, depth)?;
    }
    if let Some(child) = object.get("not") {
        validate_schema_shape(root, child, &format!("{at}/not"), depth + 1)?;
    }
    if let Some(child) = object.get("items") {
        validate_schema_shape(root, child, &format!("{at}/items"), depth + 1)?;
    }
    for key in ["$defs", "definitions", "properties"] {
        if let Some(children) = object.get(key) {
            let children = children
                .as_object()
                .ok_or_else(|| format!("{at}/{key} must be an object"))?;
            for (name, child) in children {
                validate_schema_shape(root, child, &format!("{at}/{key}/{name}"), depth + 1)?;
            }
        }
    }
    if let Some(additional) = object.get("additionalProperties") {
        if !additional.is_boolean() {
            validate_schema_shape(
                root,
                additional,
                &format!("{at}/additionalProperties"),
                depth + 1,
            )?;
        }
    }
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| format!("{at}/required must be an array"))?;
        let mut unique = HashSet::new();
        for name in required {
            let name = name
                .as_str()
                .ok_or_else(|| format!("{at}/required entries must be strings"))?;
            if !unique.insert(name) {
                return Err(format!("{at}/required repeats '{name}'"));
            }
        }
    }
    for key in ["minItems", "maxItems", "minLength", "maxLength"] {
        nonnegative_integer(object, key, at)?;
    }
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ] {
        numeric_keyword(object, key, at)?;
    }
    if object
        .get("multipleOf")
        .and_then(Value::as_f64)
        .is_some_and(|value| value <= 0.0)
    {
        return Err(format!("{at}/multipleOf must be greater than zero"));
    }
    if object
        .get("uniqueItems")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(format!("{at}/uniqueItems must be Boolean"));
    }
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or_else(|| format!("{at}/pattern must be a string"))?;
        regex::Regex::new(pattern)
            .map_err(|error| format!("{at}/pattern is not a valid regular expression: {error}"))?;
    }

    let object_shaped = types.contains(&"object") || object.contains_key("properties");
    if object_shaped {
        if object.get("additionalProperties") != Some(&Value::Bool(false)) {
            return Err(format!(
                "{at} object schemas must set additionalProperties to false"
            ));
        }
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{at} object schemas must define properties"))?;
        let required = object
            .get("required")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{at} object schemas must require every property"))?;
        let required = required
            .iter()
            .filter_map(Value::as_str)
            .collect::<HashSet<_>>();
        if properties
            .keys()
            .any(|name| !required.contains(name.as_str()))
            || required.len() != properties.len()
        {
            return Err(format!(
                "{at} object schemas must list every property exactly once in required"
            ));
        }
    }
    if types.contains(&"array") && !object.contains_key("items") {
        return Err(format!("{at} array schemas must define items"));
    }
    Ok(())
}

fn type_matches(name: &str, instance: &Value) -> bool {
    match name {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "number" => instance.is_number(),
        "integer" => {
            instance.as_i64().is_some()
                || instance.as_u64().is_some()
                || instance
                    .as_f64()
                    .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
        }
        "string" => instance.is_string(),
        _ => false,
    }
}

fn instance_child(at: &str, token: &str) -> String {
    let escaped = token.replace('~', "~0").replace('/', "~1");
    format!("{at}/{escaped}")
}

fn validation_error(
    at: &str,
    validation_keyword: &'static str,
    description: impl std::fmt::Display,
) -> OutputValidationError {
    OutputValidationError::new(at, validation_keyword, format!("{at} {description}"))
}

fn validate_instance(
    root: &Value,
    schema: &Value,
    instance: &Value,
    at: &str,
    depth: usize,
) -> Result<(), OutputValidationError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(validation_error(
            at,
            "max_depth",
            format_args!("exceeds the {MAX_SCHEMA_DEPTH}-level value limit"),
        ));
    }
    if let Some(allowed) = schema.as_bool() {
        return if allowed {
            Ok(())
        } else {
            Err(validation_error(
                at,
                "false_schema",
                "is rejected by a false schema",
            ))
        };
    }
    let object = schema
        .as_object()
        .ok_or_else(|| validation_error(at, "schema", "encountered an invalid schema node"))?;
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let target = root.pointer(&reference[1..]).ok_or_else(|| {
            validation_error(
                at,
                "$ref",
                format_args!("references missing schema '{reference}'"),
            )
        })?;
        validate_instance(root, target, instance, at, depth + 1)?;
    }
    if let Some(expected) = object.get("const") {
        if !json_value_equal(instance, expected) {
            return Err(validation_error(at, "const", "does not equal const"));
        }
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        if !values.iter().any(|value| json_value_equal(value, instance)) {
            return Err(validation_error(
                at,
                "enum",
                "is not one of the allowed enum values",
            ));
        }
    }
    if let Some(schemas) = object.get("allOf").and_then(Value::as_array) {
        for child in schemas {
            validate_instance(root, child, instance, at, depth + 1)?;
        }
    }
    if let Some(schemas) = object.get("anyOf").and_then(Value::as_array) {
        if !schemas
            .iter()
            .any(|child| validate_instance(root, child, instance, at, depth + 1).is_ok())
        {
            return Err(validation_error(
                at,
                "anyOf",
                "does not match any anyOf branch",
            ));
        }
    }
    if let Some(schemas) = object.get("oneOf").and_then(Value::as_array) {
        let matches = schemas
            .iter()
            .filter(|child| validate_instance(root, child, instance, at, depth + 1).is_ok())
            .count();
        if matches != 1 {
            return Err(validation_error(
                at,
                "oneOf",
                format_args!("matches {matches} oneOf branches instead of one"),
            ));
        }
    }
    if let Some(child) = object.get("not") {
        if validate_instance(root, child, instance, at, depth + 1).is_ok() {
            return Err(validation_error(
                at,
                "not",
                "matches a forbidden not schema",
            ));
        }
    }
    let types = schema_type_names(schema, at)
        .map_err(|message| OutputValidationError::new(at, "schema", message))?;
    if !types.is_empty() && !types.iter().any(|name| type_matches(name, instance)) {
        let actual = match instance {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        return Err(validation_error(
            at,
            "type",
            format_args!("has type {actual}, expected {}", types.join(" or ")),
        ));
    }

    if let Some(value) = instance.as_object() {
        let properties = object.get("properties").and_then(Value::as_object);
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !value.contains_key(name) {
                    return Err(validation_error(
                        at,
                        "required",
                        format_args!("is missing required property '{name}'"),
                    ));
                }
            }
        }
        for (name, member) in value {
            let child_at = instance_child(at, name);
            if let Some(child) = properties.and_then(|properties| properties.get(name)) {
                validate_instance(root, child, member, &child_at, depth + 1)?;
            } else {
                match object.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        return Err(OutputValidationError::new(
                            child_at,
                            "additionalProperties",
                            format!("{at} contains additional property '{name}'"),
                        ));
                    }
                    Some(child) if !child.is_boolean() => {
                        validate_instance(root, child, member, &child_at, depth + 1)?;
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(value) = instance.as_array() {
        if let Some(minimum) = object.get("minItems").and_then(Value::as_u64) {
            if value.len() < minimum as usize {
                return Err(validation_error(
                    at,
                    "minItems",
                    format_args!("has fewer than {minimum} items"),
                ));
            }
        }
        if let Some(maximum) = object.get("maxItems").and_then(Value::as_u64) {
            if value.len() > maximum as usize {
                return Err(validation_error(
                    at,
                    "maxItems",
                    format_args!("has more than {maximum} items"),
                ));
            }
        }
        if object.get("uniqueItems") == Some(&Value::Bool(true)) {
            for (index, item) in value.iter().enumerate() {
                if value[..index]
                    .iter()
                    .any(|previous| json_value_equal(previous, item))
                {
                    let child_at = instance_child(at, &index.to_string());
                    return Err(OutputValidationError::new(
                        child_at.clone(),
                        "uniqueItems",
                        format!("{child_at} duplicates an earlier item"),
                    ));
                }
            }
        }
        let prefix = object
            .get("prefixItems")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let items = object.get("items");
        for (index, item) in value.iter().enumerate() {
            let child = prefix.get(index).or(items);
            if let Some(child) = child {
                validate_instance(
                    root,
                    child,
                    item,
                    &instance_child(at, &index.to_string()),
                    depth + 1,
                )?;
            }
        }
    }
    if let Some(value) = instance.as_str() {
        let characters = value.chars().count() as u64;
        if object
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| characters < minimum)
        {
            return Err(validation_error(
                at,
                "minLength",
                "is shorter than minLength",
            ));
        }
        if object
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| characters > maximum)
        {
            return Err(validation_error(
                at,
                "maxLength",
                "is longer than maxLength",
            ));
        }
        if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
            if !regex::Regex::new(pattern)
                .expect("schema pattern validated at load")
                .is_match(value)
            {
                return Err(validation_error(at, "pattern", "does not match pattern"));
            }
        }
    }
    if let Some(number) = instance.as_f64() {
        for (key, passes) in [
            (
                "minimum",
                object
                    .get("minimum")
                    .and_then(Value::as_f64)
                    .map_or(true, |minimum| number >= minimum),
            ),
            (
                "maximum",
                object
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .map_or(true, |maximum| number <= maximum),
            ),
            (
                "exclusiveMinimum",
                object
                    .get("exclusiveMinimum")
                    .and_then(Value::as_f64)
                    .map_or(true, |minimum| number > minimum),
            ),
            (
                "exclusiveMaximum",
                object
                    .get("exclusiveMaximum")
                    .and_then(Value::as_f64)
                    .map_or(true, |maximum| number < maximum),
            ),
        ] {
            if !passes {
                return Err(validation_error(at, key, format_args!("violates {key}")));
            }
        }
        if let Some(divisor) = object.get("multipleOf").and_then(Value::as_f64) {
            let quotient = number / divisor;
            let tolerance = f64::EPSILON * quotient.abs().max(1.0) * 8.0;
            if (quotient - quotient.round()).abs() > tolerance {
                return Err(validation_error(
                    at,
                    "multipleOf",
                    format_args!("is not a multipleOf {divisor}"),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["ok", "error"]},
                "count": {"type": "integer", "minimum": 0},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["status", "count", "tags"],
            "additionalProperties": false
        })
    }

    #[test]
    fn supported_schema_validates_exact_json_shape() {
        assert!(json_value_equal(
            &serde_json::json!(1),
            &serde_json::json!(1.0)
        ));
        assert!(!json_value_equal(
            &serde_json::json!(9_007_199_254_740_993_u64),
            &serde_json::json!(9_007_199_254_740_992.0)
        ));
        let value = schema();
        validate_schema_shape(&value, &value, "$", 0).unwrap();
        validate_instance(
            &value,
            &value,
            &serde_json::json!({"status":"ok", "count":2, "tags":["a"]}),
            "$",
            0,
        )
        .unwrap();
        assert!(validate_instance(
            &value,
            &value,
            &serde_json::json!({"status":"maybe", "count":-1, "tags":[]}),
            "$",
            0,
        )
        .is_err());
        assert!(validate_instance(
            &value,
            &value,
            &serde_json::json!({"status":"ok", "count":2, "tags":[], "extra":true}),
            "$",
            0,
        )
        .is_err());

        let constant = serde_json::json!({
            "type":"object",
            "properties":{"accepted":{"const":true}},
            "required":["accepted"],
            "additionalProperties":false
        });
        let error = validate_instance(
            &constant,
            &constant,
            &serde_json::json!({"accepted":false}),
            "$",
            0,
        )
        .unwrap_err();
        assert_eq!(error.instance_path, "$/accepted");
        assert_eq!(error.validation_keyword, "const");

        let syntax = OutputSchema {
            value: value.clone(),
            bytes: 0,
            sha256: "0".repeat(64),
        }
        .validate_text("not json")
        .unwrap_err();
        assert_eq!(syntax.instance_path, "$");
        assert_eq!(syntax.validation_keyword, "json_syntax");
    }

    #[test]
    fn schema_shape_fails_closed_on_unknown_or_non_strict_objects() {
        let unknown = serde_json::json!({
            "type":"object", "properties":{}, "required":[],
            "additionalProperties":false, "format":"mystery"
        });
        assert!(validate_schema_shape(&unknown, &unknown, "$", 0)
            .unwrap_err()
            .contains("unsupported keyword 'format'"));
        let open = serde_json::json!({
            "type":"object", "properties":{"x":{"type":"string"}}, "required":[]
        });
        assert!(validate_schema_shape(&open, &open, "$", 0)
            .unwrap_err()
            .contains("additionalProperties"));

        let malformed_annotation = serde_json::json!({
            "type":"object", "properties":{}, "required":[],
            "additionalProperties":false, "description":42
        });
        assert!(
            validate_schema_shape(&malformed_annotation, &malformed_annotation, "$", 0)
                .unwrap_err()
                .contains("description must be a string")
        );

        let duplicate_enum = serde_json::json!({
            "type":"object",
            "properties":{"answer":{"type":"number", "enum":[1, 1.0]}},
            "required":["answer"], "additionalProperties":false
        });
        assert!(
            validate_schema_shape(&duplicate_enum, &duplicate_enum, "$", 0)
                .unwrap_err()
                .contains("enum values must be unique")
        );
    }

    #[test]
    fn local_refs_and_combinators_are_checked() {
        let value = serde_json::json!({
            "type":"object",
            "properties":{"answer":{"$ref":"#/$defs/answer"}},
            "required":["answer"],
            "additionalProperties":false,
            "$defs":{"answer":{"anyOf":[{"const":"yes"},{"const":"no"}]}}
        });
        validate_schema_shape(&value, &value, "$", 0).unwrap();
        validate_instance(&value, &value, &serde_json::json!({"answer":"yes"}), "$", 0).unwrap();
        assert!(validate_instance(
            &value,
            &value,
            &serde_json::json!({"answer":"maybe"}),
            "$",
            0,
        )
        .is_err());

        let invalid_reference = serde_json::json!({
            "type":"object",
            "title":"not a schema",
            "properties":{"answer":{"$ref":"#/title"}},
            "required":["answer"],
            "additionalProperties":false
        });
        assert!(
            validate_schema_shape(&invalid_reference, &invalid_reference, "$", 0)
                .unwrap_err()
                .contains("must resolve to a schema below")
        );
    }

    #[test]
    fn schema_file_load_is_bounded_and_records_exact_source_bytes() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("answer.schema.json");
        let bytes = serde_json::to_vec(&schema()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let loaded = OutputSchema::load(&path).unwrap();
        assert_eq!(loaded.value, schema());
        assert_eq!(loaded.bytes, bytes.len());
        assert_eq!(loaded.sha256.len(), 64);

        let oversized = root.path().join("oversized.schema.json");
        std::fs::write(&oversized, vec![b' '; MAX_OUTPUT_SCHEMA_BYTES as usize + 1]).unwrap();
        assert!(OutputSchema::load(&oversized)
            .unwrap_err()
            .contains("limit is 65536"));
    }

    #[cfg(unix)]
    #[test]
    fn schema_file_load_rejects_final_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::TempDir::new().unwrap();
        let real = root.path().join("real.schema.json");
        let alias = root.path().join("alias.schema.json");
        std::fs::write(&real, serde_json::to_vec(&schema()).unwrap()).unwrap();
        symlink(&real, &alias).unwrap();
        let error = OutputSchema::load(&alias).unwrap_err();
        assert!(error.contains("cannot open output-schema file"), "{error}");
    }
}
