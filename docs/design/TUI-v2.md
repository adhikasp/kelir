# TUI v2 design note (input to STEER-10 + STEER-3)
Status: analyst note, 2026-11-19. The implementer follows it and may deviate with a reason in
`docs/DEVIATIONS.md`. Facts below are *observed* unless marked `(recalled)` (not driven in this
session) or `(source)` (read from kelir's code).

> **Amended by tui3 (`docs/tasks/TUI3-fixes.md`, 2026-11-20).** The maintainer's calm pass changed
> four things this note describes, and the sections below are corrected where they are now wrong:
>
> 1. The **facts row** is `kelir <phase> · model · <n> tok` only. Step count, cache % and cost are
>    not ambient (§5, §8.7).
> 2. A `done` turn gets **no summary line**: it ends on its prose. `paused`, `stopped` and `error`
>    keep exactly one line, `⏸ 2 steps · 3.6k tok` — no cost, no time (§5, §8.1).
> 3. **Tool verdicts** carry exit/bytes and no duration: `ok · 1.2 KB`, `✗ exit 101 · 4.1 KB`,
>    `⏹ cancelled`. A fold reads `✓ 2 calls · 1 failed · 1 stopped` and never counts a stopped call
>    as checked; `edit` and any non-`bash` call keep their own line (§5, §8.2).
> 4. The **banner** has no window line; a *degraded* window (a 401, an offline endpoint, a default
>    fallback) is one dim line, and a `/model` swap that changes the window says so once (§3).
>
> Where the dropped numbers went: `events.jsonl` (`usage` events, `jq`-readable), the `state`
> object every `kelir ctl` verb answers with (`window`, `window_source`, `window_note`, `step`), and
> the facts row's token count. `kelir ctl status` (STEER-8) is the planned one-command answer and is
> not landed; until then a live session's spend is in the log. The reasons for all four are in
> `docs/DEVIATIONS.md` under "tui3 and its fixes".

## 1. Method and what was compared

Same tiny repo (README with markdown, two `.py` files, `notes.txt`, a 200x120 four-quadrant PNG with a white
circle), four prompts per tool: (a) explain the repo with heading/bullets/table/quote/code, (b) run 3 shell
commands, (c) look at `image.png`, (d) `sleep 20` (and 25/45 for the kelir mid-run capture). Each tool ran in
its own Herdr pane (200x120 cells), driven with `hc send/keys/wait`, read with `hc read` (exact glyphs).

| tool | version / model | note |
|---|---|---|
| opencode | v2.3.0, DeepSeek V4.1 Flash (default) | full-screen app, right sidebar at 200 cols |
| Claude Code | 2.1.328, Sonnet 5.5, high effort | inline-style app (main screen), no sidebar |
| kelir (baseline) | `target/release/kelir.exe` built 15:25, `deepseek/deepseek-v4.1-flash-1017` via OpenRouter | current inline TUI |

Limits, stated plainly: **no pixel screenshots were taken.** The Herdr window's visible tab was another private
session; capturing my panes would have meant switching the maintainer's focused tab, so I used `hc read` text
only (it carries every glyph; colours are therefore described from the code and from memory of the tools, not
measured). Panes cannot be resized through `hc`, so 80/60/40-column behaviour and live resize are **not observed
in any of the three**; section 8 derives them from the code. Provider slowness / 429 was not provoked (cost);
those rows are `(recalled)`.

Verdict line (bake-off record): on **progress clarity** Claude Code won (spinner + elapsed + tokens while
working, a one-line "done" summary after); on **per-message stats** opencode won (`36.2s · 84.7 tok/s · 4.2s
ttft` under each answer); on **markdown fidelity** they tied (both box tables, no `#`); kelir's baseline is the
weakest on all three. Cost of the comparison: opencode 5 prompts 13-36 s each (flat subscription), Claude Code
4 prompts 30-49 s each (Pro usage, not metered), kelir 6 prompts, 59.6k tokens, $0.0031 total, 79-90 % cache.

## 2. What each tool looks like (condensed captures)

**opencode** (alt-screen; prompt box with a `┃` left bar; sidebar shows title, `Context 13,480 tokens · 1 % used`,
MCP list). Idle bottom bar: `~/path:master   13.5K (1%)   ctrl+p commands`. Working bar, left side:
`■■■⬝⬝⬝⬝⬝ esc interrupt` (an 8-cell sliding block) and, right side, `↓ 1 shell · 15.0K (1%)` when a background
shell exists. In the transcript while thinking: `⠸ 1 thought` (braille spinner). When done the tools fold into
one dim line, `+ 4 commands, 3 thoughts, 1 tool` / `+ 3 commands, 1 thought` / `+ 1 read`, and the answer ends
with `Build · DeepSeek V4.1 Flash 1017 · 36.2s · 84.7 tok/s · 4.2s ttft`. Prompt line shows agent and model
(`Build · model provider · high`).

**Claude Code** (main screen; input between two `─` rules; footer line under it):

```
❯ <user text>                         <- echo of the prompt

  Ran 2 shell commands                <- dim, collapsed
● Tiny Repo                           <- assistant text, "●" on the first row only
  ...
✻ Baked for 49s · done 3:51 PM        <- finished summary (verb is random)

✶ Warping… (8s · ↓ 265 tokens)        <- while working: glyph cycles ✻ ✽ ✶ ·, verb random
  ⎿  Tip: Use /theme to change ...    <- tips line under the spinner
──────────────────────────────────
❯ _
──────────────────────────────────
  ⏵⏵ bypass permissions on · 1 shell · esc to interrupt · ↓ to manage
```
Idle footer drops `esc to interrupt`. Groups read `Ran 2 shell commands`, `Searched for 1 pattern, ran 2 shell
commands`, `Read 1 file`. `Ctrl-O` toggles a *transcript mode* that redraws the whole screen with detail:
`● Bash(sleep 20 && echo finished)` / `⎿  Running in the background (↓ to manage)`, `● Read(<path>)` / `⎿  Read
image (687 bytes)`, a right-aligned `03:53 PM claude-sonnet-5-5` per message, footer
`Showing detailed transcript · ctrl+o to toggle · ↑/↓ to scroll · v to open in notepad`.

**kelir baseline** (inline viewport, 8 rows: 4 live, 3 editor, 1 status):

```
$ git --version  (exit 0 · 29 B)
$ python --version  (exit 0 · 16 B)
Done — git 2.56.0, Python 3.12.10 ...
                                          <- 4 empty live rows
                                          <- (idle: 5 blank rows before the prompt)
›
                                          <- 2 more empty editor rows
kelir done · deepseek/deepseek-v4.1-flash-1017 · step 6/4294967295 · 35.7k tok · 85% cache · $0.0028
```
Working state differs only in the status line text/colour: `kelir working · ... · 1 running`. No spinner, no
elapsed time, nothing moves, prompt identical. Markdown arrives raw (`## Files`, `**bold**`, `|---|`, fences).
Multi-line commands print as `$ python -c " ↵ from PIL ...  (exit 0 · 219 B · 8 lines)`. `Ctrl-O` replaces the
viewport with the last call's tail (2 KB) and the status line says `tail (Ctrl-O closes)`. Prompt echo is `› text`.
Observed oddity: the banner printed `step cap 4294967295 per turn` and the status read `step 6/4294967295`
although STEER-4 says no cap by default; `session.rs` prints it only when `max_steps` is `Some`, so the config
or default produced `Some(u32::MAX)`. Cosmetic, but it is the first thing a user sees: treat `>= u32::MAX` as no
cap in the banner and the status.

## 3. Requirement 1: idle vs working

| | opencode | Claude Code | kelir today |
|---|---|---|---|
| working signal | sliding-block bar + `esc interrupt`, braille spinner in the transcript | spinner glyph + verb + `(8s · ↓ 265 tokens)` above the input, `esc to interrupt` in the footer | status line word `working` in cyan, nothing animates |
| idle signal | bar shows only context size | no spinner; footer lacks `esc to interrupt`; summary line `Baked for 49s` stays above | word `idle` in dark grey |
| input styling | unchanged | unchanged (accepts typing while busy, queues) | unchanged |
| elapsed | per message after the fact | live, `Ns` | none |
| finish | per-message stats line | `✻ <verb> for 49s · done <time>` | status word flips to `done` |
| slow provider (recalled) | a retry notice in the transcript | spinner stays, retry text with countdown appears under it | n/a until STEER-5 |

**Pointer to adopt (as amended by tui3).** Split the viewport's bottom into two single rows with different jobs: a **state row**
directly above the editor (always present, glyph + word + detail + elapsed; this is the "unmistakable" part) and
a **facts row** at the very bottom (phase word, model, tokens; dim, clipped first — step, cache % and cost are
not ambient, see the note at the top). State is conveyed by
glyph *and* word *and* colour so it survives `NO_COLOR` and colour-blind themes. Spinner and elapsed exist only
in working states. The prompt glyph dims while working. On `TurnEnded`, commit **one dim line only when the turn
needs a human** (`⏸ 2 steps · 3.6k tok`, `⏹`/`✗` variants, no cost and no time); a `done` turn ends on its prose.
The banner above the viewport carries model, session, cwd and the help line, plus one dim line when the window
is not the provider's.

State row text (width 80; `⠹` is the spinner, ASCII fallback `|/-\`). Elapsed counts from the state's start:

```
idle          ● idle · ready                                    (dim default fg, no animation)
thinking      ⠹ thinking 12s · waiting for the provider · esc stops        (cyan)
streaming     ⠹ writing 7s · 214 tok                                       (cyan)
running tool  ⠹ running bash 12s · sleep 45 && echo finished3              (cyan; preview clipped with …)
rate limited  ⠹ rate limited (429) · retrying in 34 s · attempt 5 · say stop (yellow; countdown ticks 1 Hz)
paused        ⏸ paused · step cap 50 reached · say continue                (yellow, no spinner)
error         ✗ error · provider 401 · say to retry                         (red, no spinner)
```
`running bash 12s` with several calls in flight reads `running 3 tools 12s · bash: cargo test` (latest preview).
Preview is one line, newlines shown as `↵`, clipped to the width with `…`. `esc stops` is a hint, shown only
while working and only when there is room; `Ctrl-C` keeps its current meaning.

Viewport mock, `thinking`, width 80 (rows are the real viewport; scrollback above):

```
  (scrollback: ... previous turns ...)
› look at the failing test and fix it           <- echo committed to scrollback, then:
                                                <- live rows (3): empty while thinking
 ⠹ thinking 12s · waiting for the provider · esc stops
 › say something, or /later text                <- dim prompt, still accepts input


 kelir · deepseek-v4.1-flash · step 2 · 12.1k tok · 85% cache · $0.0028
```
`streaming`: live rows show the last <=3 rows of the in-flight block (markdown rendered); `running tool`: the
live rows show the call being run and its group so far (section 4); `paused` and `error` keep the live rows
empty and put the next action in the state row. Idle viewport:

```
 ● idle · ready
 › _


 kelir · deepseek-v4.1-flash · step 6 · 35.7k tok · 85% cache · $0.0028
```

**Keybindings.** Enter = send (as today; while working it is a `say`, shown by the dimmed prompt and the hint
`Enter says now · /later queues`). `Esc` once while working = stop (same verb as Ctrl-C's first press; Ctrl-C
double-press still quits). `Ctrl-O` last tool output, `Ctrl-L` redraws (also the manual cure for a garbled
terminal). Nothing else new.

**ratatui notes.**
- The spinner is a `tokio::time::interval(100 ms)` created **only** while the phase is working (idle must cost 0
  wakeups; idle RSS budget 30 MB). Elapsed uses `Instant`, redrawn at 1 Hz (the state row's text changes once a
  second; the glyph changes at 10 Hz): both only touch one row, and ratatui's buffer diff writes just the changed
  cells.
- Derive the state from one pure function `activity(&App) -> Activity { kind, since, detail }` and unit-test the
  transitions (the STEER-10 "state machine" test); the render reads it, the Herdr OSC maps it: thinking, writing,
  running, rate limited -> `working` (STEER-5: stays working); paused -> `blocked`; error -> `error`; done ->
  `done`; idle -> `idle` (the existing `herdr::State`). One source, two consumers, so they cannot drift.
- Keep `Phase` for the coarse phase and add `Activity` for the detail: thinking = turn started / step started
  and no delta yet; writing = first text delta; running = `ToolStarted` with `running > 0`; back to thinking when
  all calls settle and the next request starts.
- Use `Modifier::DIM` for de-emphasis, not `Color::DarkGray` (bright black is the background colour in some
  Solarized-style themes: the baseline's idle status and `Ctrl-O` tail are invisible there).

## 4. Requirement 2: collapsed tool calls

| | opencode | Claude Code | kelir today |
|---|---|---|---|
| one call | folded into the group line | `● Bash(cmd)` + `⎿ result` only in transcript mode | `$ cmd  (exit 0 · 29 B)` full-weight, same colour as prose |
| group | `+ 4 commands, 3 thoughts, 1 tool` | `Ran 2 shell commands`, `Searched for 1 pattern, ran 2 ...` | none, one line per call forever |
| expand | (not driven; sidebar/commands) | `Ctrl-O` full transcript mode, `↓` manage background shells | `Ctrl-O` tail of the last call (2 KB), replaces the viewport |
| errors | (not driven) | not tested | not emphasised |
| thoughts | counted in the fold | not shown | n/a |

**Pointer to adopt.** Two tiers, because scrollback is append-only (`insert_before` cannot rewrite a committed
row; Claude Code's in-place expand is a full redraw we cannot do):

1. **While running**, a call lives in the live area as `⠹ $ cargo test · 12s` (spinner + command + elapsed).
2. **When it settles it joins the open group** held in the live area (not yet committed). The group closes when
   assistant prose starts, a user message arrives, or the turn ends. On close, commit:
   - group of 1-2 calls: one dim line each;
   - group of 3+: one dim line `  ✓ 2 calls · 1 failed · 1 stopped`, plus each failed call as its own red line,
     each call a human stopped (`⏹ cancelled`) and every `edit`/non-`bash` call on its own line;
   - a stopped call is never counted as checked, and `edit`/`write` calls always get their own line (the file is
     what the user cares about; tui3's fold briefly lost this, `tui3fix` restored it).

```
  $ git --version                                  ok · 29 B               <- dim, indent 2
  $ python --version                               ok · 16 B
Done: git 2.56.0 and Python 3.12.10 are installed.                          <- prose, default weight, col 0

  ✓ 2 calls · 1 failed · 1 stopped
  $ cargo test                                     ✗ exit 101 · 4.1 KB     <- red
    error[E0432]: unresolved import `foo`                                   <- <=1 dim tail line
  $ sleep 20                                       ⏹ cancelled             <- a stop is not a success
  edit src/lib.rs                                  ok · 1.2 KB
```
Line grammar: `  <verb> <preview>` left, right-aligned dim meta `ok|✗ exit N · bytes` (no duration: the transcript
is not a profiler). If the meta does not fit,
drop bytes, then the meta; the preview is clipped with `…` first, never the verdict. Multi-line
commands show the first line plus `↵…`. Non-zero exit: whole line red (`Color::Red`; plus the `✗` so it survives
`NO_COLOR`) and one extra dim tail line (last non-empty output line, clipped to the width). A cancelled call
reads `⏹ cancelled`. Prose is never indented and never dim; tool lines are indented 2 and dim: that asymmetry is
the "prose is the focus" mechanism, and costs no colour.

**Expansion.**
- `Ctrl-O` (as today) = overlay of the last call's output tail in the viewport, `Esc`/`Ctrl-O` closes; the
  overlay header is the call line. Keep the 2 KB `read_tail` seek.
- `/expand [n|last]` **appends** the output of call `n` (default: the last group's calls, one block each, capped at
  40 rows with `… N more rows (kelir observe)`) to scrollback, dim and indented 4. It cannot unfold the earlier
  line; say so in the help text.
- `/collapse` and `/expand` with no argument toggle the **policy** for future calls: `collapsed` (default), `full`
  (every call prints its output block). Persist it in the session only.
- Calls carry a session-wide ordinal for `/expand n` (shown right-most and dim only in `full` mode and in the
  Ctrl-O overlay header, not on every line).

**ratatui notes.** The group buffer is plain app state (`Vec<CallLine>` + counters); render it into the live rows
as one line (`ran 3 commands · $ npm test · 12s`). Committing = building `Line`s with styles and one
`insert_before`. Do the grouping on `ToolSettled` events only; the group's identity is "no assistant text and no
user message since the first call". A resize while a group is open is harmless (nothing committed yet).

## 5. Requirement 3: images

| | opencode | Claude Code | kelir today |
|---|---|---|---|
| path in prompt | model's `read` returned the image (`+ 1 read`), described the circle correctly | `Read(path)` -> `Read image (687 bytes)`, described correctly | model had no way to see it; shelled out to PIL and guessed ("Windows-logo-style", missed the circle) |
| paste / drag | `[Image 1]`-style chip, bytes sent with the prompt `(recalled)` | Ctrl-V clipboard image -> `[Image #1]` chip, drag path `(recalled)` | pasted text only; bracketed paste already enabled (`Event::Paste`) |
| cost model | image rides in the message from then on | same | n/a |

Observation worth acting on: both tools succeed with a bare path because their `read` tool returns image parts;
kelir's "pointer, model decides" design is the same idea with the cost moved off the tool schema, and the
baseline run shows what is lost without it (a confidently wrong description from a shell guess).

**Pointer to adopt.** Three ingress paths, one result (a saved file plus a pointer):
1. **Pasted/dropped path (works everywhere, ship first).** On `Event::Paste(text)`: trim, strip one pair of
   quotes, `& '...'` (PowerShell drag), `file://` prefix, and shell escapes (`\ `); if it is a single line, ends in
   `.png|.jpg|.jpeg|.gif|.webp` (case-insensitive) and the file exists, treat it as an attachment instead of
   text. Otherwise paste as text as today.
2. **Windows clipboard on Ctrl-V** when the terminal sends the key instead of a paste (empty text clipboard):
   `windows-sys` is already a dependency; add features `Win32_System_DataExchange` and `Win32_System_Memory`,
   read registered format `PNG` if present, else `CF_DIB`/`CF_DIBV5` and encode a PNG with stored (uncompressed)
   deflate blocks (~60 lines; no `image`/`png` crate). Linux: shell out to `wl-paste --type image/png` or
   `xclip -selection clipboard -t image/png -o` when on `PATH` (spawn with no window, bounded, 2 s timeout);
   if neither exists say `no image on the clipboard (or no wl-paste/xclip)`.
3. Terminal-native image paste: do not rely on it (no portable protocol); it degrades into path 1 or 2.

Saving: `<session>/attachments/<seq>-<sanitised-name>.<ext>` (seq from 1 per session, name from the source file or
`clipboard`), dedupe by content hash, refuse > 8 per prompt and > 10 MB (message in the state row). Dimensions
from the header by hand (PNG IHDR, JPEG SOF, GIF, WebP VP8/VP8L): no decoder crate.

Editor: add an atomic chip element to `editor.rs` (a `Chip { id, label }` in the text buffer, width = label width);
render as reverse-video dim `[image 1]`; one Backspace removes the whole chip (and deletes nothing from disk
until the turn is sent; unsent chip removal deletes the saved file). Cursor math must count the chip as its label
width. On submit the chip expands to the pointer in the `user` event text, and `attachments[]` lists the path:

```
›  what is wrong in [image 1] ?          <- editor
 you> what is wrong in [image: attachments/1-clipboard.png, 1280x720, 312 KB] ?    <- what the model sees
```
`kelir view <path>` (bash-reachable CLI, no tool-schema cost): resolve `attachments/...` against the session dir
first, then the cwd; check the model's vision flag (STEER-1 model metadata) and say `this model cannot take
images` otherwise; on success send a verb that appends a `source` event (`kind: attachment`, path, mime, size) to
the live session (found via the session env var the bash tool already carries); it renders at the **tail** as
a user-role message holding an image part (`image_url` data URL), so earlier messages and the cached prefix are
byte-identical. The CLI prints `image queued: appears in your next message` and exits 0. The TUI commits a dim
line `  view attachments/1-clipboard.png · 312 KB` when the source lands.

**ratatui notes.** No thumbnails (no sixel/kitty; footprint and portability). Paste arrives on the input thread as
one `Event::Paste`; do the file read and hashing off the paint task (spawn_blocking) and insert the chip when it
returns, showing `saving image…` in the state row meanwhile. Never write the path text itself into the editor.

## 6. Requirement 4: markdown

| | opencode | Claude Code | kelir today |
|---|---|---|---|
| headings | bold, no `#` | bold, no `#` | raw `## Files` |
| bold / code | styled | styled | raw `**`, backticks |
| bullets | `-` kept | `-` kept | raw `-` |
| table | box-drawn `┌─┬─┐` | box-drawn with a bold header row | raw pipes |
| quote | `│ ` bar | `▎ ` bar (soft-wraps keep the bar) | raw `>` |
| code block | plain, indented 3, no fence/label | plain, indented 2, no fence/label | raw fences |
| wrap | by word, hanging indent on list rows | same | by word, no hanging indent |

**Pointer to adopt** (STEER-3's pick stands: `pulldown-cmark`, own line model, 16 ANSI colours, no highlighter).
Concrete 16-colour styling:

```
H1   bold + underline                    Title
H2   bold                                Files
H3+  bold + dim                          Details
bold / italic / strike   BOLD / ITALIC / CROSSED_OUT (italic and strike may not render everywhere: harmless)
inline code   yellow fg (Color::Yellow); under NO_COLOR keep the backticks and add REVERSED
link   `text` underlined, then dim ` (url)` only if url != text   (no OSC 8)
bullets  level1 "• ", level2 "◦ ", level3 "▪ "   (ASCII fallback "- ", "* ", "+ ")
ordered  "1. " numbers kept from the source (continuations indent by marker width)
task     "[ ] " / "[x] "   (the x green)
quote    dim "▎ " bar on every wrapped row, text italic
rule     dim "─" x min(width, 60)
code     dim "│ " gutter on every row, language as a dim label row above (omitted when none); no re-wrap, rows
         wider than the area are clipped with a dim "…" at the last column (full text stays in events.jsonl)
table    dim box drawing; header bold; cell text clipped with "…"
html     shown as literal text; images ![alt](u) -> "[image: alt]" dim
```
Tables: widths = max content width per column; if the sum exceeds the area, shrink the widest columns first down
to a minimum of 4; if that still does not fit, or the area is narrower than 30, degrade to `a | b | c` rows with
a bold header and no borders (the STEER-3 rule). Cells wider than their column are truncated with `…`, never
wrapped (keeps one row per record, readable at 40 columns).

**Streaming (the important constraint).** Work in *blocks*, not frames:
- A tiny line-oriented splitter owns the incoming text: it tracks "inside fence", "inside table", and cuts blocks
  at blank lines outside fences/tables. A finished block is rendered **once** with `pulldown-cmark` (the block's
  text only, never the whole message), converted to styled rows for the current width, committed through
  `insert_before`, and forgotten. That is O(total) and keeps the "final render == one-shot render" invariant
  because both paths render the same blocks with the same function.
- The in-flight tail block is re-rendered tolerantly each frame: it is bounded by one paragraph/list/fence, so
  re-parsing it is cheap (coalesce to <=30 fps). An unclosed fence renders as code; an unclosed `**` stays literal
  (pulldown-cmark already does this when the closer is missing). Tables buffer as plain pipe rows in the live area
  and become the boxed table on commit (widths need every row); the live area shows only the last 3 rows, so
  nobody sees the flip except as the table landing in scrollback.
- Live rows are the *tail* of the tail block (last <=3 rows after wrapping), as today's `Live` does for plain text;
  keep its exactly-once commit counters, but count styled rows instead of strings. The earlier O(n^2) fix must
  survive: never re-wrap committed text; re-wrap only the open block, and the whole message only on resize (and
  even then only the not-yet-committed blocks).
- Styles must survive wrapping: wrap on `Span` runs, carry the style across the break, and prepend the block
  prefix (`▎ `, list hanging indent, code gutter) to every continuation row. Measure with `unicode-width` (CJK/emoji
  count 2; zero-width joiners 0; never split inside a cluster you cannot measure).
- Tool output, user text, the state/facts rows stay plain (STEER-3 scope). `markdown = false` and `NO_COLOR` use
  the same model with an identity style map.

**ratatui notes.** Model: `Block -> Vec<Line<'static>>` (ratatui `Span` runs); no extra crate. `Paragraph::new(Vec<Line>)` for the live rows and inside the
`insert_before` closure (as now). Put the width-dependent step (wrap/table fit) behind one function
`render_block(&Block, width) -> Vec<Line>` so the resize path and the tests call exactly the same code. Add the
`trait Highlighter` seam as STEER-3 asks; the default impl returns the line unstyled.

## 7. Requirement 5: cleanliness, resize, narrow, themes

What the baseline shows today (observed): five blank rows between the last answer line and the prompt when idle
(four empty live rows, plus the editor's spare rows), no blank line separating a user echo from the next output,
tool lines the same weight as prose, a status line that is one long clipped string. Both reference tools also
leave exactly one blank line between blocks and put chrome (rules, footer) only around the input.

**Spacing rules.** One blank row before a user echo and before prose that follows tool lines; none between
consecutive tool lines; none between a tool group and its summary; one blank row after the turn-summary line. The
banner is separated from the first prompt by one blank row. Empty live rows are *not* painted as blank text:
reduce them by making the viewport shorter when nothing streams (below).

**Viewport height.** Today: fixed 8 (4 live + 3 editor + 1 status). Proposed: 8 as well but re-allocated
(3 live + 1 state + 3 editor + 1 facts) at >= 8 terminal rows; the editor grows up to 5 rows for multi-line input
by taking from the live rows, never from the state row. Under 8 rows: drop live rows first, then the facts row,
then the editor to 1 row; the state row is the last thing to go (`layout()` already keeps the editor last to go). **Verify in ratatui 0.30** whether an inline viewport can change height after creation; if it cannot
without recreating the terminal, keep it fixed (recreating means clear + redraw = flicker). A 3-row live area
means the idle gap is 3 blank rows: acceptable, and better than 5.

**Flicker.** (1) Wrap every repaint in synchronized output: write `\x1b[?2026h` before and `\x1b[?2026l` after
the `insert_before` + `draw` pair (crossterm `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`; terminals that
do not know it ignore it, so no capability probing). (2) Coalesce: drain all pending `UiEvent`s, then draw at most
once per ~33 ms; the existing `drain` already batches, add the cap. (3) Only the diff: never `Terminal::clear`
outside a resize; the spinner/elapsed tick touches one row. (4) Herdr OSC records stay between frames (existing
`flush_status`).

**Resize.** The terminal reflows or truncates the old viewport rows unpredictably (terminal-dependent), so on
`Event::Resize`: debounce ~50 ms, call `terminal.autoresize()`/re-query the size, clear from the viewport's top
row down (`MoveTo(0, top)` + `Clear(FromCursorDown)`, the same call `finish()` uses), redraw, re-wrap the open
block and the live area. Committed scrollback cannot reflow: accepted and documented (SPEC says so for STEER-3).
Never let a resize emit duplicate scrollback rows: the exactly-once counters in `Live` already guard this.
`(unverified)` that Windows Terminal leaves no ghost rows when narrowing; the live drive must check it.

**Width tiers** (derived from the layout, to be snapshot-tested at 80/60/40):

```
80+  state row full; facts: kelir <phase> · model · tok ; tool meta right-aligned; tables boxed
60   facts: phase · model(short) · tok; state detail clipped
40   state row: glyph word elapsed only; facts: phase · tok; tool lines: preview + ✗/ok; tables -> pipe rows (<30)
<30  everything is a single clipped line; editor prefix "› " stays; no right-aligned meta
```
Truncation priority for the facts row (drop from the right end first): model -> tokens. There is no step, cache %
or cost in it (tui3; the design's original `cache % -> step -> cost -> model -> tokens` no longer applies).
The state word is never dropped; the detail is clipped with `…`, the spinner glyph first column.

**Themes and colour.** Only named 16 ANSI colours and the terminal default (`Color::Reset`), never `White`,
`Black`, `Rgb`, `Indexed`, or background fills (reverse video is the only "background" we use, and it inverts
correctly on both light and dark). De-emphasis = `Modifier::DIM`; emphasis = `BOLD`; state colours: working cyan,
rate limited / paused yellow, error red, done/ok green, idle default-dim. `NO_COLOR` (any non-empty value):
drop every `fg`/`bg`, keep `BOLD`/`DIM`/`UNDERLINE`/`REVERSED`, and keep the glyphs (`●`, `⠹`, `✗`, `⏸`, `✓`)
so the state remains readable. `KELIR_ASCII=1` or `TERM=dumb`: ASCII glyph set (`*`, `|/-\`, `x`, `||`, `v`).
Unicode box drawing is fine on Windows Terminal; for legacy conhost fall back to ASCII table borders (`+-|`).

**Tests that matter.** `TestBackend` snapshots per state at 80/60/40 (the mocks above are the expected shape);
a unit test for the activity transitions; a markdown stream test that splits each corpus document at every
byte/UTF-8 boundary; a PTY e2e that asserts `\x1b[?2026h`/`l` pairs surround each frame and that no spinner
tick happens while idle (count writes over 2 s of idleness: expect 0).

## 8. Decisions I would make (ranked by impact)

1. **State row + facts row split, spinner/elapsed only while working, dim prompt, a one-line outcome only for
turns that need a human.**
   Biggest single win; fixes "is it alive?" and costs one tokio interval that exists only while working.
2. **Dim one-line tool calls; group 3+ into `✓ N calls` naming what it does not cover (`· 1 failed · 1
   stopped`); errors red with a `✗` and one tail line; `edit` and non-`bash` calls keep their own line;
   expansion by Ctrl-O overlay and an append-only `/expand`.** Make prose the only default-weight text.
3. **Block-based incremental markdown**, with the 16-colour map in section 6 and tables boxed on commit.
4. **Synchronised updates + 33 ms frame cap + clear-on-resize.** Cheap, removes visible tearing during streaming.
5. **Image ingress by pasted path first** (cross-platform, no new dependency), then `kelir view` + the tail
   `source` event, then Windows Ctrl-V clipboard; chip `[image N]` in the editor, pointer text on submit.
6. **`DIM` instead of `DarkGray`, `NO_COLOR` as attributes-only, ASCII glyph fallback.**
7. **Re-allocate the 8 viewport rows (3 live / state / 3 editor / facts)** and kill the 5-blank-row gap; apply the
   spacing rules.
8. **Rate-limit countdown row** wired to STEER-5's `error` events (`retry_in_ms`) once they exist; until then the
   state row shows `thinking` with the elapsed time, which already answers "is it stuck?".
9. Cosmetic: treat a step cap `>= u32::MAX` as no cap in banner and status.

Suggested build order for the single "tui2" job: (1) activity/state machine + rows + NO_COLOR/DIM, (2) tool-call
lines/groups/expand, (3) markdown model + stream splitter + snapshots, (4) sync updates/resize, (5) image path
ingress + chip + attachments dir, (6) `kelir view` + `source` event + e2e, (7) Windows clipboard.

## 9. Do not copy

- **Alt-screen, sidebars, mouse capture** (opencode). Breaks scrollback, terminal selection and the inline
  premise; a sidebar also needs 200 columns.
- **Random whimsical verbs and tip lines** (Claude Code: `Topsy-turvying…`, `⎿ Tip: …`). Noise, not
  deterministic, impossible to snapshot; STEER-10 names the exact words.
- **LLM-generated session titles** (opencode sidebar title). A hidden model call per session: spends tokens
  and breaks the "every request is the agent's own" cache story.
- **Image bytes riding with the prompt** (both tools). Every later request pays for them; kelir's pointer plus
  tail `source` keeps the prefix byte-stable, and image tokens are paid only when the model asks.
- **In-place rewrite of committed rows for expand** (Claude Code's transcript mode redraws everything). Not
  possible with `insert_before`; do overlay + append-only expand, never cursor games over scrollback.
- **Syntect/highlighting engines, true colour, OSC 8 hyperlinks, kitty/sixel graphics, thumbnails.** Binary size,
  width-accounting bugs with escape sequences inside cells, and portability.
- **A constant-rate repaint** (e.g. a 20 Hz animation while idle): idle must cost zero wakeups, and a spinner
  must stop the moment the state leaves working.

## 10. Open items for the implementer / reviewer

- Confirm ratatui 0.30 inline viewport height mutability (section 7); else keep it fixed.
- Confirm the `windows-sys` clipboard features add < 50 KB; if not, ship path-paste only and note it.
- Live drive must verify: Windows Terminal narrowing, spinner stops on `Esc`, `NO_COLOR=1`, light theme
  legibility of `DIM`, a paste of a quoted Windows path with spaces, and a paste of a very long single-line text
  that is *not* an image path.
