//! The event log: the only source of truth for a session.
//! One JSON object per line, `{"seq": n, "ts": "<RFC 3339>", "type": …}`, `seq` monotonic
//! from 1, fsync'd per batch.
//! A torn last line is tolerated on read (skipped and reported), never half-applied.

pub mod log;
pub mod payload;
pub mod spill;
pub mod types;

pub use log::*;
pub use payload::*;
pub use spill::*;
pub use types::*;

#[cfg(test)]
pub fn test_usage(prompt: u64, completion: u64, cached: u64, kind: Option<&str>) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cached_tokens: cached,
        cache_write_tokens: 0,
        allowance: None,
        kind: kind.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kelir-event-test-{}-{}",
            ulid::Ulid::generate(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn started() -> EventBody {
        EventBody::SessionStarted {
            id: "ses_01".into(),
            format: 1,
            parent: None,
            depth: 0,
            cwd: "/tmp".into(),
            os: "linux".into(),
            kelir_version: "0.1.0".into(),
            provider: "openrouter".into(),
            model: "test/model".into(),
            system: SystemInfo {
                text: "you are kelir".into(),
                hash: "abc".into(),
            },
            sources: vec![],
            tools: vec!["bash".into(), "edit".into()],
            reasoning_effort: None,
            key_origin: None,
        }
    }

    fn samples() -> Vec<EventBody> {
        vec![
            started(),
            EventBody::User {
                text: "do it".into(),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: vec![],
            },
            EventBody::Source {
                kind: "hint".into(),
                name: "note".into(),
                text: Some("try this".into()),
                path: None,
            },
            EventBody::StepStarted {
                step: 1,
                provider: "openrouter".into(),
                model: "test/model".into(),
                request_hash: "00".into(),
            },
            EventBody::Assistant {
                step: 1,
                text: "on it".into(),
                reasoning: Some("hmm".into()),
                tool_calls: vec![LoggedToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    args: json!({"command": "ls"}).into(),
                }],
                finish_reason: Some("tool_calls".into()),
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cached_tokens: 4,
                    cache_write_tokens: 2,
                    allowance: None,
                    kind: None,
                }),
            },
            EventBody::StepUsage(Usage {
                prompt_tokens: 40,
                completion_tokens: 60,
                cached_tokens: 0,
                cache_write_tokens: 0,
                allowance: None,
                kind: Some("compact".into()),
            }),
            EventBody::Verb {
                name: "say".into(),
                args: json!({"text": "hi", "now": true}).into(),
                from: "ctl".into(),
                boundary: "now".into(),
                error: None,
            },
            call_start(
                "call_1",
                "tool",
                CallInfo {
                    name: "bash".into(),
                    input: Some(json!({"command": "ls"}).into()),
                    dir: Some("9-1-1".into()),
                    ..CallInfo::default()
                },
            ),
            call_settle(
                "call_1",
                "tool",
                "ok",
                CallSettle {
                    exit_code: Some(0),
                    bytes: Some(3),
                    lines: Some(1),
                    out_path: Some("calls/call_1/out".into()),
                    spilled: Some(false),
                    result: Some("ok\n".into()),
                    ..CallSettle::default()
                },
            ),
            call_start(
                "mcp_01",
                "mcp",
                CallInfo::mcp("demo", "echo", json!({"text": "hi"}).into()),
            ),
            call_start("bg_01", "bg", CallInfo::named("bg", "sleep 60".into())),
            call_start(
                "chd_01",
                "child",
                CallInfo::child(
                    "look at the tests".into(),
                    "ses_02",
                    "ses_01",
                    1,
                    "child",
                    true,
                    "test/model",
                ),
            ),
            EventBody::Compacted {
                through_seq: 7,
                summary: "we did the first half".into(),
                pinned_seqs: vec![2],
                by: "agent".into(),
                tokens_before: 900,
                tokens_after: 120,
                cache_restart: true,
            },
            EventBody::Error {
                location: "provider".into(),
                message: "boom".into(),
                retryable: true,
                status: Some(500),
                attempt: Some(2),
                retry_in_ms: Some(1_500),
            },
            EventBody::Note(Note {
                kind: "warn_prompt_tokens".into(),
                text: "too long".into(),
            }),
            EventBody::Note(Note {
                kind: "notify".into(),
                text: "turn_done: 1 hook(s) fired".into(),
            }),
            EventBody::TurnEnded {
                outcome: "done".into(),
                reason: None,
                final_text: "done".into(),
            },
            EventBody::Gate {
                gate: "g".into(),
                event: "turn_end".into(),
                decision: "allow".into(),
                reason: String::new(),
                ms: 1,
                rewritten: true,
                error: None,
            },
            EventBody::SessionEnded {
                reason: "done".into(),
            },
            EventBody::SessionTitle {
                title: "a title".into(),
            },
        ]
    }

    #[test]
    fn read_events_refuses_a_log_from_another_format() {
        let dir = temp_dir("format");
        let path = dir.join("events.jsonl");
        let mut first = serde_json::to_value(LogEvent::new(1, started())).unwrap();
        first["format"] = json!(2);
        std::fs::write(&path, format!("{first}\n")).unwrap();
        let error = read_events(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(FORMAT_MESSAGE));

        let user = serde_json::to_string(&LogEvent::new(
            1,
            EventBody::User {
                text: "hi".into(),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: vec![],
            },
        ))
        .unwrap();
        std::fs::write(&path, format!("{user}\n")).unwrap();
        let error = read_events(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(FORMAT_MESSAGE));
    }

    #[test]
    fn payloads_spill_only_when_oversized_and_resolve_back() {
        let dir = temp_dir("spill");
        let blobs = Blobs::new(&dir);
        assert_eq!(
            blobs.spill_text(1, "text", "short").unwrap(),
            Payload::text("short")
        );
        assert!(!blobs.dir().exists());

        let big = "x".repeat(50_000);
        let payload = blobs.spill_text(7, "assistant.text", &big).unwrap();
        let blob = match &payload {
            Payload::Blob(blob) => blob.clone(),
            other => panic!("expected a blob, got {other:?}"),
        };
        assert_eq!(blob.path, "blobs/7-assistant.text");
        assert_eq!(blob.bytes, 50_000);
        assert_eq!(blob.preview.len(), PREVIEW_BYTES);
        assert_eq!(payload.resolve_text(&dir).unwrap(), big);
        assert!(dir.join("blobs").join("7-assistant.text").exists());

        let value = json!({"new": "y".repeat(20_000)});
        let payload = blobs.spill_value(3, "input", &value).unwrap();
        assert!(payload.is_spilled());
        assert_eq!(payload.resolve(&dir).unwrap(), value);
    }

    #[test]
    fn spill_rewrites_every_body_field_and_keeps_the_log_small() {
        let dir = temp_dir("body");
        let blobs = Blobs::new(&dir);
        let big = "z".repeat(PAYLOAD_LIMIT + 1);
        let body = EventBody::Assistant {
            step: 2,
            text: big.clone().into(),
            reasoning: Some(big.clone().into()),
            tool_calls: vec![LoggedToolCall {
                id: "c1".into(),
                name: "edit".into(),
                args: json!({"new": big.clone()}).into(),
            }],
            finish_reason: Some("tool_calls".into()),
            usage: None,
        };
        let spilled = spill_payloads(body, &blobs, 11).unwrap();
        let value = serde_json::to_value(&spilled).unwrap();
        for path in ["text", "reasoning", "tool_calls"] {
            assert!(!value[path].is_null(), "{path}");
        }
        assert_eq!(value["text"]["path"], "blobs/11-text");
        assert_eq!(value["reasoning"]["path"], "blobs/11-reasoning");
        assert_eq!(
            value["tool_calls"][0]["args"]["path"],
            "blobs/11-tool_calls.0.args"
        );
        assert!(serde_json::to_string(&spilled).unwrap().len() < 10_000);
    }

    #[test]
    fn append_sync_and_reopen_keep_monotonic_sequences() {
        let dir = temp_dir("seq");
        let path = dir.join("events.jsonl");
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.next_seq(), 1);
        for i in 0..4 {
            assert_eq!(log.append(started()).unwrap().seq, i + 1);
        }
        log.sync().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let read = read_events(&path).unwrap();
        assert_eq!(
            read.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(read.torn_last.is_none() && read.bad_lines.is_empty());
        // Reopening continues the sequence instead of restarting it.
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.append(started()).unwrap().seq, 5);
    }

    #[test]
    fn torn_and_garbage_lines_are_reported_and_cut_on_reopen() {
        let dir = temp_dir("torn");
        let good = serde_json::to_string(&LogEvent::new(1, started())).unwrap();
        let whole = serde_json::to_string(&LogEvent::new(2, samples()[1].clone())).unwrap();
        let cases: [(&str, &str, bool); 3] = [
            (
                "torn",
                &format!("{good}\n{{\"seq\": 2, \"ts\": \"x\", \"type\": \"assis"),
                true,
            ),
            ("whole", &format!("{good}\n{whole}"), false),
            ("garbage", &format!("{good}\nnot json\n{good}\n"), false),
        ];
        for (name, text, torn) in cases {
            let path = dir.join(format!("events-{name}.jsonl"));
            std::fs::write(&path, text).unwrap();
            let read = read_events(&path).unwrap();
            assert_eq!(read.torn_last.is_some(), torn, "{name}");
            if name == "garbage" {
                assert_eq!(read.events.len(), 2);
                assert_eq!(read.bad_lines.len(), 1);
            } else {
                assert_eq!(read.events.len(), if torn { 1 } else { 2 });
            }
            if name != "garbage" {
                let before = read_events(&path).unwrap();
                let mut log = EventLog::open(&path).unwrap();
                let next = before.events.last().unwrap().seq + 1;
                assert_eq!(log.append(started()).unwrap().seq, next, "{name}");
                log.sync().unwrap();
                let read = read_events(&path).unwrap();
                assert_eq!(read.events.len(), before.events.len() + 1, "{name}");
                assert!(
                    read.bad_lines.is_empty() && read.torn_last.is_none(),
                    "{name}: {read:?}"
                );
            }
            if torn {
                let sidecar = std::fs::read_to_string(
                    path.with_file_name(format!("events-{name}.jsonl.torn")),
                )
                .unwrap();
                assert!(sidecar.contains("assis"), "{sidecar}");
            }
        }
        // Every byte cut of a second line is torn; only the whole line parses.
        for cut in 0..=whole.len() {
            let path = dir.join(format!("events-cut-{cut}.jsonl"));
            let bytes = format!("{good}\n{}", &whole[..cut]);
            std::fs::write(&path, bytes).unwrap();
            let read = read_events(&path).unwrap();
            assert_eq!(
                read.events.len(),
                1 + usize::from(cut == whole.len()),
                "cut {cut}"
            );
            assert_eq!(read.events[0].seq, 1);
            if cut > 0 && cut < whole.len() {
                assert!(
                    read.torn_last.is_some() || read.bad_lines.len() == 1,
                    "cut {cut}: {read:?}"
                );
            }
        }
    }

    #[test]
    fn a_batch_is_whole_on_disk_before_sync_returns() {
        let dir = temp_dir("batch");
        let path = dir.join("events.jsonl");
        let mut log = EventLog::open(&path).unwrap();
        for body in samples().into_iter().take(4) {
            log.append(body).unwrap();
        }
        log.sync().unwrap();
        // A fresh read (a new process, a new handle) sees every event of the
        // batch and nothing partial: the file ends on a line boundary.
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 4);
        assert!(read.torn_last.is_none(), "{read:?}");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        // Every line is a whole event, in seq order.
        assert_eq!(
            read.events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn a_title_is_the_first_prompt_line_flattened_and_bounded() {
        assert_eq!(
            title_from_prompt("  fix the  input gap\nand more"),
            "fix the input gap"
        );
        assert_eq!(title_from_prompt("\n\n  "), "untitled");
        let long = "a".repeat(200);
        let title = title_from_prompt(&long);
        assert_eq!(title.chars().count(), 60);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn blob_refs_need_the_prefix_and_a_whole_line_keeps_its_newline() {
        // A tool argument that merely looks like a blob ref is never resolved from disk.
        let fake = json!({"spilled": true, "path": "not-a-blob/1", "bytes": 1, "preview": "x"});
        let payload: Payload = serde_json::from_value(fake.clone()).unwrap();
        assert_eq!(payload, Payload::Inline(fake));
        let blob = Payload::Blob(BlobRef {
            spilled: true,
            path: "blobs/9-text".into(),
            bytes: 11,
            preview: "hello".into(),
        });
        assert_eq!(
            serde_json::from_value::<Payload>(serde_json::to_value(&blob).unwrap()).unwrap(),
            blob
        );
        let dir = temp_dir("torn-whole");
        let path = dir.join("events.jsonl");
        let first = serde_json::to_string(&LogEvent::new(1, started())).unwrap();
        let whole = serde_json::to_string(&LogEvent::new(2, samples()[1].clone())).unwrap();
        std::fs::write(&path, format!("{first}\n{whole}")).unwrap();
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.append(started()).unwrap().seq, 3);
        assert_eq!(std::fs::read(&path).unwrap().last(), Some(&b'\n'));
        assert!(!dir.join("events.jsonl.torn").exists());
        // Reopening a log that already ends in a newline must not stash an empty tear.
        let clean = dir.join("clean.jsonl");
        std::fs::write(&clean, format!("{first}\n{whole}\n")).unwrap();
        let _ = EventLog::open(&clean).unwrap();
        assert!(!dir.join("clean.jsonl.torn").exists());
    }
}
