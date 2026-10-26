//! Host/UI commands are separate from model tools. Registrations belong to
//! native plugin realms, and each invocation is a cancellable child fiber.
use crate::{FiberInvocation, Kernel, KernelError, LoadState, PluginSpec};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use ply_jsc::Caller;

#[derive(Debug, Clone, Serialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub plugin: String,
}

#[derive(Clone)]
struct Registration {
    spec: CommandSpec,
    callback: u64,
}

#[derive(Default)]
pub(super) struct Commands {
    entries: BTreeMap<String, Registration>,
}

impl Commands {
    pub(super) fn remove_plugin(&mut self, plugin: &str) {
        self.entries.retain(|_, entry| entry.spec.plugin != plugin);
    }
}

impl Kernel {
    pub fn commands(&self) -> Vec<CommandSpec> {
        self.inner
            .commands
            .lock()
            .expect("command table poisoned")
            .entries
            .values()
            .map(|r| r.spec.clone())
            .collect()
    }

    /// Start a registered command as a top-level fiber; not a model tool.
    pub async fn run_command(&self, name: &str, input: Value) -> Result<Value, KernelError> {
        self.invoke_command(name, input, None).await
    }

    async fn invoke_command(
        &self,
        name: &str,
        input: Value,
        parent: Option<String>,
    ) -> Result<Value, KernelError> {
        let registration = self
            .inner
            .commands
            .lock()
            .expect("command table poisoned")
            .entries
            .get(name)
            .cloned()
            .ok_or_else(|| KernelError::Fiber(format!("no registered command named `{name}`")))?;
        self.start_invocation(
            &registration.spec.plugin,
            input,
            parent,
            false,
            self.next_fiber_id(),
            FiberInvocation::Command {
                callback: registration.callback,
                name: name.into(),
            },
        )
        .await
    }

    pub(super) async fn command_op(
        &self,
        caller: Caller,
        op: &str,
        args: Value,
    ) -> Result<Value, String> {
        if op == "kernel.command_list" {
            return serde_json::to_value(self.commands()).map_err(|e| e.to_string());
        }
        if op == "kernel.command_run" {
            if caller.loading {
                return Err("run commands after onLoad; register during onLoad".into());
            }
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("missing command name")?;
            return self
                .invoke_command(
                    name,
                    args.get("input").cloned().unwrap_or(Value::Null),
                    caller.fiber,
                )
                .await
                .map_err(|e| e.to_string());
        }
        if op != "kernel.command_register" {
            return Err(format!("unknown command op {op}"));
        }
        let plugin = {
            let plugins = self.inner.plugins.read().expect("plugin table poisoned");
            let plugin = plugins
                .iter()
                .find(|p| p.realm == caller.realm_id)
                .ok_or("commands require a loaded plugin realm")?;
            if !caller.loading || !matches!(*plugin.state.borrow(), LoadState::Loading) {
                return Err("register commands during onLoad".into());
            }
            plugin.spec.name.clone()
        };
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| PluginSpec::valid_name(s))
            .ok_or("command name must match [a-zA-Z0-9_-]{1,64}")?;
        if ["tools", "commands", "call", "mcp", "run", "help"].contains(&name) {
            return Err(format!("command name `{name}` is reserved by the CLI"));
        }
        let description = match args.get("description") {
            None => "",
            Some(Value::String(s)) if s.len() <= 4096 => s,
            _ => return Err("command description must be a string of at most 4096 bytes".into()),
        };
        let callback = args
            .get("callback")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
            .ok_or("invalid command callback")?;
        let mut commands = self.inner.commands.lock().expect("command table poisoned");
        if commands.entries.contains_key(name) {
            return Err(format!("command `{name}` is already registered"));
        }
        commands.entries.insert(
            name.into(),
            Registration {
                spec: CommandSpec {
                    name: name.into(),
                    description: description.into(),
                    plugin,
                },
                callback,
            },
        );
        Ok(json!(null))
    }
}
