//! What a plugin says about itself (its `definePlugin` call, as JSON).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema of the tool's input.
    #[serde(default = "object_schema")]
    pub input_schema: serde_json::Value,
    /// The plugin providing it.
    #[serde(default)]
    pub plugin: String,
}

fn object_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
}

impl PluginSpec {
    pub fn tool(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// Names are identifiers, and a plugin does not define a tool twice.
    pub fn validate(&self) -> Result<(), String> {
        let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ident(&self.name) {
            return Err(format!("plugin name `{}` must be letters, digits, '-' and '_'", self.name));
        }
        for (i, tool) in self.tools.iter().enumerate() {
            if !ident(&tool.name) || tool.name.len() > 64 {
                return Err(format!("{}: tool name `{}` must match [a-zA-Z0-9_-]{{1,64}}", self.name, tool.name));
            }
            if self.tools[..i].iter().any(|t| t.name == tool.name) {
                return Err(format!("{}: tool `{}` is defined twice", self.name, tool.name));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_describe_returns_and_checks_names() {
        let spec: PluginSpec = serde_json::from_value(serde_json::json!({
            "name": "builtin",
            "tools": [{ "name": "edit", "description": "replace text" }, { "name": "bash" }]
        }))
        .unwrap();
        assert_eq!(spec.tool("edit").unwrap().input_schema["type"], "object");
        assert!(spec.validate().is_ok());

        let mut twice = spec.clone();
        twice.tools.push(twice.tools[0].clone());
        assert!(twice.validate().unwrap_err().contains("defined twice"));
        let bad = PluginSpec { name: "no spaces".into(), ..spec };
        assert!(bad.validate().is_err());
    }
}
