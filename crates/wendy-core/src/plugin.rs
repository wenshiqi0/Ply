//! What a plugin is, as read from its source.

use serde::{Deserialize, Serialize};

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

impl PluginSpec {
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_identifiers() {
        assert!(PluginSpec::valid_name("edit") && PluginSpec::valid_name("git-log_2"));
        assert!(!PluginSpec::valid_name("") && !PluginSpec::valid_name("a b") && !PluginSpec::valid_name("../x"));
    }
}
