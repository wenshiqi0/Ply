//! What a plugin is, as read from its source, and what it puts on `ctx`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Type;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSpec {
    /// The plugin's directory name: letters, digits, `-` and `_`, at most
    /// 64; unique among loaded plugins.
    pub name: String,
    /// The JSDoc comment on `onHandle` (or, without one, on `onLoad`).
    #[serde(default)]
    pub description: String,
    /// It exports `onHandle`, so it can be called (and is a tool).
    pub callable: bool,
    /// `onHandle`'s parameter type.
    pub input: Type,
    /// `onHandle`'s return type (`Promise<T>` read as `T`).
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

/// What a plugin's `onLoad` returned, as the global `ctx` carries it in
/// every realm (flat: members of all plugins side by side, a later mount
/// of the same name overriding an earlier one): plain members are copied
/// (a snapshot taken when `onLoad` returned), function members are called
/// in the owning plugin's realm.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mounts {
    #[serde(default)]
    pub data: BTreeMap<String, Value>,
    #[serde(default)]
    pub methods: Vec<String>,
}

impl Mounts {
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.data.keys().chain(&self.methods)
    }
}

impl PluginSpec {
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    /// The plugin as a tool, if it can be called.
    pub fn tool_schema(&self) -> Option<ToolSchema> {
        if !self.callable {
            return None;
        }
        let (parameters, wraps_input) = match &self.input {
            Type::Unknown => (json!({ "type": "object" }), false),
            Type::Undefined => (json!({ "type": "object", "properties": {} }), false),
            Type::Object { .. } | Type::Map { .. } => (self.input.to_json_schema(), false),
            other => (
                json!({ "type": "object", "properties": { "input": other.to_json_schema() }, "required": ["input"] }),
                true,
            ),
        };
        Some(ToolSchema { name: self.name.clone(), description: self.description.clone(), parameters, wraps_input })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(input: Type, callable: bool) -> PluginSpec {
        PluginSpec { name: "p".into(), description: "d".into(), callable, input, output: Type::Unknown }
    }

    #[test]
    fn tools_take_objects_and_other_inputs_are_wrapped() {
        let object = spec(Type::Object { fields: vec![] }, true).tool_schema().unwrap();
        assert!(!object.wraps_input);
        assert_eq!(object.parameters["type"], "object");
        let text = spec(Type::String, true).tool_schema().unwrap();
        assert!(text.wraps_input);
        assert_eq!(text.parameters["properties"]["input"], json!({ "type": "string" }));
        assert_eq!(spec(Type::Undefined, true).tool_schema().unwrap().parameters, json!({ "type": "object", "properties": {} }));
        assert_eq!(spec(Type::Undefined, false).tool_schema(), None);
    }

    #[test]
    fn names_are_identifiers() {
        assert!(PluginSpec::valid_name("edit") && PluginSpec::valid_name("git-log_2"));
        assert!(!PluginSpec::valid_name("") && !PluginSpec::valid_name("a b") && !PluginSpec::valid_name("../x"));
    }
}
