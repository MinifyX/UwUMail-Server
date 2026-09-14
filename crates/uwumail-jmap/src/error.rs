//! Method-level and set-level errors (RFC 8620, sections 3.6.2 and 5.3).

use serde_json::{Map, Value, json};
use uwumail_store::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodError {
    pub kind: &'static str,
    pub description: Option<String>,
}

pub type MethodResult<T> = Result<T, MethodError>;

impl MethodError {
    pub fn new(kind: &'static str, description: impl Into<String>) -> MethodError {
        MethodError { kind, description: Some(description.into()) }
    }

    pub fn invalid_arguments(description: impl Into<String>) -> MethodError {
        MethodError::new("invalidArguments", description)
    }

    pub fn server_fail(description: impl Into<String>) -> MethodError {
        MethodError::new("serverFail", description)
    }

    pub fn kind(kind: &'static str) -> MethodError {
        MethodError { kind, description: None }
    }

    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("type".into(), json!(self.kind));
        if let Some(description) = &self.description {
            object.insert("description".into(), json!(description));
        }
        Value::Object(object)
    }
}

impl From<StoreError> for MethodError {
    fn from(err: StoreError) -> MethodError {
        match err {
            StoreError::Invalid(message) => MethodError::invalid_arguments(message),
            other => {
                tracing::error!(err = %other, "store error in a JMAP method");
                MethodError::server_fail("something went wrong on the server")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetError {
    pub kind: &'static str,
    pub description: Option<String>,
    pub properties: Option<Vec<String>>,
}

impl SetError {
    pub fn new(kind: &'static str, description: impl Into<String>) -> SetError {
        SetError { kind, description: Some(description.into()), properties: None }
    }

    pub fn invalid_properties(properties: &[&str], description: impl Into<String>) -> SetError {
        SetError {
            kind: "invalidProperties",
            description: Some(description.into()),
            properties: Some(properties.iter().map(|p| p.to_string()).collect()),
        }
    }

    pub fn not_found() -> SetError {
        SetError { kind: "notFound", description: None, properties: None }
    }

    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("type".into(), json!(self.kind));
        if let Some(description) = &self.description {
            object.insert("description".into(), json!(description));
        }
        if let Some(properties) = &self.properties {
            object.insert("properties".into(), json!(properties));
        }
        Value::Object(object)
    }
}

impl From<StoreError> for SetError {
    fn from(err: StoreError) -> SetError {
        match err {
            StoreError::NotFound(_) => SetError::not_found(),
            StoreError::Rule { code, message } => SetError::new(code, message),
            StoreError::Invalid(message) => SetError::new("invalidProperties", message),
            StoreError::QuotaExceeded => SetError::new("overQuota", "the mailbox is full"),
            other => {
                tracing::error!(err = %other, "store error in a JMAP set");
                SetError::new("serverFail", "something went wrong on the server")
            }
        }
    }
}
