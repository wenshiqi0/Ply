//! wendy core: what the kernel runs, described. No IO, no runtime.
//!
//! Important decisions:
//! - Every call in wendy is an IPO operation: input → process → output.
//!   A tool call is one; so is asking a human for permission. Operations
//!   nest (a tool calling another tool) and run concurrently, like fibers
//!   the kernel schedules.
//! - Plugins are tool wrappers: a plugin is a named set of tools, a tool is
//!   a name, a description, an input schema and the three phases (written
//!   in TypeScript). Payloads are opaque JSON; their shape is the plugin's
//!   business (the schema documents it), never typed here.
//! - Effects are not declared up front. What an operation changed is what
//!   the IO layer saw it do: every IO call, and every change a process it
//!   started made, is traced with the operation it belongs to.

pub mod ipo;
pub mod plugin;

pub use ipo::{Op, OpKind, OpRecord, Outcome, PHASES, Phase};
pub use plugin::{PluginSpec, ToolSpec};

/// Opaque data an operation takes or returns. Its fields are described by
/// the TS side.
pub type Payload = serde_json::Value;
