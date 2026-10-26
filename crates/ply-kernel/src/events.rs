//! Plugin-owned events. Callbacks stay in their own realm and run as child fibers.
use crate::{FiberEvent, FiberInvocation, Kernel, LoadState, PluginSpec, now_ms};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;
use ply_jsc::Caller;

#[derive(Clone)]
struct Subscription {
    id: u64,
    plugin: String,
    callback: u64,
}
struct Definition {
    plugin: String,
    subscriptions: Vec<Subscription>,
}
#[derive(Default)]
pub(super) struct Events {
    definitions: BTreeMap<String, Definition>,
    next_subscription: u64,
}

impl Events {
    pub(super) fn remove_plugin(&mut self, plugin: &str) {
        self.definitions
            .retain(|_, definition| definition.plugin != plugin);
        for definition in self.definitions.values_mut() {
            definition.subscriptions.retain(|s| s.plugin != plugin);
        }
    }
}

impl Kernel {
    fn event_owner(&self, caller: &Caller, registering: bool) -> Result<String, String> {
        let plugins = self.inner.plugins.read().expect("plugin table poisoned");
        let plugin = plugins
            .iter()
            .find(|p| p.realm == caller.realm_id)
            .ok_or("events require a loaded plugin realm")?;
        if registering && (!caller.loading || !matches!(*plugin.state.borrow(), LoadState::Loading))
        {
            return Err("declare and subscribe to events during onLoad".into());
        }
        Ok(plugin.spec.name.clone())
    }

    pub(super) async fn event_op(
        &self,
        caller: Caller,
        op: &str,
        args: Value,
    ) -> Result<Value, String> {
        if op == "kernel.event_list" {
            let events = self.inner.events.lock().expect("event table poisoned");
            return Ok(Value::Array(events.definitions.iter().map(|(name, d)| json!({ "name": name, "plugin": d.plugin, "subscribers": d.subscriptions.len() })).collect()));
        }
        let owner = self.event_owner(
            &caller,
            matches!(op, "kernel.event_define" | "kernel.event_on"),
        )?;
        if op == "kernel.event_off" {
            let id = args
                .get("subscription")
                .and_then(Value::as_u64)
                .ok_or("invalid event subscription")?;
            let mut events = self.inner.events.lock().expect("event table poisoned");
            for definition in events.definitions.values_mut() {
                if let Some(index) = definition.subscriptions.iter().position(|s| s.id == id) {
                    if definition.subscriptions[index].plugin != owner {
                        return Err("subscription belongs to another plugin".into());
                    }
                    definition.subscriptions.remove(index);
                    return Ok(json!(true));
                }
            }
            return Ok(json!(false));
        }
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or("missing event name")?;
        if op == "kernel.event_define" {
            if !PluginSpec::valid_name(name) {
                return Err("event local name must match [a-zA-Z0-9_-]{1,64}".into());
            }
            let name = format!("{owner}.{name}");
            let mut events = self.inner.events.lock().expect("event table poisoned");
            if events.definitions.contains_key(&name) {
                return Err(format!("event {name} is already declared"));
            }
            events.definitions.insert(
                name.clone(),
                Definition {
                    plugin: owner,
                    subscriptions: Vec::new(),
                },
            );
            return Ok(json!(name));
        }
        if op == "kernel.event_on" {
            let callback = args
                .get("callback")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
                .ok_or("invalid event callback")?;
            let mut events = self.inner.events.lock().expect("event table poisoned");
            if !events.definitions.contains_key(name) {
                return Err(format!(
                    "unknown event {name}; await the provider's plugins.<plugin>.loaded before subscribing"
                ));
            }
            let id = events
                .next_subscription
                .checked_add(1)
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or("event subscription IDs exhausted")?;
            events.next_subscription = id;
            events
                .definitions
                .get_mut(name)
                .unwrap()
                .subscriptions
                .push(Subscription {
                    id,
                    plugin: owner,
                    callback,
                });
            return Ok(json!(id));
        }
        if op != "kernel.event_emit" {
            return Err(format!("unknown event op {op}"));
        }
        if caller.loading {
            return Err("emit events after onLoad; onLoad declares and subscribes only".into());
        }
        let subscriptions = {
            let events = self.inner.events.lock().expect("event table poisoned");
            let definition = events
                .definitions
                .get(name)
                .ok_or_else(|| format!("unknown event {name}"))?;
            if definition.plugin != owner {
                return Err("only an event's owning plugin can emit it".into());
            }
            definition.subscriptions.clone()
        };
        if let Some(fiber) = &caller.fiber {
            if !self
                .inner
                .fibers
                .lock()
                .expect("fiber table poisoned")
                .contains_key(fiber)
            {
                return Err("the emitting fiber has finished".into());
            }
        }
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
            .ok_or("event timeout_ms must be a positive safe integer")?;
        let id = format!(
            "{}-event-{}",
            self.inner.run,
            self.inner.next_event.fetch_add(1, Ordering::Relaxed)
        );
        let data = args.get("data").cloned().unwrap_or(Value::Null);
        let envelope = json!({ "id": id, "name": name, "source": { "plugin": owner, "fiber": caller.fiber }, "data": data });
        self.emit(&FiberEvent::Plugin {
            at_ms: now_ms(),
            id,
            name: name.into(),
            plugin: owner,
            fiber: caller.fiber.clone(),
            data,
        });
        let dispatch = async {
            let mut results = Vec::new();
            for subscription in subscriptions {
                let invocation = FiberInvocation::Event {
                    callback: subscription.callback,
                    name: name.into(),
                };
                let result = self
                    .start_invocation(
                        &subscription.plugin,
                        envelope.clone(),
                        caller.fiber.clone(),
                        false,
                        self.next_fiber_id(),
                        invocation,
                    )
                    .await
                    .map_err(|error| {
                        format!("event {name}, listener {}: {error}", subscription.plugin)
                    })?;
                results.push(result);
            }
            Ok(Value::Array(results))
        };
        tokio::time::timeout(Duration::from_millis(timeout_ms), dispatch)
            .await
            .map_err(|_| format!("event {name} exceeded its {timeout_ms}ms deadline"))?
    }
}
