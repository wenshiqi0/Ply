//! Deciding whether one IO call may happen.
//!
//! A [`Gate`] answers per call. [`Policy`] is the standard gate: ordered path
//! rules, then a default per access kind, and an [`Approver`] for the calls a
//! rule says to ask about. How a human is asked (a TUI prompt, a dialog, an
//! approval run in the kernel) is the approver's business; the gate only
//! waits for the answer, so the IO call simply stays pending meanwhile.

use std::sync::{Arc, RwLock};

use crate::{Access, IoRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

#[async_trait::async_trait]
pub trait Gate: Send + Sync {
    async fn decide(&self, req: &IoRequest) -> Decision;
}

/// Lets everything through (still traced). For trusted hosts and tests.
pub struct AllowAll;

#[async_trait::async_trait]
impl Gate for AllowAll {
    async fn decide(&self, _req: &IoRequest) -> Decision {
        Decision::Allow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    Allow,
    Deny,
    Ask,
}

/// Applies to paths equal to or below `prefix`, for one access kind or both.
#[derive(Debug, Clone)]
pub struct Rule {
    pub access: Option<Access>,
    pub prefix: String,
    pub action: RuleAction,
}

impl Rule {
    pub fn new(access: Option<Access>, prefix: impl Into<String>, action: RuleAction) -> Self {
        Rule { access, prefix: prefix.into(), action }
    }

    fn matches(&self, access: Access, path: &str) -> bool {
        self.access.is_none_or(|a| a == access) && under(path, &self.prefix)
    }
}

/// `path` equals `prefix` or lies below it, compared by whole components.
pub(crate) fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// What a human (or an automated approver) said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Allow this one call.
    Once,
    /// Allow this call and any later call with the same access on this path.
    Always,
    Deny(String),
}

#[async_trait::async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, req: &IoRequest) -> Answer;
}

/// Ordered rules + per-access defaults + an approver for `Ask`.
pub struct Policy {
    rules: Vec<Rule>,
    remembered: RwLock<Vec<Rule>>,
    default_read: RuleAction,
    default_write: RuleAction,
    approver: Option<Arc<dyn Approver>>,
}

impl Default for Policy {
    /// Reads allowed, writes asked about.
    fn default() -> Self {
        Policy {
            rules: Vec::new(),
            remembered: RwLock::new(Vec::new()),
            default_read: RuleAction::Allow,
            default_write: RuleAction::Ask,
            approver: None,
        }
    }
}

impl Policy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn default_read(mut self, action: RuleAction) -> Self {
        self.default_read = action;
        self
    }

    pub fn default_write(mut self, action: RuleAction) -> Self {
        self.default_write = action;
        self
    }

    pub fn approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    fn action_for(&self, access: Access, path: &str) -> RuleAction {
        let remembered = self.remembered.read().expect("policy grants poisoned");
        remembered
            .iter()
            .chain(self.rules.iter())
            .find(|r| r.matches(access, path))
            .map(|r| r.action)
            .unwrap_or(match access {
                Access::Read => self.default_read,
                Access::Write => self.default_write,
            })
    }
}

#[async_trait::async_trait]
impl Gate for Policy {
    async fn decide(&self, req: &IoRequest) -> Decision {
        let mut targets = vec![req.path.as_str()];
        targets.extend(req.to.as_deref());

        let mut ask = false;
        for path in &targets {
            match self.action_for(req.access, path) {
                RuleAction::Allow => {}
                RuleAction::Deny => return Decision::Deny(format!("policy denies {path}")),
                RuleAction::Ask => ask = true,
            }
        }
        if !ask {
            return Decision::Allow;
        }
        let Some(approver) = &self.approver else {
            return Decision::Deny("needs approval, but no approver is configured".into());
        };
        match approver.approve(req).await {
            Answer::Once => Decision::Allow,
            Answer::Always => {
                let mut remembered = self.remembered.write().expect("policy grants poisoned");
                for path in targets {
                    remembered.push(Rule::new(Some(req.access), path, RuleAction::Allow));
                }
                Decision::Allow
            }
            Answer::Deny(reason) => Decision::Deny(reason),
        }
    }
}
