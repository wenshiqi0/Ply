//! One IPO operation: what it is, where it came from, how it ended.

use serde::{Deserialize, Serialize};

use crate::Payload;

/// The three phases every operation walks, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Normalize and check what came in.
    Input,
    /// Do the work; the only phase expected to cause effects.
    Process,
    /// Shape what goes back to the caller.
    Output,
}

pub const PHASES: [Phase; 3] = [Phase::Input, Phase::Process, Phase::Output];

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Input => "input",
            Phase::Process => "process",
            Phase::Output => "output",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    /// A tool of a plugin, called by the host or by another operation.
    Tool,
    /// A permission request: input is the request, process is whatever the
    /// host asks (a prompt, a dialog, a rule), output is the answer.
    Approval,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub id: String,
    /// The operation this one was started from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub kind: OpKind,
    /// Tool name, or `approval`.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Started without anyone waiting for its output (woken).
    #[serde(default)]
    pub detached: bool,
    pub input: Payload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    Ok { output: Payload },
    Err { error: String },
}

/// A finished operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpRecord {
    pub op: Op,
    pub outcome: Outcome,
    pub started_ms: u64,
    pub ended_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_run_input_process_output() {
        assert_eq!(PHASES.map(Phase::as_str), ["input", "process", "output"]);
    }

    #[test]
    fn records_round_trip_through_json() {
        let record = OpRecord {
            op: Op {
                id: "op-2".into(),
                parent: Some("op-1".into()),
                kind: OpKind::Tool,
                target: "edit".into(),
                plugin: Some("builtin".into()),
                detached: false,
                input: serde_json::json!({ "path": "a.txt" }),
            },
            outcome: Outcome::Err { error: "old_string not found".into() },
            started_ms: 1,
            ended_ms: 2,
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["outcome"]["status"], "err");
        assert_eq!(serde_json::from_value::<OpRecord>(json).unwrap(), record);
    }
}
