//! The tools bounded contexts offer agents through MCP

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use schemars::{JsonSchema, schema_for};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use validator::ValidationErrors;

pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 50;

/// What a tool does to the data, which decides who may call it and whether its session can write
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Read,
    Write,
    Delete,
}

/// How much a tool's answer holds
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    /// The fields that tell records apart, and their ids for later calls
    #[default]
    Concise,
    /// Every field
    Detailed,
}

#[derive(Debug)]
pub enum ToolError {
    /// Something the agent can fix, told in the message
    Refused(String),
    /// A fault of the server
    Internal(anyhow::Error),
}

type Run = dyn Fn(Surreal<Any>, Value) -> Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send>>
    + Send
    + Sync;

#[derive(Clone)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub effect: Effect,
    pub input_schema: Arc<Map<String, Value>>,
    run: Arc<Run>,
}

impl Tool {
    /// `handler` gets a session limited to what the caller may see, read-only for [`Effect::Read`]
    pub fn new<I, O, F, Fut>(
        name: &'static str,
        effect: Effect,
        description: &'static str,
        handler: F,
    ) -> Self
    where
        I: DeserializeOwned + JsonSchema,
        O: Serialize,
        F: Fn(Surreal<Any>, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, ToolError>> + Send + 'static,
    {
        let input_schema = match schema_for!(I).to_value() {
            Value::Object(schema) => schema,
            _ => Map::new(),
        };
        let run = Arc::new(move |db: Surreal<Any>, input: Value| {
            let parsed = serde_json::from_value::<I>(input)
                .map_err(|error| ToolError::Refused(format!("invalid input: {error}")));
            let call = parsed.map(|input| handler(db, input));
            let answer: Pin<Box<dyn Future<Output = _> + Send>> = Box::pin(async move {
                let output = call?.await?;
                serde_json::to_value(output).map_err(|error| ToolError::Internal(error.into()))
            });
            answer
        });
        Self {
            name,
            description,
            effect,
            input_schema: Arc::new(input_schema),
            run,
        }
    }

    pub async fn run(&self, db: Surreal<Any>, input: Value) -> Result<Value, ToolError> {
        (self.run)(db, input).await
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("effect", &self.effect)
            .finish_non_exhaustive()
    }
}

impl ToolError {
    pub fn refused(message: impl Into<String>) -> Self {
        Self::Refused(message.into())
    }
}

impl From<ValidationErrors> for ToolError {
    fn from(errors: ValidationErrors) -> Self {
        let mut fields: Vec<String> = errors
            .field_errors()
            .into_iter()
            .map(|(field, errors)| {
                let messages: Vec<String> = errors
                    .iter()
                    .map(|error| match &error.message {
                        Some(message) => message.to_string(),
                        None => "is invalid".to_owned(),
                    })
                    .collect();
                format!("{field}: {}", messages.join(", "))
            })
            .collect();
        fields.sort_unstable();
        Self::Refused(format!("invalid input: {}", fields.join("; ")))
    }
}

impl From<resysto_surrealdb::Error> for ToolError {
    fn from(error: resysto_surrealdb::Error) -> Self {
        Self::Internal(error.into())
    }
}

/// The rows a list reads: 20 when the agent asks for no number, 50 at most
pub fn limit(asked: Option<u32>) -> u32 {
    asked.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}
