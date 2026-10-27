//! Fibers: one execution of a plugin, where it came from and how it ended.

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
    /// Present for an event-listener invocation, rather than onHandle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
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
    Cancelled {
        reason: String,
    },
    #[serde(rename = "budget_exceeded")]
    BudgetExceeded {
        resource: BudgetResource,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetResource { Cpu, Memory }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus { Started, Done, Failed, TimedOut, Cancelled }

/// What happens to a fiber, as hooks see it and records store it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum FiberEvent {
    /// An event emitted by its owning plugin. Identity is supplied by the kernel.
    Plugin {
        at_ms: u64,
        id: String,
        name: String,
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fiber: Option<String>,
        data: Payload,
    },
    /// Runtime cleanup is independent of the owner's already-emitted exit.
    Cleanup {
        at_ms: u64,
        id: u64,
        realm: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
        status: CleanupStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
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
            fiber: Fiber { id: "f2".into(), parent: Some("f1".into()), plugin: "read".into(), detached: false, input: json!({ "path": "a" }), event: None },
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
    #[test]
    fn cancellation_and_budget_exits_have_distinct_round_trip_states() {
        for (exit, status) in [
            (Exit::Cancelled { reason: "requested".into() }, "cancelled"),
            (Exit::BudgetExceeded { resource: BudgetResource::Cpu, error: "CPU limit".into() }, "budget_exceeded"),
            (Exit::BudgetExceeded { resource: BudgetResource::Memory, error: "memory limit".into() }, "budget_exceeded"),
        ] {
            let value = serde_json::to_value(&exit).unwrap();
            assert_eq!(value["status"], status);
            assert_eq!(serde_json::from_value::<Exit>(value).unwrap(), exit);
        }
    }

}
