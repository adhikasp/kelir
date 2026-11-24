//! Large-payload spilling to the blob directory.
use serde_json::Value;

use crate::log::event::*;

/// Apply the payload rule to every string field of an event.
pub fn spill_payloads(body: EventBody, blobs: &Blobs, seq: u64) -> Result<EventBody, String> {
    Ok(match body {
        EventBody::SessionStarted {
            id,
            format,
            parent,
            depth,
            cwd,
            os,
            kelir_version,
            provider,
            model,
            system,
            sources,
            tools,
            reasoning_effort,
            key_origin,
        } => EventBody::SessionStarted {
            id,
            format,
            parent,
            depth,
            cwd,
            os,
            kelir_version,
            provider,
            model,
            system: SystemInfo {
                text: spill_payload(blobs, seq, "system.text", system.text)?,
                hash: system.hash,
            },
            sources,
            tools,
            reasoning_effort,
            key_origin,
        },
        EventBody::User {
            text,
            from,
            delivery,
            attachments,
        } => EventBody::User {
            text: spill_payload(blobs, seq, "user.text", text)?,
            from,
            delivery,
            attachments,
        },
        EventBody::Source {
            kind,
            name,
            text,
            path,
        } => EventBody::Source {
            kind,
            name,
            text: text
                .map(|text| spill_payload(blobs, seq, "source.text", text))
                .transpose()?,
            path,
        },
        EventBody::Assistant {
            step,
            text,
            reasoning,
            tool_calls,
            finish_reason,
            usage,
        } => {
            let mut spilled_calls = Vec::with_capacity(tool_calls.len());
            for (index, call) in tool_calls.into_iter().enumerate() {
                let field = format!("tool_calls.{index}.args");
                spilled_calls.push(LoggedToolCall {
                    id: call.id,
                    name: call.name,
                    args: spill_payload(blobs, seq, &field, call.args)?,
                });
            }
            EventBody::Assistant {
                step,
                text: spill_payload(blobs, seq, "text", text)?,
                reasoning: reasoning
                    .map(|reasoning| spill_payload(blobs, seq, "reasoning", reasoning))
                    .transpose()?,
                tool_calls: spilled_calls,
                finish_reason,
                usage,
            }
        }
        EventBody::CallStarted(mut start) => {
            if let Some(input) = start.info.input.take() {
                start.info.input = Some(spill_payload(blobs, seq, "input", input)?);
            }
            EventBody::CallStarted(start)
        }
        EventBody::CallSettled(mut settle) => {
            if let Some(result) = settle.result.take() {
                settle.result = Some(spill_payload(blobs, seq, "result", result)?);
            }
            EventBody::CallSettled(settle)
        }
        EventBody::TurnEnded {
            outcome,
            reason,
            final_text,
        } => EventBody::TurnEnded {
            outcome,
            reason,
            final_text: spill_payload(blobs, seq, "final_text", final_text)?,
        },
        EventBody::Compacted {
            through_seq,
            summary,
            pinned_seqs,
            by,
            tokens_before,
            tokens_after,
            cache_restart,
        } => EventBody::Compacted {
            through_seq,
            summary: spill_payload(blobs, seq, "summary", summary)?,
            pinned_seqs,
            by,
            tokens_before,
            tokens_after,
            cache_restart,
        },
        other => other,
    })
}

pub(super) fn spill_payload(
    blobs: &Blobs,
    seq: u64,
    field: &str,
    payload: Payload,
) -> Result<Payload, String> {
    match payload {
        Payload::Inline(Value::String(text)) => blobs.spill_text(seq, field, &text),
        Payload::Inline(value) => blobs.spill_value(seq, field, &value),
        already_spilled => Ok(already_spilled),
    }
}
