//! The JSON process contract (version 1).
//!
//! Every JSON invocation prints one newline-terminated object:
//! `{ok, command, result, warnings, error, metadata}`. `ok` is true exactly
//! when the exit status is 0. `error.code` is a stable category; messages
//! and hints are for people. A successful envelope may also carry a top-level
//! `advisories` array holding at most one release notice; advisories never
//! change `ok`, `result`, or the exit status.

use serde::Serialize;
use serde_json::{Value, json};

use dsd::{Error, ErrorCode, Issue};

pub const CONTRACT_VERSION: &str = "1";

#[derive(Debug, Clone, Serialize)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

impl Warning {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into() }
    }
}

impl From<Issue> for Warning {
    fn from(issue: Issue) -> Self {
        Self { code: issue.code, message: issue.message }
    }
}

/// A failed invocation.
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub issues: Vec<Issue>,
    /// A partial or diagnostic result, such as `check`'s issue list.
    pub result: Option<Value>,
    usage: bool,
}

impl Failure {
    pub fn usage(message: String, hint: String) -> Self {
        Self { code: "invalid_invocation", message, hint: Some(hint), issues: Vec::new(), result: None, usage: true }
    }

    pub fn with_result(mut self, result: Value) -> Self {
        self.result = Some(result);
        self
    }

    pub const fn exit_code(&self) -> u8 {
        if self.usage { 2 } else { 1 }
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self { code: e.code.as_str(), message: e.message, hint: e.hint, issues: e.issues, result: None, usage: false }
    }
}

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Error::new(code, message).into()
    }

    /// A failure whose category belongs to the CLI rather than the library,
    /// such as an upgrade error.
    pub fn cli(code: &'static str, message: impl Into<String>, hint: Option<String>) -> Self {
        Self { code, message: message.into(), hint, issues: Vec::new(), result: None, usage: false }
    }
}

pub struct Envelope(Value);

fn metadata() -> Value {
    json!({"tool": "dsd", "version": crate::VERSION, "contract": CONTRACT_VERSION})
}

impl Envelope {
    pub fn success(command: &str, result: Value, warnings: Vec<Warning>) -> Self {
        let mut map = serde_json::Map::new();
        map.insert("ok".into(), Value::Bool(true));
        map.insert("command".into(), json!(command));
        map.insert("result".into(), result);
        map.insert("warnings".into(), Value::Array(warnings.into_iter().map(|w| json!(w)).collect()));
        map.insert("error".into(), Value::Null);
        map.insert("metadata".into(), metadata());
        Self(Value::Object(map))
    }

    /// Adds release advisories to a success envelope; none leaves it unchanged.
    pub fn with_advisories(mut self, advisories: Vec<Value>) -> Self {
        if let (Value::Object(map), false) = (&mut self.0, advisories.is_empty()) {
            map.insert("advisories".into(), Value::Array(advisories));
        }
        self
    }

    pub fn failure(command: &str, failure: &Failure) -> Self {
        let mut error = json!({"code": failure.code, "message": failure.message});
        if let Some(hint) = &failure.hint {
            error["hint"] = json!(hint);
        }
        if !failure.issues.is_empty() {
            error["issues"] = json!(failure.issues);
        }
        Self(json!({
            "ok": false,
            "command": if command.is_empty() { Value::Null } else { json!(command) },
            "result": failure.result.clone().unwrap_or(Value::Null),
            "warnings": [],
            "error": error,
            "metadata": metadata(),
        }))
    }

    pub fn render(&self, pretty: bool) -> String {
        let mut text = if pretty { serde_json::to_string_pretty(&self.0) } else { serde_json::to_string(&self.0) }
            .expect("JSON values always serialize");
        text.push('\n');
        text
    }
}
