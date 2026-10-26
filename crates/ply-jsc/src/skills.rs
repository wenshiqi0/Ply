//! Parse bounded SKILL.md data. Discovery and activation belong to the skill plugin.
use crate::runtime::OpResult;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Header {
    name: String,
    description: String,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: Option<BTreeMap<String, Value>>,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<String>,
}

fn document(source: &str) -> Result<Value, String> {
    if source.len() > 65536 {
        return Err("SKILL.md exceeds 64 KiB".into());
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.split_inclusive('\n');
    let first = lines.next().ok_or("SKILL.md is empty")?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Err("SKILL.md must start with YAML frontmatter (---)".into());
    }
    let start = first.len();
    let mut end = start;
    let mut body = None;
    for line in lines {
        if end - start > 16384 {
            return Err("skill frontmatter exceeds 16 KiB".into());
        }
        if line.trim_end_matches(['\r', '\n']) == "---" {
            body = Some(&source[end + line.len()..]);
            break;
        }
        end += line.len();
    }
    let body = body.ok_or("skill frontmatter is missing its closing ---")?;
    let header: Header = serde_yaml_ng::from_str(&source[start..end])
        .map_err(|e| format!("invalid skill YAML: {e}"))?;
    let name = &header.name;
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("skill name must be 1..64 lowercase letters, digits and single hyphens".into());
    }
    let description = header.description.trim();
    if description.is_empty() || description.chars().count() > 1024 {
        return Err("skill description must contain 1..1024 characters".into());
    }
    if header
        .compatibility
        .as_ref()
        .is_some_and(|s| s.trim().is_empty() || s.chars().count() > 500)
    {
        return Err("skill compatibility must contain 1..500 characters".into());
    }
    if body.trim().is_empty() {
        return Err("skill instructions must not be empty".into());
    }
    if source
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("SKILL.md contains unsupported control characters".into());
    }
    Ok(
        json!({ "name": name, "description": description, "license": header.license,
        "compatibility": header.compatibility, "metadata": header.metadata, "allowed_tools": header.allowed_tools,
        "body": body }),
    )
}

pub(crate) fn parse(args: Value) -> OpResult {
    let Some(source) = args.get("source").and_then(Value::as_str) else {
        return OpResult::error("ERR_PLY_SKILL", "skill source must be a string");
    };
    match document(source) {
        Ok(value) => OpResult::Json(value),
        Err(error) => OpResult::error("ERR_PLY_SKILL", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn yaml_frontmatter_supports_multiline_quoted_fields_and_preserves_body() {
        let value = document("\u{feff}---\r\nname: code-review\r\ndescription: >-\r\n  Review code:\r\n  explain findings.\r\nmetadata:\r\n  author: 'Ply'\r\nallowed-tools: 'bash read'\r\n---\r\n# Review\r\n\r\nKeep `**code**` literal.\r\n").unwrap();
        assert_eq!(value["name"], "code-review");
        assert_eq!(value["description"], "Review code: explain findings.");
        assert_eq!(value["metadata"]["author"], "Ply");
        assert_eq!(value["allowed_tools"], "bash read");
        assert_eq!(
            value["body"],
            "# Review\r\n\r\nKeep `**code**` literal.\r\n"
        );
    }
    #[test]
    fn malformed_oversize_duplicate_and_recursive_metadata_is_rejected() {
        for source in [
            "# no frontmatter".into(),
            "---\nname: x\n".into(),
            "---\nname: X\ndescription: d\n---\nbody".into(),
            "---\nname: x--y\ndescription: d\n---\nbody".into(),
            "---\nname: x\nname: y\ndescription: d\n---\nbody".into(),
            "---\nname: x\ndescription: ''\n---\nbody".into(),
            "---\nname: x\ndescription: [not, text]\n---\nbody".into(),
            "---\nname: x\ndescription: d\nmetadata: &self\n  loop: *self\n---\nbody".into(),
            "---\nname: x\ndescription: d\n---\n \n".into(),
            format!(
                "---\nname: x\ndescription: {}\n---\nbody",
                "a".repeat(17000)
            ),
            format!("---\nname: x\ndescription: d\n---\n{}", "a".repeat(65536)),
        ] {
            assert!(document(&source).is_err(), "accepted invalid skill");
        }
    }

    #[test]
    fn extension_metadata_can_contain_nested_requirements_without_granting_tools() {
        let value = document("---\nname: lark-doc\ndescription: Read documents.\nmetadata:\n  requires:\n    bins: [lark-cli]\n    skills: [lark-shared]\n  cliHelp: 'lark-cli docs --help'\n---\nRead only when asked.\n").unwrap();
        assert_eq!(value["metadata"]["requires"]["bins"][0], "lark-cli");
        assert_eq!(value["metadata"]["requires"]["skills"][0], "lark-shared");
        assert!(value["allowed_tools"].is_null());
    }
}
