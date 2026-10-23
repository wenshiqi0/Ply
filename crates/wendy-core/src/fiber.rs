//! Fibers: one execution of a plugin, where it came from, how it ended.

use serde::{Deserialize, Serialize};

use crate::Payload;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fiber {
    pub id: String,
    /// The fiber whose `await` started this one; none for the host's calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub plugin: String,
    /// Started by the host with nobody awaiting it.
    #[serde(default)]
    pub detached: bool,
    pub input: Payload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Exit {
    Done {
        output: Payload,
    },
    Failed {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stack: Option<String>,
    },
}

/// What happens to a fiber, as hooks see it and records store it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum FiberEvent {
    Spawn {
        /// Milliseconds since the Unix epoch.
        at_ms: u64,
        fiber: Fiber,
    },
    Exit {
        at_ms: u64,
        id: String,
        exit: Exit,
        /// How long the fiber ran.
        ms: u64,
    },
}

/// Receives every fiber event, synchronously, in order.
pub trait FiberHook: Send + Sync {
    fn on_event(&self, event: &FiberEvent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_serialize_as_tagged_records_and_round_trip() {
        let spawn = FiberEvent::Spawn {
            at_ms: 1,
            fiber: Fiber { id: "f2".into(), parent: Some("f1".into()), plugin: "read".into(), detached: false, input: json!({ "path": "a" }) },
        };
        let exit = FiberEvent::Exit {
            at_ms: 4,
            id: "f2".into(),
            exit: Exit::Failed { error: "Error: ENOENT".into(), stack: None },
            ms: 3,
        };
        assert_eq!(serde_json::to_value(&spawn).unwrap()["event"], "spawn");
        assert_eq!(serde_json::to_value(&exit).unwrap()["exit"]["status"], "failed");
        for event in [spawn, exit] {
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(serde_json::from_value::<FiberEvent>(json).unwrap(), event);
        }
    }
}
