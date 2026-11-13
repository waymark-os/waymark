// SPDX-License-Identifier: MIT OR Apache-2.0

//! Budgets for model and tool calls (design 5.3 and L17).
//!
//! A budget is checked only where a program calls a model or starts a
//! command, so ordinary code pays nothing for it. The host gives each
//! attempt a budget (`waymark eval --budget ...`, or the Gateway attempt's
//! resource limits); `model.limit(...)` narrows the model's share of it.
//!
//! Running out of any budget ends the attempt. The current call does not
//! happen, `try` cannot catch the stop, later evaluations in the same
//! session do not run, and the host returns an `unfinished` result built
//! from the budget's own record of recent calls. A program that wants to
//! report partial work stops early by reading `model.remaining`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};

/// Limit names the host understands, as in the Gateway's attempt
/// `resource_limits`.
pub(crate) const HOST_LIMIT_NAMES: [&str; 5] = [
    "model_calls",
    "input_tokens",
    "output_tokens",
    "tool_calls",
    "wall_time_ms",
];

/// Keywords of `model.limit(...)`.
pub(crate) const MODEL_LIMIT_NAMES: [&str; 3] = ["calls", "input_tokens", "output_tokens"];

/// Recent metered calls kept for the `unfinished` result.
const RECENT_CALLS: usize = 8;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct HostLimits {
    pub(crate) model_calls: Option<u64>,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) tool_calls: Option<u64>,
    pub(crate) wall_time_ms: Option<u64>,
}

impl HostLimits {
    /// Parse `model_calls=40,output_tokens=60000`.
    pub(crate) fn parse(text: &str) -> Result<HostLimits, String> {
        let mut limits = HostLimits::default();
        for part in text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let Some((name, value)) = part.split_once('=') else {
                return Err(format!(
                    "budget entry `{part}` must be name=value, such as model_calls=40"
                ));
            };
            let value = value.trim().parse::<u64>().ok().filter(|value| *value > 0);
            let Some(value) = value else {
                return Err(format!(
                    "budget `{}` must be a whole number greater than zero",
                    name.trim()
                ));
            };
            limits.set(name.trim(), value)?;
        }
        if limits.is_empty() {
            return Err(format!(
                "a budget needs at least one limit: {}",
                HOST_LIMIT_NAMES.join(", ")
            ));
        }
        Ok(limits)
    }

    /// The limits this module knows from a Gateway attempt's
    /// `resource_limits`; other limits belong to the Gateway.
    pub(crate) fn from_resource_limits(map: &serde_json::Map<String, JsonValue>) -> HostLimits {
        let mut limits = HostLimits::default();
        for name in HOST_LIMIT_NAMES {
            let value = match map.get(name) {
                Some(JsonValue::String(text)) => text.trim().parse::<u64>().ok(),
                Some(JsonValue::Number(number)) => number.as_u64(),
                _ => None,
            };
            if let Some(value) = value.filter(|value| *value > 0) {
                let _ = limits.set(name, value);
            }
        }
        limits
    }

    fn set(&mut self, name: &str, value: u64) -> Result<(), String> {
        let slot = match name {
            "model_calls" => &mut self.model_calls,
            "input_tokens" => &mut self.input_tokens,
            "output_tokens" => &mut self.output_tokens,
            "tool_calls" => &mut self.tool_calls,
            "wall_time_ms" => &mut self.wall_time_ms,
            other => {
                return Err(format!(
                    "unknown budget `{other}`; expected {}",
                    HOST_LIMIT_NAMES.join(", ")
                ))
            }
        };
        *slot = min_limit(*slot, Some(value));
        Ok(())
    }

    /// The tighter of two budgets, limit by limit.
    fn tighten(self, other: HostLimits) -> HostLimits {
        HostLimits {
            model_calls: min_limit(self.model_calls, other.model_calls),
            input_tokens: min_limit(self.input_tokens, other.input_tokens),
            output_tokens: min_limit(self.output_tokens, other.output_tokens),
            tool_calls: min_limit(self.tool_calls, other.tool_calls),
            wall_time_ms: min_limit(self.wall_time_ms, other.wall_time_ms),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == HostLimits::default()
    }

    fn to_json(self) -> JsonValue {
        let mut map = serde_json::Map::new();
        for (name, value) in [
            ("model_calls", self.model_calls),
            ("input_tokens", self.input_tokens),
            ("output_tokens", self.output_tokens),
            ("tool_calls", self.tool_calls),
            ("wall_time_ms", self.wall_time_ms),
        ] {
            if let Some(value) = value {
                map.insert(name.to_string(), json!(value));
            }
        }
        JsonValue::Object(map)
    }
}

fn min_limit(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct ModelLimits {
    pub(crate) calls: Option<u64>,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
}

/// Tokens one model call reports. `cached_input_tokens` is the part of
/// `input_tokens` the provider served from its prompt cache.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct ModelCallTokens {
    pub(crate) input_tokens: u64,
    pub(crate) cached_input_tokens: u64,
    pub(crate) output_tokens: u64,
}

#[derive(Clone, Copy, Default, Debug)]
struct ModelUsed {
    calls: u64,
    input_tokens: u64,
    /// The part of `input_tokens` the provider served from its prompt cache.
    cached_input_tokens: u64,
    output_tokens: u64,
}

/// One model budget. The attempt's meter is the root; each
/// `model.limit(...)` adds a child whose calls are also charged to every
/// meter above it.
pub(crate) struct Meter {
    limits: Cell<ModelLimits>,
    used: Cell<ModelUsed>,
    parent: Option<Rc<Meter>>,
}

impl Meter {
    fn root() -> Rc<Meter> {
        Rc::new(Meter {
            limits: Cell::new(ModelLimits::default()),
            used: Cell::new(ModelUsed::default()),
            parent: None,
        })
    }

    /// A narrowed meter for `model.limit(...)`.
    pub(crate) fn narrow(self: &Rc<Meter>, limits: ModelLimits) -> Rc<Meter> {
        Rc::new(Meter {
            limits: Cell::new(limits),
            used: Cell::new(ModelUsed::default()),
            parent: Some(self.clone()),
        })
    }

    fn is_root(&self) -> bool {
        self.parent.is_none()
    }

    fn chain(self: &Rc<Meter>) -> impl Iterator<Item = &Meter> {
        std::iter::successors(Some(self.as_ref()), |meter| meter.parent.as_deref())
    }

    /// `model.usage`: what this model value has spent.
    pub(crate) fn usage_json(&self) -> JsonValue {
        let used = self.used.get();
        json!({
            "calls": used.calls,
            "input_tokens": used.input_tokens,
            "cached_input_tokens": used.cached_input_tokens,
            "output_tokens": used.output_tokens,
        })
    }
}

/// A metered limit that ran out, or would run out.
#[derive(Clone, Debug)]
pub(crate) struct Exhausted {
    /// The limit's name as the program or host wrote it.
    name: &'static str,
    /// "attempt" or "model.limit".
    scope: &'static str,
    limit: u64,
    used: u64,
}

impl Exhausted {
    fn message(&self) -> String {
        match (self.scope, self.name) {
            ("attempt", "wall_time_ms") => format!(
                "the attempt's wall-clock budget ({} ms) is used up",
                self.limit
            ),
            ("attempt", name) => format!(
                "the attempt's {name} budget is used up ({} of {})",
                self.used, self.limit
            ),
            (_, name) => format!(
                "a model.limit({name}={}) budget is used up ({} of {})",
                self.limit, self.used, self.limit
            ),
        }
    }
}

/// The attempt's budget, shared by every evaluation in a session.
pub(crate) struct Budget {
    root: Rc<Meter>,
    started: Instant,
    /// Limits from `waymark eval --budget`.
    configured: Cell<HostLimits>,
    /// Effective host limits once the Gateway attempt has been read.
    limits: Cell<HostLimits>,
    deadline: Cell<Option<Instant>>,
    loaded: Cell<bool>,
    sources: RefCell<Vec<&'static str>>,
    load_error: RefCell<Option<String>>,
    tool_calls: Cell<u64>,
    recent: RefCell<VecDeque<JsonValue>>,
    stopped: RefCell<Option<JsonValue>>,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            root: Meter::root(),
            started: Instant::now(),
            configured: Cell::new(HostLimits::default()),
            limits: Cell::new(HostLimits::default()),
            deadline: Cell::new(None),
            loaded: Cell::new(false),
            sources: RefCell::new(Vec::new()),
            load_error: RefCell::new(None),
            tool_calls: Cell::new(0),
            recent: RefCell::new(VecDeque::new()),
            stopped: RefCell::new(None),
        }
    }
}

/// What the Gateway says about the current attempt's budget.
pub(crate) struct AttemptLimits {
    pub(crate) limits: HostLimits,
    /// Milliseconds left before the attempt's deadline, if it has one.
    pub(crate) remaining_ms: Option<u64>,
}

impl Budget {
    /// The root meter, which the program's `model` value charges.
    pub(crate) fn root(&self) -> Rc<Meter> {
        self.root.clone()
    }

    /// Limits given on the command line. They tighten whatever the Gateway
    /// attempt allows.
    pub(crate) fn configure(&self, limits: HostLimits) {
        self.configured.set(self.configured.get().tighten(limits));
        self.loaded.set(false);
    }

    /// Read the host's limits once, on first use. `attempt` asks the
    /// Gateway for the current attempt's limits, when there is one.
    pub(crate) fn ensure_loaded(
        &self,
        attempt: impl FnOnce() -> Option<Result<AttemptLimits, String>>,
    ) {
        if self.loaded.replace(true) {
            return;
        }
        let configured = self.configured.get();
        let mut limits = configured;
        let mut sources = Vec::new();
        if !configured.is_empty() {
            sources.push("command line");
        }
        let mut deadline = configured
            .wall_time_ms
            .map(|ms| self.started + Duration::from_millis(ms));
        match attempt() {
            Some(Ok(attempt)) => {
                if !attempt.limits.is_empty() || attempt.remaining_ms.is_some() {
                    sources.push("gateway attempt");
                }
                limits = limits.tighten(attempt.limits);
                if let Some(remaining) = attempt.remaining_ms {
                    let gateway_deadline = Instant::now() + Duration::from_millis(remaining);
                    deadline = Some(match deadline {
                        Some(existing) => existing.min(gateway_deadline),
                        None => gateway_deadline,
                    });
                }
            }
            Some(Err(error)) => *self.load_error.borrow_mut() = Some(error),
            None => {}
        }
        self.limits.set(limits);
        self.deadline.set(deadline);
        *self.sources.borrow_mut() = sources;
        self.root.limits.set(ModelLimits {
            calls: limits.model_calls,
            input_tokens: limits.input_tokens,
            output_tokens: limits.output_tokens,
        });
    }

    /// The `unfinished` record, once a budget has run out.
    pub(crate) fn stopped(&self) -> Option<JsonValue> {
        self.stopped.borrow().clone()
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.borrow().is_some()
    }

    /// The stop message, for the error that unwinds the program.
    pub(crate) fn stop_message(&self) -> Option<String> {
        self.stopped
            .borrow()
            .as_ref()
            .and_then(|record| record.get("message"))
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    }

    fn remaining_ms(&self) -> Option<u64> {
        self.deadline.get().map(|deadline| {
            u64::try_from(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )
            .unwrap_or(u64::MAX)
        })
    }

    fn check_clock(&self) -> Result<(), Exhausted> {
        match (self.remaining_ms(), self.limits.get().wall_time_ms) {
            (Some(0), limit) => Err(Exhausted {
                name: "wall_time_ms",
                scope: "attempt",
                limit: limit.unwrap_or_default(),
                used: self.elapsed_ms(),
            }),
            _ => Ok(()),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Check every meter from `meter` up to the attempt before a model call.
    /// Returns the output-token cap the call must respect, if any.
    pub(crate) fn before_model_call(&self, meter: &Rc<Meter>) -> Result<Option<u64>, String> {
        if let Some(message) = self.stop_message() {
            return Err(message);
        }
        let mut cap: Option<u64> = None;
        let mut result = self.check_clock();
        for node in meter.chain() {
            if result.is_err() {
                break;
            }
            let limits = node.limits.get();
            let used = node.used.get();
            let scope = if node.is_root() {
                "attempt"
            } else {
                "model.limit"
            };
            let names = if node.is_root() {
                ["model_calls", "input_tokens", "output_tokens"]
            } else {
                MODEL_LIMIT_NAMES
            };
            for (name, limit, spent) in [
                (names[0], limits.calls, used.calls),
                (names[1], limits.input_tokens, used.input_tokens),
                (names[2], limits.output_tokens, used.output_tokens),
            ] {
                if let Some(limit) = limit.filter(|limit| spent >= *limit) {
                    result = Err(Exhausted {
                        name,
                        scope,
                        limit,
                        used: spent,
                    });
                    break;
                }
            }
            if let Some(limit) = limits.output_tokens {
                cap = min_limit(cap, Some(limit.saturating_sub(used.output_tokens)));
            }
        }
        match result {
            Ok(()) => Ok(cap),
            Err(exhausted) => Err(self.stop(exhausted)),
        }
    }

    /// Charge one model call, made or attempted, to every meter in the chain.
    pub(crate) fn after_model_call(
        &self,
        meter: &Rc<Meter>,
        tokens: ModelCallTokens,
        ok: bool,
    ) {
        let ModelCallTokens {
            input_tokens,
            cached_input_tokens,
            output_tokens,
        } = tokens;
        for node in meter.chain() {
            let mut used = node.used.get();
            used.calls = used.calls.saturating_add(1);
            used.input_tokens = used.input_tokens.saturating_add(input_tokens);
            used.cached_input_tokens = used.cached_input_tokens.saturating_add(cached_input_tokens);
            used.output_tokens = used.output_tokens.saturating_add(output_tokens);
            node.used.set(used);
        }
        self.remember(json!({
            "kind": "model",
            "ok": ok,
            "limited": !meter.is_root(),
            "input_tokens": input_tokens,
            "cached_input_tokens": cached_input_tokens,
            "output_tokens": output_tokens,
        }));
    }

    /// Check the attempt's tool-call and wall-clock budgets before a
    /// command. Returns the milliseconds left, to bound the command's
    /// timeout.
    pub(crate) fn before_tool_call(&self) -> Result<Option<u64>, String> {
        if let Some(message) = self.stop_message() {
            return Err(message);
        }
        let mut result = self.check_clock();
        if result.is_ok() {
            if let Some(limit) = self.limits.get().tool_calls {
                let used = self.tool_calls.get();
                if used >= limit {
                    result = Err(Exhausted {
                        name: "tool_calls",
                        scope: "attempt",
                        limit,
                        used,
                    });
                }
            }
        }
        match result {
            Ok(()) => Ok(self.remaining_ms()),
            Err(exhausted) => Err(self.stop(exhausted)),
        }
    }

    /// Charge one command and remember what it did.
    pub(crate) fn after_tool_call(&self, summary: JsonValue) {
        self.tool_calls.set(self.tool_calls.get().saturating_add(1));
        self.remember(summary);
    }

    fn remember(&self, entry: JsonValue) {
        let mut recent = self.recent.borrow_mut();
        if recent.len() >= RECENT_CALLS {
            recent.pop_front();
        }
        recent.push_back(entry);
    }

    /// Keep the end of what the program printed, once it has stopped.
    pub(crate) fn attach_stdout(&self, stdout: &str) {
        const MAX_STDOUT_CHARS: usize = 4000;
        let mut stopped = self.stopped.borrow_mut();
        let Some(record) = stopped.as_mut() else {
            return;
        };
        if stdout.is_empty() || record.get("stdout").is_some() {
            return;
        }
        let skip = stdout.chars().count().saturating_sub(MAX_STDOUT_CHARS);
        record["stdout"] = JsonValue::String(stdout.chars().skip(skip).collect());
    }

    /// End the attempt: build the `unfinished` record and keep it.
    fn stop(&self, exhausted: Exhausted) -> String {
        let message = exhausted.message();
        let record = json!({
            "reason": "budget",
            "message": message,
            "limit": {
                "name": exhausted.name,
                "scope": exhausted.scope,
                "limit": exhausted.limit,
                "used": exhausted.used,
            },
            "usage": self.usage_json(),
            "limits": self.limits.get().to_json(),
            "recent": self.recent.borrow().iter().cloned().collect::<Vec<_>>(),
            "next": "Give the attempt a larger budget, or have the program read model.remaining and stop early with a partial result.",
        });
        *self.stopped.borrow_mut() = Some(record);
        message
    }

    fn usage_json(&self) -> JsonValue {
        let used = self.root.used.get();
        json!({
            "model_calls": used.calls,
            "input_tokens": used.input_tokens,
            "output_tokens": used.output_tokens,
            "tool_calls": self.tool_calls.get(),
            "elapsed_ms": self.elapsed_ms(),
        })
    }

    /// `model.remaining` for a model value: the smallest amount left on
    /// any meter in its chain, `None` where nothing limits it.
    pub(crate) fn remaining_json(&self, meter: &Rc<Meter>) -> JsonValue {
        let mut calls = None;
        let mut input_tokens = None;
        let mut output_tokens = None;
        for node in meter.chain() {
            let limits = node.limits.get();
            let used = node.used.get();
            calls = min_limit(
                calls,
                limits.calls.map(|limit| limit.saturating_sub(used.calls)),
            );
            input_tokens = min_limit(
                input_tokens,
                limits
                    .input_tokens
                    .map(|limit| limit.saturating_sub(used.input_tokens)),
            );
            output_tokens = min_limit(
                output_tokens,
                limits
                    .output_tokens
                    .map(|limit| limit.saturating_sub(used.output_tokens)),
            );
        }
        json!({
            "calls": calls,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "tool_calls": self
                .limits
                .get()
                .tool_calls
                .map(|limit| limit.saturating_sub(self.tool_calls.get())),
            "seconds": self.remaining_ms().map(|ms| ms / 1000),
        })
    }

    /// `diagnostics.budget`, when the attempt has a budget.
    pub(crate) fn diagnostics(&self) -> Option<JsonValue> {
        let limits = self.limits.get();
        let load_error = self.load_error.borrow().clone();
        if limits.is_empty() && load_error.is_none() {
            return None;
        }
        let mut record = json!({
            "limits": limits.to_json(),
            "used": self.usage_json(),
            "source": *self.sources.borrow(),
        });
        if let Some(error) = load_error {
            record["load_error"] = JsonValue::String(error);
        }
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::{Budget, HostLimits, ModelCallTokens, ModelLimits};

    fn tokens(input_tokens: u64, output_tokens: u64) -> ModelCallTokens {
        ModelCallTokens {
            input_tokens,
            cached_input_tokens: input_tokens / 2,
            output_tokens,
        }
    }

    #[test]
    fn budgets_parse_and_reject_unknown_names() {
        let limits = HostLimits::parse("model_calls=3, tool_calls=10").unwrap();
        assert_eq!(limits.model_calls, Some(3));
        assert_eq!(limits.tool_calls, Some(10));
        assert!(HostLimits::parse("calls=3")
            .unwrap_err()
            .contains("unknown budget `calls`"));
        assert!(HostLimits::parse("model_calls=0")
            .unwrap_err()
            .contains("greater than zero"));
        assert!(HostLimits::parse("").is_err());
    }

    #[test]
    fn narrowed_meters_charge_the_whole_chain() {
        let budget = Budget::default();
        budget.configure(HostLimits::parse("model_calls=5,output_tokens=100").unwrap());
        budget.ensure_loaded(|| None);
        let child = budget.root().narrow(ModelLimits {
            calls: Some(2),
            input_tokens: None,
            output_tokens: Some(30),
        });
        assert_eq!(budget.before_model_call(&child).unwrap(), Some(30));
        budget.after_model_call(&child, tokens(10, 20), true);
        assert_eq!(budget.before_model_call(&child).unwrap(), Some(10));
        assert_eq!(budget.root().usage_json()["calls"], 1);
        assert_eq!(child.usage_json()["cached_input_tokens"], 5);
        assert_eq!(budget.remaining_json(&budget.root())["output_tokens"], 80);
        budget.after_model_call(&child, tokens(10, 5), true);
        let message = budget.before_model_call(&child).unwrap_err();
        assert!(message.contains("model.limit(calls=2)"), "{message}");
        let stopped = budget.stopped().unwrap();
        assert_eq!(stopped["limit"]["scope"], "model.limit");
        assert_eq!(stopped["usage"]["model_calls"], 2);
        // Once stopped, every metered call stops, including the root's.
        assert!(budget.before_model_call(&budget.root()).is_err());
        assert!(budget.before_tool_call().is_err());
    }
}
