//! The description of a plugin's input and output: the structural part of
//! TypeScript that a function signature usually uses.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Type {
    /// `any`, `unknown`, or something ply does not read (generics,
    /// unresolved/external types, conditional types...).
    Unknown,
    String,
    Number,
    Boolean,
    Null,
    /// `undefined` / `void`.
    Undefined,
    /// A string, number or boolean literal type.
    Literal { value: Value },
    /// `T[]`, `Array<T>`.
    Array { items: Box<Type> },
    /// An object literal type or interface.
    Object { fields: Vec<Field> },
    /// `Record<string, T>`, `{ [key: string]: T }`.
    Map { values: Box<Type> },
    /// `A | B`.
    Union { of: Vec<Type> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(rename = "type")]
    pub ty: Type,
    /// The JSDoc comment on the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Type {
    /// The same description in JSON Schema, for clients that speak it.
    pub fn to_json_schema(&self) -> Value {
        match self {
            Type::Unknown => json!({}),
            Type::String => json!({ "type": "string" }),
            Type::Number => json!({ "type": "number" }),
            Type::Boolean => json!({ "type": "boolean" }),
            Type::Null | Type::Undefined => json!({ "type": "null" }),
            Type::Literal { value } => json!({ "const": value }),
            Type::Array { items } => json!({ "type": "array", "items": items.to_json_schema() }),
            Type::Map { values } => json!({ "type": "object", "additionalProperties": values.to_json_schema() }),
            Type::Object { fields } => {
                let mut properties = Map::new();
                for field in fields {
                    let mut schema = field.ty.to_json_schema();
                    if let (Some(text), Value::Object(map)) = (&field.description, &mut schema) {
                        map.insert("description".into(), json!(text));
                    }
                    properties.insert(field.name.clone(), schema);
                }
                let required: Vec<&str> = fields.iter().filter(|f| !f.optional).map(|f| f.name.as_str()).collect();
                json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
            }
            Type::Union { of } => {
                // `T | undefined` is an optional T.
                let members: Vec<&Type> = of.iter().filter(|t| **t != Type::Undefined).collect();
                match members.as_slice() {
                    [] => json!({ "type": "null" }),
                    [only] => only.to_json_schema(),
                    _ if members.iter().all(|t| matches!(t, Type::Literal { .. })) => {
                        let values: Vec<&Value> =
                            members.iter().filter_map(|t| if let Type::Literal { value } = t { Some(value) } else { None }).collect();
                        json!({ "enum": values })
                    }
                    _ => json!({ "anyOf": members.iter().map(|t| t.to_json_schema()).collect::<Vec<_>>() }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, optional: bool, ty: Type) -> Field {
        Field { name: name.into(), optional, ty, description: None }
    }

    #[test]
    fn object_types_become_closed_schemas_with_required_fields() {
        let ty = Type::Object {
            fields: vec![
                Field { description: Some("File to edit".into()), ..field("path", false, Type::String) },
                field("all", true, Type::Boolean),
                field("mode", false, Type::Union { of: vec![Type::Literal { value: json!("a") }, Type::Literal { value: json!("b") }] }),
                field("tags", true, Type::Array { items: Box::new(Type::String) }),
            ],
        };
        assert_eq!(
            ty.to_json_schema(),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to edit" },
                    "all": { "type": "boolean" },
                    "mode": { "enum": ["a", "b"] },
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["path", "mode"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn unions_drop_undefined_and_round_trip_through_json() {
        let ty = Type::Union { of: vec![Type::Number, Type::Undefined] };
        assert_eq!(ty.to_json_schema(), json!({ "type": "number" }));
        let mixed = Type::Union { of: vec![Type::Number, Type::Null] };
        assert_eq!(mixed.to_json_schema(), json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }));
        let json = serde_json::to_value(&mixed).unwrap();
        assert_eq!(json, json!({ "kind": "union", "of": [{ "kind": "number" }, { "kind": "null" }] }));
        assert_eq!(serde_json::from_value::<Type>(json).unwrap(), mixed);
    }
}
