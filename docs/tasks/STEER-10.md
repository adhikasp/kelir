# STEER-10 — TUI polish (v2): progress clarity, collapsed tool calls, image paste, full markdown

Maintainer requirements (2026-11-19). Builds on STEER-3 (markdown); **do both as one job ("tui2")**, after the design
note docs/design/TUI-v2.md exists (a fresh analyst compares opencode and Claude Code runs and records pointers; the
implementer follows it and may deviate with a stated reason).

## Requirements
1. **Current progress is unmistakable: idle vs agent working.** A persistent status line shows the state at a glance:
   `idle` (waiting for you), `thinking` (waiting on the provider; elapsed + spinner), `writing` (streaming text),
   `running <tool>` (elapsed, command preview), `rate limited, retrying in 34 s` (STEER-5), `paused`, `error`.
   Spinner/elapsed time only while working; the input prompt styling differs between idle and working (e.g. a dimmed
   prompt while the agent works, still accepting `say`/`later`). On finish a one-line summary (steps, tokens, cost,
   time) then back to idle. The Herdr OSC status stays in sync.
2. **Tool calls are collapsed and de-emphasised.** One dim line per call (`$ cmd` / `edit path` + exit/bytes/time); the
   output is hidden by default and expandable (focus + key, or `Ctrl-O` for the last one, `/expand`, `/collapse`); groups
   of consecutive calls fold into "ran 5 commands" when finished; errors/non-zero exits are emphasised (colour) but
   still one line with a short tail. The assistant's prose is the visual focus.
3. **Paste images.** Pasting an image (clipboard image via bracketed paste of a path, drag-dropped file path, or the
   terminal's image paste where available; Windows clipboard image via the platform API when the terminal sends Ctrl-V)
   saves it into the session state dir (`<session>/attachments/<seq>-<name>.png`), and the submitted prompt contains a
   **pointer** (`[image: attachments/…png, 1280x720, 312 KB]`) rather than the bytes. The model decides whether to look:
   `kelir view <path>` (a CLI reached through bash, no tool-schema cost) makes the image arrive as an image part in the
   next request (a `source` event, kind `attachment`, per SPEC M5 attachments), never edited into earlier messages
   (cache prefix stays intact). Without vision support in the model the CLI says so. The TUI shows a thumbnail-free
   chip `[image 1]` in the input box and removes it on backspace.
4. **All markdown rendered** (STEER-3 scope): headings, bold/italic/strike, inline code, fenced code, lists (nested,
   ordered, task), block quotes, tables, links, rules; streaming-tolerant; width-aware wrapping.
5. Cleanliness: consistent spacing, no flicker (synchronised updates / minimal repaint), correct behaviour on resize,
   narrow panes (80, 60, 40 columns), light and dark terminals (ANSI 16 colours only), `NO_COLOR`.

## Tests and gates
Unit (state machine of the activity indicator, collapse/expand model, markdown model, attachment saving and pointer
text, path/clipboard parsing), TestBackend snapshots for every state and width, PTY e2e on the mock (paste a file
path of a PNG, see the pointer, `kelir view` yields an image part on the next request, prefix byte-stable), Windows +
Linux gates, no leaked processes. Live drive (reviewer): screenshots in a Herdr pane against a real cheap model for
each requirement; compare with the design note.
