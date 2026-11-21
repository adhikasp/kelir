# bashdesc implementation notes

## Shape
- `description` is optional in the `bash` schema (`required: ["command"]` only);
  the tool function description tells the model to use a short label.
- `tools_view::description(&Value)` is the single sanitizer: trims, folds all
  whitespace to one line, caps at `DESCRIPTION_MAX = 120` **characters** (not
  bytes, so a wide Unicode label cannot crowd out the right-aligned verdict),
  and returns `None` for missing/blank/wrong-typed values.
- All three renderers share it: `tui::call_head` (settled line / `/expand`
  header), `tui::call_label` (the live `⠹` row) and `session::compact_input`
  (headless `run`, stdout for a top-level session, stderr for a child).

## Compatibility
- A call with no description, a blank one or a non-string one renders exactly
  as before (`$ <command>` in TUI, `<command>` in headless). Old logs, resumed
  sessions and third-party callers are unaffected.
- The full args (`command` **and** `description`) are still written verbatim to
  the `assistant` and `tool_started` events — the e2e asserts both. The label
  never replaces the command in the log or in `/expand`.
- The frozen system prefix (prompt + sources + tools) changes only in that
  future sessions carry the new schema; within a session the tools are the same
  list on every step (asserted request-for-request in the e2e). The base prompt
  gained the bullet, but the 1024-byte budget still holds (985 bytes).

## Tests
- Unit: schema (required/properties), description extraction/cap/fallback,
  head/label/line rendering (dim, width, failed red line), `compact_input`.
- E2E (`tests/e2e_bashdesc.rs`): real binary + scripted mock; headless stdout
  head, NDJSON `tool_started`, schema and prefix stability, TUI head/row,
  command preserved in log and output file.
- Docs: `src/prompt.md`, `docs/reference/tui.md` (size budget was tight; prose
  in the Tool lines section was condensed without dropping facts), generate +
  `cargo test --lib docs` green.
