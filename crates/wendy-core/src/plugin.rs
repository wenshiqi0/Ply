//! What a plugin is, as read from its source.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Type;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSpec {
    /// The plugin's directory name: letters, digits, `-` and `_`, at most
    /// 64; unique among loaded plugins.
    pub name: String,
    /// The JSDoc comment on `handle`.
    #[serde(default)]
    pub description: String,
    /// `handle`'s parameter type.
    pub input: Type,
    /// `handle`'s return type (`Promise<T>` read as `T`).
    pub output: Type,
}

/// A plugin as a tool for a model or an MCP client: their tool arguments
/// are always a JSON object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema of the tool arguments.
    pub parameters: Value,
    /// The plugin's input is not an object, so the arguments carry it as
    /// `{ "input": ... }`.
    pub wraps_input: bool,
}

impl PluginSpec {
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    pub fn tool_schema(&self) -> ToolSchema {
        let (parameters, wraps_input) = match &self.input {
            Type::Unknown => (json!({ "type": "object" }), false),
            Type::Undefined => (json!({ "type": "object", "properties": {} }), false),
            Type::Object { .. } | Type::Map { .. } => (self.input.to_json_schema(), false),
            other => (
                json!({ "type": "object", "properties": { "input": other.to_json_schema() }, "required": ["input"] }),
                true,
            ),
        };
        ToolSchema { name: self.name.clone(), description: self.description.clone(), parameters, wraps_input }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_take_objects_and_other_inputs_are_wrapped() {
        let spec = |input: Type| PluginSpec { name: "p".into(), description: "d".into(), input, output: Type::Unknown };
        let object = spec(Type::Object { fields: vec![] }).tool_schema();
        assert!(!object.wraps_input);
        assert_eq!(object.parameters["type"], "object");
        let text = spec(Type::String).tool_schema();
        assert!(text.wraps_input);
        assert_eq!(text.parameters["properties"]["input"], json!({ "type": "string" }));
        assert_eq!(spec(Type::Undefined).tool_schema().parameters, json!({ "type": "object", "properties": {} }));
    }

    #[test]
    fn names_are_identifiers() {
        assert!(PluginSpec::valid_name("edit") && PluginSpec::valid_name("git-log_2"));
        assert!(!PluginSpec::valid_name("") && !PluginSpec::valid_name("a b") && !PluginSpec::valid_name("../x"));
    }
}
