//! wendy core: what the kernel runs, described. No IO, no runtime.
//!
//! Important decisions:
//! - A plugin is one function, standing alone: `handle(input) → output`.
//!   Its name is its directory, its description is the JSDoc on `handle`,
//!   and the description of its input and output is `handle`'s TypeScript
//!   signature, read as a [`Type`]. Plugins declare nothing else; checking
//!   input is the plugin's own business.
//! - Every execution of a plugin is a [`Fiber`]. A plugin reaches another
//!   only by awaiting it (`await tools.read(...)`): `then` is the call, and
//!   the awaited execution is a child fiber.
//! - What is recorded is fibers: their input, how they exited, and which
//!   fiber awaited which ([`FiberEvent`], delivered to [`FiberHook`]s).
//! - These types are the only definition. The kernel builds them, hooks
//!   receive them, records are their serialization.

pub mod fiber;
pub mod plugin;
pub mod types;

pub use fiber::{Exit, Fiber, FiberEvent, FiberHook};
pub use plugin::{PluginSpec, ToolSchema};
pub use types::{Field, Type};

/// Opaque data a fiber takes or returns.
pub type Payload = serde_json::Value;
