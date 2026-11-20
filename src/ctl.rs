//! `kelir ctl <session> <verb> [args…]`: send one verb to a running session.
//!
//! The session is resolved from the state dir: an exact id, a unique prefix, or
//! `latest` (the newest session only, and an error when it is not running — a
//! verb never lands on an older session by accident). The verb is validated
//! locally, and the answer is printed as one JSON line. Exit 0 for `ok`, 1 for
//! a refusal, 2 for a bad invocation.

use crate::cli::CtlArgs;
use crate::ipc::{self, Request};
use crate::{paths, verbs};

pub async fn ctl(args: CtlArgs) -> u8 {
    let flags = verbs::CliFlags {
        now: args.now,
        call: args.call.clone(),
        reason: args.reason.clone(),
        disable: args.disable.clone(),
        enable: args.enable.clone(),
        path: args.path.clone(),
        text: args.text.clone(),
        name: args.name.clone(),
        kind: args.kind.clone(),
    };
    let request_args = match verbs::cli_args(&args.verb, &args.args, &flags) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("kelir: {message}");
            return 2;
        }
    };

    let candidates = match paths::session_candidates(&args.session) {
        Ok(candidates) => candidates,
        Err(message) => {
            eprintln!("kelir: {message}");
            return 1;
        }
    };

    let request = Request::new(args.verb.clone(), request_args, "ctl");
    let mut last: Option<(String, String)> = None;
    for id in &candidates {
        let dir = paths::session_dir(id);
        // The token comes from the session's own `<session dir>/token`, which
        // only its owner can read (STEER-7): a human's terminal knows which
        // session it is steering, so it reads that file rather than the
        // environment a tool child would get.
        match ipc::request_with_token_file(&dir, id, &request).await {
            Ok(response) => {
                // `status` reads for a human unless `--json` asks for the line.
                match (&args.verb[..], &response.state, args.json) {
                    ("status", Some(state), false) if response.ok => {
                        println!("{}", crate::spend::render_text(state));
                    }
                    _ => println!("{}", response.line()),
                }
                return if response.ok { 0 } else { 1 };
            }
            Err(error) => last = Some((id.clone(), error)),
        }
    }

    match last {
        Some((id, error)) => {
            eprintln!("kelir: session {id} is not running ({error})");
        }
        None => eprintln!("kelir: no session to send to"),
    }
    1
}
