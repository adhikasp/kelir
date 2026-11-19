# STEER-7 — session-socket hardening (retro M5-A F8)

The session socket accepts `bg` reports and `cancel` for job ids it did not create. During dogfooding a test child
inside a running session impersonated the shim (43 injected "bg job was lost" user events) and one run cancelled a
job belonging to the driver. Tests were scrubbed; the harness is still exposed.

## Build
- The session only accepts `bg` reports / `cancel` / `bg-spawn` for job ids **it created or registered** (an
  in-memory + `bg/` registry); unknown or foreign ids are refused with a clear error and a `verb` event marked
  refused. Job dirs and status/out paths must resolve **inside the session's own `bg/`** (canonicalise, reject `..`,
  symlinks escaping, UNC/other-drive paths on Windows).
- Authenticate callers beyond "can reach the socket": a per-session secret token (random, generated at start, held
  in the session's memory, exported to the session's own tool children in an env var such as `KELIR_TOKEN` and
  required on every verb that mutates state; never logged, never in events/argv). Document that any process that
  can read the session's environment is already trusted. Keep `ctl` from a human terminal working (it reads the
  token from the session dir file with owner-only permissions, `0600` / owner-only ACL).
- Unix socket and Windows pipe permissions re-checked (owner-only).
- Tests: foreign job id refused (report, cancel, spawn), path escapes refused, token missing/wrong refused,
  a child process of one session cannot cancel another session's job, `ctl` works with the token file; red tests
  first. Windows + Linux gates; no leaked processes.
