//! Test fixture: a compact description of an event that is translated into the
//! real `View` the app consumes. It keeps the row-model tests readable without
//! reintroducing a production translation layer.

#![allow(dead_code)]

use std::cell::RefCell;
use std::time::Instant;

use kelir::features::herdr::State as HerdrState;
use kelir::ipc::Response;
use kelir::log::event::{
    CallInfo, CallSettle, CallStart, EventBody, LogEvent, Note, Payload, Usage,
};
use kelir::tui::{App, Live, Phase, View};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Ev {
    TurnStarted {
        turn: u32,
        provider: String,
        model: String,
        limit: Option<u32>,
    },
    Step {
        step: u32,
        provider: String,
        model: String,
        turn_steps: u32,
        limit: Option<u32>,
    },
    Paused {
        steps: u32,
    },
    Delta(String),
    Usage {
        prompt: u64,
        completion: u64,
        cached: u64,
    },
    Assistant {
        text: String,
    },
    ToolStarted {
        name: String,
        input: Value,
    },
    ToolSettled {
        name: String,
        input: Value,
        outcome: String,
        exit_code: Option<i32>,
        elapsed_ms: u64,
        bytes: u64,
        lines: u64,
        spilled: bool,
        out_path: String,
        tail: String,
    },
    Info(String),
    Error(String),
    Retrying {
        line: String,
        until: Instant,
        status: Option<u16>,
        attempt: u32,
    },
    RetryingDone,
    Reply(Response),
    Herdr {
        state: HerdrState,
    },
    TurnEnded {
        outcome: String,
        final_text: String,
    },
}

pub trait ApplyEv {
    fn push(&mut self, ev: Ev);
}

impl ApplyEv for App {
    fn push(&mut self, ev: Ev) {
        let view = match ev {
            Ev::TurnStarted {
                provider, model, ..
            } => {
                self.apply(View::Log(LogEvent::new(
                    1,
                    EventBody::StepStarted {
                        step: 1,
                        provider,
                        model,
                        request_hash: String::new(),
                    },
                )));
                self.status.turn_steps = 0;
                return;
            }
            Ev::Step {
                step,
                provider,
                model,
                ..
            } => View::Log(LogEvent::new(
                1,
                EventBody::StepStarted {
                    step,
                    provider,
                    model,
                    request_hash: String::new(),
                },
            )),
            Ev::Paused { steps } => {
                self.status.phase = Phase::Paused;
                self.status.running = 0;
                self.status.turn_steps = steps;
                self.activity
                    .paused(Instant::now(), &format!("step cap {steps} reached"));
                let noun = if steps == 1 { "step" } else { "steps" };
                View::Log(LogEvent::new(
                    1,
                    EventBody::Note(Note {
                        kind: "test".into(),
                        text: format!("turn paused after {steps} {noun}; say continue to resume"),
                    }),
                ))
            }
            Ev::Delta(text) => View::Live(Live::Delta(text)),
            Ev::Usage {
                prompt,
                completion,
                cached,
            } => View::Log(LogEvent::new(
                1,
                EventBody::StepUsage(Usage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    cached_tokens: cached,
                    cache_write_tokens: 0,
                    allowance: None,
                    kind: None,
                }),
            )),
            Ev::Assistant { text } => View::Log(LogEvent::new(
                1,
                EventBody::Assistant {
                    step: 1,
                    text: Payload::text(text),
                    reasoning: None,
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    usage: None,
                },
            )),
            Ev::ToolStarted { name, input } => {
                let id = call_id(&name, &input);
                STARTED.with(|set| set.borrow_mut().insert(id));
                View::Log(LogEvent::new(
                    1,
                    EventBody::CallStarted(CallStart {
                        id: call_id(&name, &input),
                        kind: "tool".into(),
                        info: CallInfo {
                            name,
                            input: Some(Payload::Inline(input)),
                            ..CallInfo::default()
                        },
                    }),
                ))
            }
            Ev::ToolSettled {
                name,
                input,
                outcome,
                exit_code,
                bytes,
                lines,
                spilled,
                out_path,
                tail,
                ..
            } => {
                let id = call_id(&name, &input);
                let started = STARTED.with(|set| set.borrow_mut().remove(&id));
                let mut views = Vec::new();
                if !started {
                    views.push(View::Log(LogEvent::new(
                        1,
                        EventBody::CallStarted(CallStart {
                            id: id.clone(),
                            kind: "tool".into(),
                            info: CallInfo {
                                name: name.clone(),
                                input: Some(Payload::Inline(input.clone())),
                                ..CallInfo::default()
                            },
                        }),
                    )));
                }
                views.push(View::Log(LogEvent::new(
                    1,
                    EventBody::CallSettled(CallSettle {
                        id: call_id(&name, &input),
                        kind: "tool".into(),
                        outcome,
                        exit_code,
                        bytes: Some(bytes),
                        lines: Some(lines),
                        spilled: Some(spilled),
                        out_path: Some(out_path),
                        result: Some(Payload::text(format!("{tail}\n[ END OF RESULT ]"))),
                        tail: Some(tail),
                        ..CallSettle::default()
                    }),
                )));
                for view in views {
                    self.apply(view);
                }
                return;
            }
            Ev::Info(text) => View::Log(LogEvent::new(
                1,
                EventBody::Note(Note {
                    kind: "test".into(),
                    text,
                }),
            )),
            Ev::Error(message) => View::Log(LogEvent::new(
                1,
                EventBody::Error {
                    location: "test".into(),
                    message,
                    retryable: false,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                },
            )),
            Ev::Retrying {
                line,
                until,
                status,
                attempt,
            } => View::Live(Live::Retrying {
                line,
                until,
                status,
                attempt,
            }),
            Ev::RetryingDone => View::Live(Live::RetryingDone),
            Ev::Reply(response) => View::Live(Live::Reply(response)),
            Ev::Herdr { state } => View::Live(Live::Herdr { state }),
            Ev::TurnEnded {
                outcome,
                final_text,
            } => View::Log(LogEvent::new(
                1,
                EventBody::TurnEnded {
                    outcome,
                    reason: None,
                    final_text: Payload::text(final_text),
                },
            )),
        };
        self.apply(view);
    }
}

thread_local! {
    static STARTED: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
}

fn call_id(name: &str, input: &Value) -> String {
    format!(
        "{}-{}",
        name,
        kelir::util::fnv1a64_hex(input.to_string().as_bytes())
    )
}
