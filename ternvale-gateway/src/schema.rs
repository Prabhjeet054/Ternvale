//! Strict JSON Schema checks for control-API method params.

use serde_json::{Map, Value};
use ternvale_api::{Method, CONTROL_SCHEMA};

/// Why a body was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError(pub String);

impl std::fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validate `params` against `docs/schema/control-api.json` for `method`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(method = method.as_str()))]
pub fn validate_params(method: Method, params: &Value) -> Result<(), SchemaError> {
    let schema: Value = serde_json::from_str(CONTROL_SCHEMA)
        .map_err(|error| SchemaError(format!("embedded control schema is not JSON: {error}")))?;
    let methods = schema
        .get("methods")
        .and_then(Value::as_object)
        .ok_or_else(|| SchemaError("control schema missing methods".into()))?;
    let entry = methods
        .get(method.as_str())
        .ok_or_else(|| SchemaError(format!("no schema for {}", method.as_str())))?;
    let params_schema = entry
        .get("params")
        .ok_or_else(|| SchemaError(format!("{} schema missing params", method.as_str())))?;
    match_object(params, params_schema)
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn match_object(value: &Value, schema: &Value) -> Result<(), SchemaError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SchemaError("body must be a JSON object".into()))?;
    let additional = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);
    if !additional {
        for key in obj.keys() {
            if !properties.contains_key(key) {
                return Err(SchemaError(format!("unknown field {key:?}")));
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required {
            let Some(key) = name.as_str() else {
                continue;
            };
            if !obj.contains_key(key) {
                return Err(SchemaError(format!("missing field {key:?}")));
            }
        }
    }
    for (key, val) in obj {
        if let Some(prop) = properties.get(key) {
            check_type(key, val, prop)?;
        }
    }
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(key))]
fn check_type(key: &str, value: &Value, schema: &Value) -> Result<(), SchemaError> {
    let Some(kind) = schema.get("type").and_then(Value::as_str) else {
        return Ok(());
    };
    let ok = match kind {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => true,
    };
    if !ok {
        return Err(SchemaError(format!("field {key:?} must be {kind}")));
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            return Err(SchemaError(format!(
                "field {key:?} is not an allowed value"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_rejects_extra_fields() {
        let err = validate_params(Method::List, &serde_json::json!({"x": 1})).expect_err("extra");
        assert!(err.0.contains("unknown field"));
    }

    #[test]
    fn stop_requires_name() {
        let err = validate_params(Method::Stop, &serde_json::json!({})).expect_err("name");
        assert!(err.0.contains("name"));
        validate_params(
            Method::Stop,
            &serde_json::json!({"name": "vm", "force": true}),
        )
        .expect("ok");
    }
}
