//! The agent's markdown, rendered into styled rows.
//! Three pieces, each testable alone:

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Prose,
    Strong,
    Emphasis,
    Strike,
    Code,
    Link,
    H1,
    H2,
    H3,
    /// De-emphasised: block-quote text, code gutters, table borders, a link's
    /// URL, an image placeholder.
    Dim,
    /// The task-list box that is not ticked.
    Muted,
    /// The `x` of a ticked task box.
    Done,
    /// A fenced block's language label.
    Label,
}

/// A stretch of text with one tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub tone: Tone,
}

impl Run {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// One rendered row: runs, already wrapped to the width it was rendered for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub runs: Vec<Run>,
}

impl Row {
    pub fn plain(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            runs: vec![Run::new(text, tone)],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }

    /// The row's text, tones dropped: what a snapshot test compares.
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// The display width, over every run.
    pub fn width(&self) -> usize {
        self.runs
            .iter()
            .map(|run| {
                run.text
                    .chars()
                    .map(|c| c.width().unwrap_or(0))
                    .sum::<usize>()
            })
            .sum()
    }

    pub fn line(
        &self,
        theme: Theme,
        highlighter: &dyn Highlighter,
    ) -> ratatui::text::Line<'static> {
        let runs = highlighter.highlight(self);
        let spans = runs
            .iter()
            .map(|run| {
                ratatui::text::Span::styled(run.text.clone(), self.style_of(theme, run.tone))
            })
            .collect::<Vec<_>>();
        ratatui::text::Line::from(spans)
    }

    fn style_of(&self, theme: Theme, tone: Tone) -> ratatui::style::Style {
        match tone {
            Tone::Prose => theme.prose(),
            Tone::Strong => theme.strong(),
            Tone::Emphasis => theme.italic(),
            Tone::Strike => theme.strike(),
            Tone::Code => theme.code(),
            Tone::Link => theme.link(),
            Tone::H1 => theme.h1(),
            Tone::H2 => theme.h2(),
            Tone::H3 => theme.h3(),
            Tone::Dim => theme.dim(),
            Tone::Muted => theme.dim(),
            Tone::Done => theme.ok(),
            Tone::Label => theme.dim(),
        }
    }
}

/// The seam for a later optional syntax highlighter. The default does nothing.
pub trait Highlighter {
    fn highlight(&self, row: &Row) -> Vec<Run>;
}

/// No highlighting: fenced code is dim and monochrome in v1.
pub struct NoHighlight;

impl Highlighter for NoHighlight {
    fn highlight(&self, row: &Row) -> Vec<Run> {
        row.runs.clone()
    }
}

/// How wide a document is rendered, and whether tones are mapped to styles at
/// all (`markdown = false` keeps the text, drops the model's structure).
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub width: usize,
    /// The glyph set: quotes, bullets, the code gutter, the ellipsis and the table borders come from here,
    /// so `KELIR_ASCII`/`TERM=dumb` reaches the rendered text too (a Unicode table would be mojibake on a
    pub glyphs: crate::theme::Glyphs,
}

impl Layout {
    pub fn new(width: usize) -> Self {
        Self::with_glyphs(width, crate::theme::Glyphs::unicode())
    }

    pub fn with_glyphs(width: usize, glyphs: crate::theme::Glyphs) -> Self {
        Self {
            width: width.max(1),
            glyphs,
        }
    }
}

fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

fn text_width(text: &str) -> usize {
    text.chars().map(width_of).sum()
}

fn parser() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options
}

/// Render markdown into rows no wider than `layout.width`.
pub fn render(text: &str, layout: Layout) -> Vec<Row> {
    let mut stream = Stream::new();
    let mut blocks = stream.push(text);
    blocks.extend(stream.finish());
    let mut rows = Vec::new();
    for block in join_blocks(blocks) {
        if !rows.is_empty() {
            rows.push(Row::default());
        }
        rows.extend(render_block(&block, layout));
    }
    rows
}

/// The blocks a splitter cut, with a loose list item's continuation joined back on.
pub fn join_blocks(blocks: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for block in blocks {
        let previous_open = out.last().is_some_and(|last| ends_in_item(last));
        match out.last_mut() {
            Some(last) if previous_open && continues_item(&block) => {
                last.push_str("\n\n");
                last.push_str(&block);
            }
            _ => out.push(block),
        }
    }
    out
}

fn continues_item(block: &str) -> bool {
    let first = block
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    first.starts_with("  ") || first.starts_with('\t')
}

/// Render one block (a paragraph, a fence, a table…) as the streaming path
/// does: same function, same rules.
pub fn render_block(text: &str, layout: Layout) -> Vec<Row> {
    Renderer::render_with_open(text, layout).0
}

enum ListKind {
    Bullet { level: usize },
    Ordered { next: u64, level: usize },
}

impl ListKind {
    fn level(&self) -> usize {
        match self {
            ListKind::Bullet { level } | ListKind::Ordered { level, .. } => *level,
        }
    }
}

struct Renderer {
    layout: Layout,
    theme_glyphs: crate::theme::Glyphs,
    rows: Vec<Row>,
    line: Vec<Run>,
    tones: Vec<Tone>,
    lists: Vec<ListKind>,
    quote: usize,
    item_prefix: Option<Vec<Run>>,
    heading: Option<Tone>,
    code: Option<Code>,
    table: Option<TableState>,
    /// The parser is inside a link: its destination, for the `(url)` suffix.
    link: Option<String>,
    image_alt: Option<String>,
    /// Just after a paragraph/heading inside a list item: the next line is that
    /// item's continuation, not a new item.
    in_item: bool,
    /// The prefix a not-yet-flushed line inherits: a list item's first marker or its hanging indent.
    pending_prefix: Option<(Vec<Run>, Vec<Run>)>,
    /// The quote bar and indent a fenced block inside a list item carries.
    code_prefix: Option<(Vec<Run>, Vec<Run>)>,
    /// A chunk cut out of the middle of a long list counts on from where the
    /// items before it left off, so `3.` still reads `3.`.
    first_number: Option<u64>,
}

struct Code {
    language: String,
    lines: Vec<String>,
    /// Nothing but the opening fence was seen so far.
    empty: bool,
}

/// A table under construction: the header cells, the body rows, the row being
/// filled and the cell being filled.
#[derive(Default)]
struct TableState {
    head: Vec<Vec<Run>>,
    body: Vec<Vec<Vec<Run>>>,
    row: Vec<Vec<Run>>,
    in_head: bool,
    cell: Vec<Run>,
}

impl Renderer {
    fn new(layout: Layout) -> Self {
        let glyphs = layout.glyphs;
        Self {
            layout,
            theme_glyphs: glyphs,
            rows: Vec::new(),
            line: Vec::new(),
            tones: Vec::new(),
            lists: Vec::new(),
            quote: 0,
            item_prefix: None,
            heading: None,
            code: None,
            table: None,
            link: None,
            image_alt: None,
            in_item: false,
            pending_prefix: None,
            code_prefix: None,
            first_number: None,
        }
    }

    fn document(&mut self, text: &str) {
        for event in Parser::new_ext(text, parser()) {
            self.event(event);
        }
    }

    /// Whether the next container-level line belongs to a list item that is still open *and* outside an
    /// open fence: that one thing the block splitter cannot see, because it cuts at blank lines and the
    fn wants_more(&self) -> bool {
        !self.lists.is_empty() && self.code.is_none()
    }

    /// Render `block`, and say whether a following block may continue it: the block ended inside a list
    /// item (a loose item's blank line is a separator, not the end of the item, and CommonMark keeps the
    pub fn render_with(text: &str, layout: Layout, first: Option<u64>) -> Vec<Row> {
        let mut renderer = Renderer::new(layout);
        renderer.first_number = first;
        renderer.document(text);
        renderer.finish()
    }

    pub fn render_with_open(text: &str, layout: Layout) -> (Vec<Row>, bool) {
        let mut renderer = Renderer::new(layout);
        renderer.document(text);
        let open = renderer.wants_more();
        (renderer.finish(), open)
    }

    fn finish(mut self) -> Vec<Row> {
        self.flush_line();
        while self.rows.last().is_some_and(Row::is_empty) {
            self.rows.pop();
        }
        self.rows
    }

    fn tone(&self) -> Tone {
        self.tones.last().copied().unwrap_or(Tone::Prose)
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let tone = self.tone();
        if let Some(table) = &mut self.table {
            table.cell.push(Run::new(text, tone));
            return;
        }
        if let Some(code) = &mut self.code {
            code.lines.extend(text.split('\n').map(str::to_string));
            code.empty = false;
            return;
        }
        self.line.push(Run::new(text, self.tone()));
    }

    /// The prefix every row of the current block carries: quote bars, then the
    /// list indent.
    fn prefixes(&mut self) -> (Vec<Run>, Vec<Run>) {
        if self.layout.width < 4 {
            // A quote bar is two columns: at three columns or fewer it would
            // eat the text it is meant to decorate.
            self.item_prefix = None;
            self.pending_prefix = None;
            return (Vec::new(), Vec::new());
        }
        let mut bar = Vec::new();
        for _ in 0..self.quote {
            bar.push(Run::new(self.theme_glyphs.quote_bar(), Tone::Dim));
        }
        // The line being flushed is either the item's first (marker) or one of
        // its continuations (the indent the previous flush remembered).
        let with = |bar: &[Run], prefix: &[Run]| {
            let mut runs = bar.to_vec();
            runs.extend(prefix.to_vec());
            runs
        };
        let hang = |bar: &[Run], prefix: &[Run]| {
            let mut runs = bar.to_vec();
            let width: usize = prefix.iter().map(|run| text_width(&run.text)).sum();
            runs.push(Run::new(" ".repeat(width), Tone::Dim));
            runs
        };
        let (first, cont) = match (&self.item_prefix, &self.pending_prefix) {
            (Some(marker), _) => (with(&bar, marker), hang(&bar, marker)),
            (None, Some((first, cont))) => {
                let first = if first.is_empty() {
                    bar.clone()
                } else {
                    first.clone()
                };
                let cont = if cont.is_empty() {
                    bar.clone()
                } else {
                    cont.clone()
                };
                (first, cont)
            }
            (None, None) => (bar.clone(), bar),
        };
        // The prefix must leave room for text: at a narrow width a deep list's indent alone can be wider than
        // the pane, and the ellipsis of a clipped row then overflowed it.
        let room = self.layout.width.saturating_sub(1).max(1);
        (fit_prefix(first, room), fit_prefix(cont, room))
    }

    /// The hanging indent for a list item's marker: the quote bar, then spaces as wide as the marker, so a
    /// continuation paragraph sits under the item's text and not under a second bullet.
    fn hang(&self, marker: &[Run]) -> Vec<Run> {
        let mut runs = Vec::new();
        for _ in 0..self.quote {
            runs.push(Run::new(self.theme_glyphs.quote_bar(), Tone::Dim));
        }
        let width: usize = marker.iter().map(|run| text_width(&run.text)).sum();
        runs.push(Run::new(" ".repeat(width), Tone::Dim));
        runs
    }

    /// A block starts: nothing to do, the caller puts the blank rows between
    /// blocks (so streaming and one-shot join them identically).
    fn start_block(&mut self) {}

    fn paragraph_start(&mut self) {
        self.start_block();
        let continues_item = self.in_item && self.pending_prefix.is_some();
        let last_is_blank = self.rows.last().is_none_or(Row::is_empty);
        if continues_item && !last_is_blank {
            self.rows.push(Row::default());
        }
    }

    fn end_block(&mut self) {
        self.flush_line();
    }

    fn flush_line(&mut self) {
        if self.line.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.line);
        let (first, cont) = self.prefixes();
        let rows = wrap_runs(&first, &cont, &runs, self.layout.width);
        // A following line of the same item (its second paragraph) hangs under
        // the item's marker, never under a bullet of its own.
        let hanging = match &self.item_prefix {
            Some(prefix) => self.hang(prefix),
            None => cont.clone(),
        };
        // The item's marker is spent; the hang stays for its continuation.
        self.item_prefix = None;
        self.pending_prefix = Some((hanging.clone(), hanging));
        self.rows.extend(rows);
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some(alt) = &mut self.image_alt {
                    alt.push_str(&text);
                } else {
                    self.push(&text);
                }
            }
            Event::Code(text) => self.push_styled(&text, Tone::Code),
            Event::Html(text) | Event::InlineHtml(text) => {
                // Raw HTML is shown literally (never interpreted), one row per
                // source line.
                let mut lines = text.split('\n').peekable();
                while let Some(line) = lines.next() {
                    if !line.is_empty() {
                        self.push_styled(line, Tone::Dim);
                    }
                    if lines.peek().is_some() {
                        self.flush_line();
                    }
                }
            }
            Event::SoftBreak => self.push(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.end_block();
                let rule = self.theme_glyphs.rule();
                let width = self
                    .layout
                    .width
                    .saturating_sub(self.quote * 2)
                    .clamp(1, 60);
                let mut row = Row::plain(rule.to_string().repeat(width), Tone::Dim);
                if self.quote > 0 {
                    let mut runs = vec![Run::new(self.theme_glyphs.quote_bar(), Tone::Dim)];
                    runs.append(&mut row.runs);
                    row.runs = runs;
                }
                self.rows.push(row);
            }
            Event::TaskListMarker(done) => {
                let marker = if done { "[x] " } else { "[ ] " };
                self.line.push(Run::new(
                    marker,
                    if done { Tone::Done } else { Tone::Muted },
                ));
            }
            Event::FootnoteReference(name) => self.push_styled(&format!("[{name}]"), Tone::Dim),
            Event::InlineMath(text) => self.push_styled(&text, Tone::Code),
            Event::DisplayMath(text) => self.push_styled(&text, Tone::Code),
        }
    }

    fn push_styled(&mut self, text: &str, tone: Tone) {
        if text.is_empty() {
            return;
        }
        if let Some(table) = &mut self.table {
            table.cell.push(Run::new(text, tone));
            return;
        }
        self.line.push(Run::new(text, tone));
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.paragraph_start(),
            Tag::Heading { level, .. } => {
                self.start_block();
                self.heading = Some(match level {
                    HeadingLevel::H1 => Tone::H1,
                    HeadingLevel::H2 => Tone::H2,
                    _ => Tone::H3,
                });
                self.tones.push(self.heading.unwrap_or(Tone::H3));
            }
            Tag::BlockQuote(_) => {
                if self.quote == 0 {
                    self.start_block();
                }
                self.quote += 1;
            }
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.start_block();
                }
                let level = self.lists.len();
                self.lists.push(match start {
                    Some(next) => ListKind::Ordered {
                        next: match self.first_number.take() {
                            Some(first) if self.lists.is_empty() => first,
                            _ => next,
                        },
                        level,
                    },
                    None => ListKind::Bullet { level },
                });
            }
            Tag::Item => {
                self.flush_line();
                // A new item starts its own prefix; nothing carries over.
                self.pending_prefix = None;
                let level = self.lists.last().map(ListKind::level).unwrap_or(0);
                let indent = "  ".repeat(level);
                let marker = match self.lists.last_mut() {
                    Some(ListKind::Ordered { next, .. }) => {
                        let marker = format!("{indent}{next}. ");
                        *next += 1;
                        Run::new(marker, Tone::Dim)
                    }
                    _ => Run::new(
                        format!("{indent}{}", self.theme_glyphs.bullet(level)),
                        Tone::Dim,
                    ),
                };
                self.item_prefix = Some(vec![marker]);
                self.in_item = true;
            }
            Tag::CodeBlock(kind) => {
                self.end_block();
                let language = match kind {
                    CodeBlockKind::Fenced(language) => language.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                // A fence inside a list item keeps the item's indent.
                self.code_prefix = self.pending_prefix.clone();
                self.code = Some(Code {
                    language,
                    lines: Vec::new(),
                    empty: true,
                });
            }
            Tag::Emphasis => self.tones.push(Tone::Emphasis),
            Tag::Strong => self.tones.push(Tone::Strong),
            Tag::Strikethrough => self.tones.push(Tone::Strike),
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.tones.push(Tone::Link);
            }
            Tag::Image { dest_url, .. } => {
                self.image_alt = Some(String::new());
                self.link = Some(dest_url.to_string());
            }
            Tag::Table(_) => {
                self.start_block();
                self.table = Some(TableState::default());
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell.clear();
                }
            }
            Tag::FootnoteDefinition(name) => {
                self.start_block();
                self.line.push(Run::new(format!("[{name}] "), Tone::Dim));
            }
            Tag::HtmlBlock => self.start_block(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
            }
            TagEnd::Heading(_) => {
                self.flush_line();
                self.tones.pop();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.in_item = false;
                    self.pending_prefix = None;
                }
            }
            TagEnd::Item => {
                self.flush_line();
                self.in_item = false;
                self.pending_prefix = None;
            }
            TagEnd::CodeBlock => self.finish_code(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.tones.pop();
            }
            TagEnd::Link => {
                self.tones.pop();
                if let Some(url) = self.link.take() {
                    let text = self
                        .line
                        .iter()
                        .map(|run| run.text.as_str())
                        .collect::<String>();
                    if !url.is_empty() && url != text {
                        self.line.push(Run::new(format!(" ({url})"), Tone::Dim));
                    }
                }
            }
            TagEnd::Image => {
                let alt = self.image_alt.take().unwrap_or_default();
                self.link = None;
                let label = if alt.trim().is_empty() {
                    "[image]".to_string()
                } else {
                    format!("[image: {alt}]")
                };
                self.line.push(Run::new(label, Tone::Dim));
            }
            TagEnd::Table => self.finish_table(),
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = false;
                    table.head = std::mem::take(&mut table.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.body.push(row);
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let cell = std::mem::take(&mut table.cell);
                    table.row.push(cell);
                }
            }
            TagEnd::FootnoteDefinition => self.flush_line(),
            TagEnd::HtmlBlock => self.flush_line(),
            _ => {}
        }
    }

    /// A fenced block: an optional dim language label, then every source line with a gutter,
    /// hard-clipped rather than re-wrapped (a wrapped command is not a command you can copy).
    fn finish_code(&mut self) {
        let Some(code) = self.code.take() else {
            return;
        };
        let gutter = self.theme_glyphs.code_gutter();
        let mut bar = String::new();
        for _ in 0..self.quote {
            bar.push_str(self.theme_glyphs.quote_bar());
        }
        // A fence inside a list item sits at the item's indent (the hanging
        // indent its own paragraph rows use).
        let indent = self
            .code_prefix
            .take()
            .map(|(_, cont)| {
                let width: usize = cont.iter().map(|run| text_width(&run.text)).sum();
                " ".repeat(width)
            })
            .unwrap_or_default();
        let mut prefix = format!("{bar}{indent}{gutter}");
        if text_width(&prefix) + 2 > self.layout.width {
            // At two or three columns a gutter would leave no room for code.
            prefix.clear();
        }
        let prefix_width = text_width(&prefix);
        if !code.language.trim().is_empty() {
            // The label sits at the item's indent too, like the code under it: a fence
            // inside a list item is the item's, not the document's.
            self.rows.push(Row::plain(
                format!("{bar}{indent}{}", code.language.trim()),
                Tone::Label,
            ));
        }
        let mut lines = code.lines;
        // A fence's text ends with a newline, which the splitter turns into one
        // trailing empty line: not a row.
        while lines.len() > 1 && lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let room = self.layout.width.saturating_sub(prefix_width);
        for line in lines {
            let clipped = clip_columns(&line, room.max(1), self.theme_glyphs.ellipsis());
            self.rows
                .push(Row::plain(format!("{prefix}{clipped}"), Tone::Dim));
        }
    }

    fn finish_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        for row in render_table(&table, self.layout, self.theme_glyphs) {
            self.rows.push(row);
        }
    }
}

// ---------------------------------------------------------------- wrapping

/// Wrap `runs` to `width`, prefixing the first row with `first` and every continuation row with `cont`.
#[optimize(speed)]
pub fn wrap_runs(first: &[Run], cont: &[Run], runs: &[Run], width: usize) -> Vec<Row> {
    let width = width.max(1);
    let first_width = prefix_width(first);
    let cont_width = prefix_width(cont);
    if width <= cont_width {
        // Narrower than the indent itself: show the prefix and clip the text.
        let room = width.saturating_sub(cont_width).max(1);
        let text = runs.iter().map(|run| run.text.as_str()).collect::<String>();
        let mut row = Row {
            runs: cont.to_vec(),
        };
        row.runs.push(Run::new(
            clip_columns(&text, room, '…'),
            runs.first().map(|run| run.tone).unwrap_or(Tone::Prose),
        ));
        return vec![row];
    }
    let cells: Vec<(char, Tone)> = runs
        .iter()
        .flat_map(|run| run.text.chars().map(move |c| (c, run.tone)))
        .collect();
    let mut rows = Vec::new();
    let mut rest: &[(char, Tone)] = &cells;
    let mut prefix = first;
    let mut room = width - first_width;
    while !rest.is_empty() {
        // How many characters fit in `room` columns.
        let mut used = 0usize;
        let mut fit = 0usize;
        for (c, _) in rest {
            let c_width = width_of(*c);
            if used + c_width > room && fit > 0 {
                break;
            }
            used += c_width;
            fit += 1;
        }
        if fit == rest.len() {
            let mut row = Row {
                runs: prefix.to_vec(),
            };
            row.runs.extend(coalesce(rest));
            trim_end(&mut row);
            rows.push(row);
            break;
        }
        // Break at the last space inside the window, else mid-word.
        let window = &rest[..fit];
        let (take, skip) = match window.iter().rposition(|(c, _)| *c == ' ') {
            Some(space) if space > 0 => (space, 1),
            _ => (fit, 0),
        };
        let mut row = Row {
            runs: prefix.to_vec(),
        };
        row.runs.extend(coalesce(&rest[..take]));
        trim_end(&mut row);
        rows.push(row);
        rest = &rest[take + skip..];
        while rest.first().is_some_and(|(c, _)| *c == ' ') {
            rest = &rest[1..];
        }
        prefix = cont;
        room = width.saturating_sub(cont_width);
    }
    if rows.is_empty() {
        rows.push(Row {
            runs: first.to_vec(),
        });
    }
    rows
}

/// Cut `prefix` back to `room` columns, dropping whole characters from its end.
/// An indent is decoration; the text it decorates is not.
fn fit_prefix(prefix: Vec<Run>, room: usize) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut used = 0usize;
    for run in prefix {
        let mut text = String::new();
        for c in run.text.chars() {
            let width = width_of(c);
            if used + width > room {
                break;
            }
            used += width;
            text.push(c);
        }
        if !text.is_empty() {
            out.push(Run::new(text, run.tone));
        }
        if used >= room {
            break;
        }
    }
    out
}

fn prefix_width(prefix: &[Run]) -> usize {
    prefix.iter().map(|run| text_width(&run.text)).sum()
}

/// Consecutive characters with one tone, as runs.
fn coalesce(cells: &[(char, Tone)]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (c, tone) in cells {
        match runs.last_mut() {
            Some(last) if last.tone == *tone => last.text.push(*c),
            _ => runs.push(Run::new(c.to_string(), *tone)),
        }
    }
    runs
}

fn trim_end(row: &mut Row) {
    while let Some(last) = row.runs.last_mut() {
        let trimmed = last.text.trim_end().len();
        if trimmed == 0 {
            row.runs.pop();
        } else {
            last.text.truncate(trimmed);
            break;
        }
    }
}

/// Clip `text` to `width` columns with `marker` at the end when something was
/// cut.
pub fn clip_columns(text: &str, width: usize, marker: char) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let c_width = width_of(c);
        if used + c_width > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += c_width;
    }
    out.push(marker);
    out
}

// ------------------------------------------------------------------ tables

/// A table, boxed when it fits and as plain `a | b | c` rows when it does not
/// (narrow panes and wide records), per's rule.
fn render_table(table: &TableState, layout: Layout, glyphs: crate::theme::Glyphs) -> Vec<Row> {
    let columns = table
        .body
        .iter()
        .chain(std::iter::once(&table.head))
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let mut widths = vec![1usize; columns];
    for (index, cell) in table.head.iter().enumerate() {
        widths[index] = widths[index].max(cell_width(cell));
    }
    for row in &table.body {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell_width(cell));
        }
    }
    let boxed = layout.width >= 30;
    // Total width a boxed table needs: the borders, the padding and the cells.
    let borders = if boxed {
        columns * 3 + 1
    } else {
        columns * 3 - 1
    };
    if boxed && widths.iter().sum::<usize>() + borders <= layout.width {
        boxed_table(table, &widths, glyphs)
    } else {
        // Shrink the widest columns first, down to 4 columns each.
        let mut widths = widths.clone();
        let budget = layout.width.saturating_sub(borders);
        while widths.iter().sum::<usize>() > budget {
            let Some((index, _)) = widths
                .iter()
                .enumerate()
                .filter(|(_, width)| **width > 4)
                .max_by_key(|(_, width)| **width)
            else {
                break;
            };
            widths[index] -= 1;
        }
        if boxed && widths.iter().sum::<usize>() + borders <= layout.width {
            boxed_table(table, &widths, glyphs)
        } else {
            plain_table(table, layout.width, glyphs)
        }
    }
}

fn cell_width(cell: &[Run]) -> usize {
    cell.iter().map(|run| text_width(&run.text)).sum()
}

fn boxed_table(table: &TableState, widths: &[usize], glyphs: crate::theme::Glyphs) -> Vec<Row> {
    let pieces = glyphs.table();
    let mut rows = Vec::new();
    rows.push(border(widths, pieces.tl, pieces.tm, pieces.tr, pieces.h));
    if !table.head.is_empty() {
        rows.push(table_row(
            &table.head,
            widths,
            pieces.v,
            Some(Tone::Strong),
            glyphs,
        ));
        rows.push(border(widths, pieces.ml, pieces.mm, pieces.mr, pieces.h));
    }
    for row in &table.body {
        rows.push(table_row(row, widths, pieces.v, None, glyphs));
    }
    rows.push(border(widths, pieces.bl, pieces.bm, pieces.br, pieces.h));
    rows
}

fn border(widths: &[usize], left: char, mid: char, right: char, horizontal: char) -> Row {
    let mut text = String::new();
    text.push(left);
    for (index, width) in widths.iter().enumerate() {
        text.push_str(&horizontal.to_string().repeat(width + 2));
        text.push(if index + 1 == widths.len() {
            right
        } else {
            mid
        });
    }
    Row::plain(text, Tone::Dim)
}

fn table_row(
    cells: &[Vec<Run>],
    widths: &[usize],
    separator: char,
    tone: Option<Tone>,
    glyphs: crate::theme::Glyphs,
) -> Row {
    let mut runs = vec![Run::new(separator.to_string(), Tone::Dim)];
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let mut text = String::new();
        for run in cell {
            text.push_str(&run.text);
        }
        text = text.replace('\n', " ");
        let clipped = clip_columns(&text, *width, glyphs.ellipsis());
        let pad = width.saturating_sub(text_width(&clipped));
        let cell_tone = tone
            .or_else(|| cell.first().map(|run| run.tone))
            .unwrap_or(Tone::Prose);
        runs.push(Run::new(
            format!(" {clipped}{} ", " ".repeat(pad)),
            cell_tone,
        ));
        runs.push(Run::new(separator.to_string(), Tone::Dim));
    }
    Row { runs }
}

/// No borders: `a | b | c` with a bold header row. Cells are clipped so one
/// record stays one row, which is what makes a table readable at 40 columns.
fn plain_table(table: &TableState, width: usize, glyphs: crate::theme::Glyphs) -> Vec<Row> {
    if width < 8 {
        // Below a single cell plus one separator there is no table to show:
        // one clipped cell per row is the honest degradation.
        let mut rows = Vec::new();
        for cells in std::iter::once(&table.head).chain(table.body.iter()) {
            let mut text = String::new();
            for cell in cells {
                for run in cell {
                    text.push_str(&run.text);
                }
                text.push(' ');
            }
            let text = text.trim_end().replace('\n', " ");
            rows.push(Row::plain(
                clip_columns(&text, width, glyphs.ellipsis()),
                Tone::Prose,
            ));
        }
        return rows;
    }
    plain_table_rows(table, width, glyphs)
}

fn plain_table_rows(table: &TableState, width: usize, glyphs: crate::theme::Glyphs) -> Vec<Row> {
    // The cells and the ` | ` between them must fit the pane, not the other way round: a fixed minimum
    // cell width made a 3-column table one column too wide at narrow widths.
    let separators = 3usize;
    let mut columns = table
        .body
        .iter()
        .chain(std::iter::once(&table.head))
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(1);
    // As many columns as fit with at least two columns of text each; one when
    // even that is too wide.
    while columns > 1 && columns * 2 + (columns - 1) * separators > width {
        columns -= 1;
    }
    let budget = width
        .saturating_sub((columns - 1) * separators)
        .max(columns);
    let each = (budget / columns).max(1);
    let cells =
        |cells: &[Vec<Run>]| -> Vec<Vec<Run>> { cells.iter().take(columns).cloned().collect() };
    let mut rows = Vec::new();
    if !table.head.is_empty() {
        rows.push(plain_row(&cells(&table.head), each, true, glyphs));
    }
    for row in &table.body {
        rows.push(plain_row(&cells(row), each, false, glyphs));
    }
    rows
}

fn plain_row(cells: &[Vec<Run>], each: usize, strong: bool, glyphs: crate::theme::Glyphs) -> Row {
    let mut runs = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            runs.push(Run::new(" | ", Tone::Dim));
        }
        let mut text = String::new();
        for run in cell {
            text.push_str(&run.text);
        }
        let text = text.replace('\n', " ");
        let tone = if strong {
            Tone::Strong
        } else {
            cell.first().map(|run| run.tone).unwrap_or(Tone::Prose)
        };
        runs.push(Run::new(clip_columns(&text, each, glyphs.ellipsis()), tone));
    }
    Row { runs }
}

// ---------------------------------------------------------------- streaming

/// A block the splitter has finished, and how many of its rows were already committed while it streamed
/// (`S1b`): the TUI continues the block from there instead of writing those rows again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    pub text: String,
    pub delivered: usize,
}

impl From<Closed> for String {
    fn from(closed: Closed) -> Self {
        closed.text
    }
}

/// Splits streamed markdown into complete blocks.
#[derive(Debug, Default)]
pub struct Stream {
    /// The block being collected, its last line included but without that
    /// line's newline.
    open: String,
    /// The open fence's marker (` ``` ` or `~~~ ` plus its info string), while
    /// inside one.
    in_fence: Option<String>,
    /// The open block's last line looks like a table row, so a following row
    /// belongs to it.
    in_table: bool,
    /// The line being collected (the last line of `open`, without its newline): the block's
    /// structure is decided from this, never from the whole block.
    line: String,
    /// What the open block is (fence, list, prose): decided once its first line
    /// is complete, and what decides which of its rows are final early (`S1b`).
    shape: Option<Shape>,
    /// How many bytes of `open` are already out, and in how many rows: what the
    /// block is continued from when it closes, so no row is written twice.
    delivered_bytes: usize,
    delivered_rows: usize,
}

impl Stream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &str) -> Vec<String> {
        let mut done: Vec<String> = Vec::new();
        for line in delta.split_inclusive('\n') {
            let newline = line.ends_with('\n');
            let text = line.trim_end_matches('\n');
            self.open.push_str(text);
            self.line.push_str(text);
            if newline {
                self.line_ended(&mut done);
            }
        }
        done
    }

    /// Feed a delta; returns the blocks it completed, each with the rows of it
    /// that were already delivered while it streamed.
    pub fn push_blocks(&mut self, delta: &str) -> Vec<Closed> {
        let mut done = Vec::new();
        for line in delta.split_inclusive('\n') {
            let newline = line.ends_with('\n');
            let text = line.trim_end_matches('\n');
            self.open.push_str(text);
            self.line.push_str(text);
            if newline {
                self.line_ended(&mut done);
            }
        }
        done
    }

    fn line_ended<T: From<Closed>>(&mut self, done: &mut Vec<T>) {
        let line = std::mem::take(&mut self.line);
        if let Some(fence) = self.in_fence.clone() {
            if opens_or_closes(&line, &fence) {
                self.in_fence = None;
                // The closing fence belongs to this block.
                self.hand_over(done);
                self.in_table = false;
            } else {
                self.open.push('\n');
            }
            return;
        }
        if let Some(fence) = fence_at(&line) {
            self.in_fence = Some(fence);
            self.open.push('\n');
            return;
        }
        if line.trim().is_empty() {
            // A blank line closes the block. A table's blank line closes it too: the table is complete,
            // and its widths need every row it has, which it now has.
            if !self.open.trim().is_empty() {
                self.hand_over(done);
            }
            self.open.clear();
            self.reset_block();
            self.in_table = false;
            return;
        }
        self.in_table = is_table_row(&line);
        self.open.push('\n');
    }

    /// The open block's text, exactly as it arrived (no synthetic closer).
    pub fn open(&self) -> &str {
        &self.open
    }

    /// The block that just closed: its text, and the rows of it that are already
    /// out (committed while it streamed).
    fn hand_over<T: From<Closed>>(&mut self, done: &mut Vec<T>) {
        let rows = self.delivered_rows;
        self.delivered_bytes = 0;
        self.delivered_rows = 0;
        let text = std::mem::take(&mut self.open);
        self.reset_block();
        done.push(T::from(Closed {
            text,
            delivered: rows,
        }));
    }

    /// A new block starts: nothing of the previous one carries over.
    fn reset_block(&mut self) {
        self.shape = None;
        self.delivered_bytes = 0;
        self.delivered_rows = 0;
    }

    /// What the open block is, once its first line is complete.
    pub fn shape(&self) -> Option<&Shape> {
        self.shape.as_ref()
    }

    /// How many bytes of the open block are already out.
    pub fn delivered_bytes(&self) -> usize {
        self.delivered_bytes
    }

    /// The rows of the open block that can be committed now (`S1b`): a fence's finished lines and a top-
    /// level list's finished items, keeping `keep` complete lines behind for the live area.
    pub fn settle(&mut self, layout: Layout, keep: usize) -> Vec<Row> {
        if self.shape.is_none() {
            self.shape = Shape::of(&self.open);
        }
        let Some(shape) = self.shape.clone() else {
            return Vec::new();
        };
        let in_fence = self.in_fence.is_some();
        let Some(cut) = stable_cut(&self.open, self.delivered_bytes, &shape, in_fence, keep) else {
            return Vec::new();
        };
        if cut <= self.delivered_bytes {
            return Vec::new();
        }
        let rows = piece_rows(&self.open, self.delivered_bytes, cut, &shape, layout);
        self.delivered_bytes = cut;
        self.delivered_rows += rows.len();
        rows
    }

    pub fn delivered_rows(&self) -> usize {
        self.delivered_rows
    }

    /// The block being streamed: rendered tolerantly (an unclosed fence shows
    /// as code, an unclosed `**` stays literal).
    pub fn tail(&self) -> String {
        let text = self.open.trim_end_matches('\n');
        if self.in_fence.is_some() {
            // An unfinished fence still renders as code: give the renderer the closing fence the stream
            // has not sent yet. One small string per frame is cheaper than leaking one per frame.
            format!(
                "{text}
```"
            )
        } else {
            text.to_string()
        }
    }

    /// The part of the open block not yet committed, rendered tolerantly.
    /// Keeping only this tail in the live draft makes a long stream linear in
    /// the number of deltas rather than quadratic in the message length.
    pub fn tail_remaining(&self) -> String {
        let mut text = self
            .open
            .get(self.delivered_bytes..)
            .unwrap_or("")
            .trim_end_matches('\n');
        // A paragraph's wrap depends on all its text, so `settle` cannot commit
        // rows early. Keep only its tail in the live draft: the whole block
        // lands in the transcript when it closes, and a huge prose delta does
        // not re-render a growing megabyte on every frame.
        if !text.is_empty()
            && self
                .shape
                .as_ref()
                .is_none_or(|s| matches!(s, Shape::Whole))
        {
            let mut cut = 0usize;
            for (i, _) in text.char_indices().rev() {
                if text.len() - i > 4096 {
                    cut = i;
                    break;
                }
            }
            text = &text[cut..];
        }
        if self.in_fence.is_some() {
            format!("{text}\n```")
        } else {
            text.to_string()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.open.trim().is_empty()
    }

    /// The stream is complete: whatever is left is the last block.
    pub fn finish(&mut self) -> Option<String> {
        self.finish_block().map(|closed| closed.text)
    }

    /// The stream is complete: the last block, with the rows of it that already
    /// went out while it streamed.
    pub fn finish_block(&mut self) -> Option<Closed> {
        self.in_fence = None;
        self.in_table = false;
        self.shape = None;
        if self.open.trim().is_empty() {
            self.open.clear();
            self.reset_block();
            return None;
        }
        let rows = self.delivered_rows;
        self.delivered_bytes = 0;
        self.delivered_rows = 0;
        Some(Closed {
            text: std::mem::take(&mut self.open),
            delivered: rows,
        })
    }
}

/// The fence an opening line starts (` ```rust ` → ` ```rust `), if it is one.
fn fence_at(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for marker in ["```", "~~~"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(format!("{marker}{}", rest.trim()));
        }
    }
    None
}

/// Is this line the closing fence of `open`? A closing fence repeats the
/// marker and carries no info string.
fn opens_or_closes(line: &str, open: &str) -> bool {
    let trimmed = line.trim_start();
    let marker = if open.starts_with("~~~") {
        "~~~"
    } else {
        "```"
    };
    match trimmed.strip_prefix(marker) {
        Some(rest) => rest.trim().is_empty(),
        None => false,
    }
}

/// Does this block end inside a list item that a following, indented block continues? (The renderer
/// knows: the item's container is still open at the end of the block — a loose item's blank line does
pub fn ends_in_item(text: &str) -> bool {
    // The splitter's cut is a blank line: the one place where CommonMark's *loose* list rule matters.
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    opens_item(text) || last.starts_with("  ") || last.starts_with("	")
}

/// Does this block open with a list item (`- x`, `* x`, `1. x`, an ordered
/// item)? Two of those in a row are two lists, never one item's continuation.
pub fn opens_item(block: &str) -> bool {
    let line = block
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars().peekable();
    match chars.peek() {
        Some('-') | Some('*') | Some('+') => {
            chars.next();
            chars.next().is_some_and(char::is_whitespace)
        }
        Some(c) if c.is_ascii_digit() => {
            let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
            let rest = &trimmed[digits.len()..];
            rest.starts_with(". ") || rest.starts_with(") ")
        }
        _ => false,
    }
}

/// A line that looks like a table row: pipes on both ends.
fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() > 1 && trimmed.starts_with('|') && trimmed.ends_with('|')
}

/// Is this text a table, or the start of one? A boxed table's column widths are the widest
/// cell of every column, so no row of it is final before its last row has arrived.
pub fn looks_like_table(text: &str) -> bool {
    text.lines().any(is_table_row)
}

/// Does this line open a list item at column 0? An item's own continuation (an
/// indented line) is not one: its rows depend on the item it belongs to.
fn is_top_item(line: &str) -> bool {
    !line.starts_with([' ', '\t']) && opens_item(line)
}

/// The shape of a block being streamed: which of its rows are final before the
/// block ends (`S1b`), and how a piece of it renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A fence. Every completed line is one row (code is clipped, never
    /// wrapped), so a line that ended can go out before the closing fence.
    Fence { marker: String, label: String },
    /// A top-level list. An item that ended cannot change: its wrapping and its
    /// hanging indent depend on that item alone.
    List,
    /// Prose or a table: no row is final until the block ends (a paragraph's
    /// wrap depends on all of it, a table's widths on every row it has).
    Whole,
}

impl Shape {
    /// The shape of a block from its lines so far.
    pub fn of(text: &str) -> Option<Shape> {
        let head = text.split_inclusive('\n').next().unwrap_or_default();
        let line = head.strip_suffix('\n')?;
        if let Some(marker) = fence_at(line) {
            let marker: String = marker.chars().take(3).collect();
            let trimmed = line.trim_start();
            let label = trimmed[marker.len()..].trim().to_string();
            return Some(Shape::Fence { marker, label });
        }
        if is_top_item(line) {
            return Some(Shape::List);
        }
        Some(Shape::Whole)
    }
}

/// The bytes of a fence's opening line, marker and label: the block's first
/// line is a label row, not code.
fn opening_len(text: &str) -> usize {
    text.find('\n').map_or(0, |at| at + 1)
}

/// How many of the `text`'s completed lines are whole, and where each of them
/// ends.
fn complete_lines(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.ends_with('\n') {
            ends.push(offset);
        }
    }
    ends
}

/// How much of `text` (an open block, from `from` on) is final, keeping at least `keep`
/// complete lines after the cut for the live area.
pub fn stable_cut(
    text: &str,
    from: usize,
    shape: &Shape,
    in_fence: bool,
    keep: usize,
) -> Option<usize> {
    let rest = text.get(from..)?;
    if !in_fence && looks_like_table(rest) {
        return None;
    }
    let ends = complete_lines(rest);
    let keep = keep.max(1);
    match shape {
        Shape::Fence { .. } => {
            // Every complete line but the last `keep` (the one being typed is
            // not final, and a couple of rows stay for the live area).
            let index = ends.len().checked_sub(keep + 1)?;
            Some(from + ends[index])
        }
        Shape::List if !in_fence => {
            // The cut is just before a top-level item, with `keep` complete
            // lines after it: every item before that one can never change.
            let lines: Vec<&str> = rest.split_inclusive('\n').collect();
            let mut cut = None;
            for (index, end) in ends.iter().enumerate() {
                if ends.len() - 1 - index < keep {
                    break;
                }
                if lines
                    .get(index + 1)
                    .is_some_and(|next| next.strip_suffix('\n').is_some_and(is_top_item))
                {
                    cut = Some(from + end);
                }
            }
            cut
        }
        _ => None,
    }
}

/// How many top-level ordered items `text` holds: the numbering a piece cut out
/// of the middle of a long list counts on from.
pub fn ordered_items(text: &str) -> u64 {
    text.lines()
        .filter(|line| !line.starts_with([' ', '\t']))
        .filter(|line| {
            let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
            !digits.is_empty()
                && line[digits.len()..].starts_with(['.', ')'])
                && line[digits.len() + 1..].starts_with(' ')
        })
        .count() as u64
}

/// The number a list's first ordered item carries (`3. x` → 3, `- x` → 1).
fn list_start(text: &str) -> u64 {
    let line = text.lines().next().unwrap_or_default();
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(1)
}

/// The rows of the block `text[from..to]` — a piece of a block that is being streamed (`to` a
/// cut), or all of what is left when the block closes (`to == text.len()`).
pub fn piece_rows(text: &str, from: usize, to: usize, shape: &Shape, layout: Layout) -> Vec<Row> {
    let Some(piece) = text.get(from.min(text.len())..to.min(text.len())) else {
        return Vec::new();
    };
    match shape {
        Shape::Fence { marker, label } => {
            let (label, body) = if from == 0 {
                (
                    label.as_str(),
                    &piece[opening_len(piece).min(piece.len())..],
                )
            } else {
                ("", piece)
            };
            let mut text = String::with_capacity(marker.len() + label.len() + body.len() + 4);
            text.push_str(marker);
            text.push_str(label);
            text.push('\n');
            text.push_str(body);
            if !body.is_empty() && !body.ends_with('\n') {
                text.push('\n');
            }
            // A fence that has not closed yet still renders as code: give the renderer the closing line
            // the model has not sent, so the rows are the ones the block will have.
            text.push_str(marker);
            render_block(&text, layout)
        }
        Shape::List if from > 0 => {
            let first = list_start(text) + ordered_items(&text[..from]);
            Renderer::render_with(piece, layout, Some(first))
        }
        _ => render_block(piece, layout),
    }
}

/// The rows of the open block that are not out yet: what the live area shows while a block streams.
pub fn open_rows(text: &str, shape: Option<&Shape>, delivered: usize, layout: Layout) -> Vec<Row> {
    let from = delivered.min(text.len());
    match shape {
        Some(shape) => piece_rows(text, from, text.len(), shape, layout),
        // The block's first line is still arriving: render it tolerantly.
        None => render_block(text.trim_end_matches('\n'), layout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(text: &str, width: usize) -> Vec<String> {
        render(text, Layout::new(width))
            .into_iter()
            .map(|row| row.text())
            .collect()
    }

    fn tones_of(text: &str, width: usize) -> Vec<(String, Tone)> {
        render(text, Layout::new(width))
            .into_iter()
            .flat_map(|row| row.runs)
            .map(|run| (run.text, run.tone))
            .collect()
    }

    #[test]
    fn headings_lose_their_hashes_and_keep_their_level_in_the_tone() {
        let rows = render("# One\n\n## Two\n\n### Three\n", Layout::new(80));
        let texts: Vec<String> = rows.iter().map(Row::text).collect();
        assert_eq!(texts, vec!["One", "", "Two", "", "Three"]);
        let tones: Vec<Tone> = rows
            .iter()
            .flat_map(|row| row.runs.iter().map(|run| run.tone))
            .collect();
        assert_eq!(
            tones.first(),
            Some(&Tone::H1),
            "the title is a heading, not a paragraph"
        );
        assert_eq!(tones.iter().filter(|tone| **tone == Tone::H2).count(), 1);
        assert_eq!(tones.iter().filter(|tone| **tone == Tone::H3).count(), 1);
    }

    #[test]
    fn emphasis_code_strike_and_links_are_toned_and_the_markers_go() {
        let tones = tones_of(
            "a **bold** *it* `code` ~~gone~~ [link](https://e.test/x)\n",
            200,
        );
        let joined: String = tones.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(joined, "a bold it code gone link (https://e.test/x)");
        assert!(tones.contains(&("bold".into(), Tone::Strong)));
        assert!(tones.contains(&("it".into(), Tone::Emphasis)));
        assert!(tones.contains(&("code".into(), Tone::Code)));
        assert!(tones.contains(&("gone".into(), Tone::Strike)));
        assert!(
            tones
                .iter()
                .any(|(text, tone)| text == "link" && *tone == Tone::Link)
        );
        // A link whose text *is* its url is not repeated.
        let joined: String = tones_of("[https://e.test](https://e.test)\n", 200)
            .iter()
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(joined, "https://e.test");
    }

    #[test]
    fn nested_lists_indent_and_ordered_numbers_are_kept() {
        let rows = rendered("- one\n- two\n  - nested\n    1. deep\n", 80);
        assert_eq!(rows, vec!["• one", "• two", "  ◦ nested", "    1. deep"]);
        // An ordered list counts from its own start.
        let rows = rendered("3. three\n4. four\n", 80);
        assert_eq!(rows, vec!["3. three", "4. four"]);
    }

    #[test]
    fn a_task_list_keeps_its_boxes_and_tones_the_tick() {
        let rows = render("- [ ] todo\n- [x] done\n", Layout::new(80));
        let texts: Vec<String> = rows.iter().map(Row::text).collect();
        assert_eq!(texts, vec!["• [ ] todo", "• [x] done"]);
        let tones: Vec<Tone> = rows
            .iter()
            .flat_map(|row| row.runs.iter().map(|run| run.tone))
            .collect();
        assert!(tones.contains(&Tone::Done), "{tones:?}");
        assert!(tones.contains(&Tone::Muted), "{tones:?}");
    }

    #[test]
    fn a_code_fence_shows_its_language_and_a_gutter_and_is_never_rewrapped() {
        let long = "x".repeat(120);
        let rows = render(&format!("```rust\n{long}\n```\n"), Layout::new(40));
        assert_eq!(rows[0].text(), "rust", "the language is a dim label row");
        let code = rows[1].text();
        assert!(code.starts_with("│ "), "{code:?}");
        assert_eq!(rows[1].width(), 40, "clipped, not wrapped: {code:?}");
        assert!(code.ends_with('…'), "the clip is visible: {code:?}");
        assert_eq!(rows.len(), 2, "one source line is one row");
        // No language: no label row.
        let rows = rendered("```\na\n```\n", 80);
        assert_eq!(rows, vec!["│ a"]);
    }

    #[test]
    fn an_unclosed_fence_still_renders_as_code_and_unclosed_emphasis_stays_literal() {
        let rows = rendered("```py\nprint(1)\n", 80);
        assert_eq!(rows, vec!["py", "│ print(1)"]);
        let tones = tones_of("an unclosed **bold here\n", 200);
        let joined: String = tones.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(joined, "an unclosed **bold here");
        assert!(tones.iter().all(|(_, tone)| *tone == Tone::Prose));
    }

    #[test]
    fn a_table_is_boxed_when_it_fits_and_pipe_rows_when_it_does_not() {
        let table = "| a | bb |\n|---|---|\n| 1 | 2 |\n| 中文 | x |\n";
        let wide = render(table, Layout::new(80));
        let wide_text: Vec<String> = wide.iter().map(Row::text).collect();
        assert!(wide_text[0].starts_with('┌'), "{wide_text:?}");
        assert!(wide.iter().all(|row| row.width() <= 80), "{wide_text:?}");
        assert!(wide_text.iter().any(|row| row.contains("中文")));
        let narrow = render(table, Layout::new(20));
        let narrow_text: Vec<String> = narrow.iter().map(Row::text).collect();
        assert!(
            !narrow_text[0].starts_with('┌'),
            "degraded at 20: {narrow_text:?}"
        );
        assert!(narrow_text[0].contains(" | "), "{narrow_text:?}");
        assert!(
            narrow.iter().all(|row| row.width() <= 20),
            "{narrow_text:?}"
        );
        // The header row is bold, in either shape.
        assert_eq!(wide[1].runs[1].tone, Tone::Strong, "{wide_text:?}");
        assert_eq!(narrow[0].runs[0].tone, Tone::Strong);
    }

    #[test]
    fn a_quote_keeps_its_bar_on_every_wrapped_row() {
        let rows = rendered(
            "> quoted line that is long enough to need wrapping here\n",
            30,
        );
        assert!(rows.len() > 1, "{rows:?}");
        for row in &rows {
            assert!(row.starts_with("▎ "), "{row:?}");
            assert!(text_width(row) <= 30, "{row:?}");
        }
    }

    #[test]
    fn a_list_inside_a_quote_keeps_the_bar_and_the_hanging_indent() {
        let rows = rendered(
            "> - quoted bullet that is long enough to wrap at thirty\n> - second\n",
            30,
        );
        assert!(rows[0].starts_with("▎ • "), "{rows:?}");
        for row in &rows {
            assert!(row.starts_with("▎ "), "{row:?}");
            assert!(text_width(row) <= 30, "{row:?}");
        }
    }
    #[test]
    fn wrapping_carries_styles_across_the_break_and_loses_no_characters() {
        let text = "alpha **beta gamma** delta `code here` omega\n";
        for width in [12usize, 20, 30, 80] {
            let rows = render(text, Layout::new(width));
            let joined: String = rows.iter().map(Row::text).collect::<Vec<_>>().join(" ");
            let words: Vec<&str> = joined.split_whitespace().collect();
            assert_eq!(
                words,
                vec!["alpha", "beta", "gamma", "delta", "code", "here", "omega"],
                "width {width}: {rows:?}"
            );
            for row in &rows {
                assert!(row.width() <= width, "width {width}: {row:?}");
            }
            // The bold word stays bold on whichever row it lands.
            let strong: String = rows
                .iter()
                .flat_map(|row| row.runs.iter())
                .filter(|run| run.tone == Tone::Strong)
                .map(|run| run.text.as_str())
                .collect::<String>()
                .replace(' ', "");
            assert_eq!(strong, "betagamma", "width {width}");
        }
    }

    #[test]
    fn a_rule_is_drawn_up_to_sixty_columns() {
        assert_eq!(rendered("***\n", 80), vec!["─".repeat(60)]);
        assert_eq!(rendered("***\n", 30), vec!["─".repeat(30)]);
    }

    #[test]
    fn html_and_images_fall_back_to_text() {
        let rows = rendered("<div>raw</div>\n\n![alt text](a.png)\n", 80);
        assert_eq!(rows[0], "<div>raw</div>");
        assert_eq!(rows[2], "[image: alt text]");
    }

    #[test]
    fn a_soft_break_joins_a_paragraph_and_a_hard_break_starts_a_row() {
        assert_eq!(rendered("one\ntwo\n", 80), vec!["one two"]);
        assert_eq!(rendered("one  \ntwo\n", 80), vec!["one", "two"]);
    }

    #[test]
    fn every_character_of_a_long_paragraph_is_rendered_once_at_any_width() {
        let text = "word ".repeat(200);
        for width in [1usize, 7, 10, 40, 300] {
            let rows = render(&text, Layout::new(width));
            // Mid-word breaks are legitimate (a single word wider than the
            // row); what must hold is that no character is lost.
            let joined: String = rows.iter().map(Row::text).collect::<Vec<_>>().join(" ");
            assert_eq!(
                joined.split_whitespace().collect::<String>(),
                "word".repeat(200),
                "width {width}"
            );
            for row in &rows {
                assert!(row.width() <= width.max(1), "width {width}: {row:?}");
            }
        }
    }

    #[test]
    fn a_cjk_paragraph_counts_two_columns_per_character() {
        let text = "中文".repeat(20);
        let rows = render(&text, Layout::new(30));
        for row in &rows {
            assert!(row.width() <= 30, "{row:?}");
        }
        assert!(rows.len() > 1, "{rows:?}");
        let joined: String = rows.iter().map(Row::text).collect();
        assert_eq!(joined, text);
    }

    // -------------------------------------------------------------- streaming

    /// The invariant names: the final render of a message streamed in
    /// chunks equals the one-shot render of the same text.
    #[test]
    fn streaming_a_document_in_blocks_renders_it_exactly_as_one_pass_does() {
        let document = "# Title\n\nA paragraph with **bold** and `code`.\n\n\
                        - one\n- two\n  - nested\n\n\
                        ```sh\ncargo test\ncargo fmt\n```\n\n\
                        | a | bb |\n|---|---|\n| 1 | 2 |\n\n\
                        > quoted\n\nlast line\n";
        let expected: Vec<String> = rendered(document, 40);
        let mut stream = Stream::new();
        let mut blocks = Vec::new();
        for chunk in document.as_bytes().chunks(5) {
            let text = String::from_utf8_lossy(chunk);
            blocks.extend(stream.push(&text));
        }
        blocks.extend(stream.finish());
        // The TUI commits each block through the same join `render` uses.
        let mut rows = Vec::new();
        for block in blocks {
            if !rows.is_empty() {
                rows.push(String::new());
            }
            rows.extend(rendered(&block, 40));
        }
        assert_eq!(rows, expected);
    }

    /// A fence and a table are one block each however the chunking falls: their
    /// interior blank-line-free structure must not be cut.
    #[test]
    fn a_fence_and_a_table_are_never_split_by_a_chunk_boundary() {
        let document = "```sh\ncargo test\n\ncargo fmt\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        for size in 1..document.len() {
            let mut stream = Stream::new();
            let mut blocks = Vec::new();
            for chunk in document.as_bytes().chunks(size) {
                let text = String::from_utf8_lossy(chunk);
                blocks.extend(stream.push(&text));
            }
            blocks.extend(stream.finish());
            assert_eq!(blocks.len(), 2, "chunk size {size}: {blocks:#?}");
            assert!(blocks[0].contains("cargo fmt"));
            assert!(blocks[1].contains("| 1 | 2 |"));
        }
    }

    #[test]
    fn a_shape_is_decided_from_the_blocks_first_complete_line() {
        // Nothing is decided while the first line is still arriving: it could
        // become a fence.
        assert_eq!(Shape::of("``"), None);
        assert_eq!(Shape::of("- item without a newline yet"), None);
        assert_eq!(
            Shape::of("```rust\nlet x = 1;\n"),
            Some(Shape::Fence {
                marker: "```".into(),
                label: "rust".into()
            })
        );
        assert_eq!(
            Shape::of("```\n"),
            Some(Shape::Fence {
                marker: "```".into(),
                label: String::new()
            })
        );
        assert_eq!(Shape::of("- a\n- b\n"), Some(Shape::List));
        assert_eq!(Shape::of("1. a\n2. b\n"), Some(Shape::List));
        // An indented line is an item's continuation, not a new item.
        assert_eq!(Shape::of("  - nested\n"), Some(Shape::Whole));
        assert_eq!(Shape::of("A paragraph.\n"), Some(Shape::Whole));
        assert_eq!(Shape::of("| a | b |\n|---|---|\n"), Some(Shape::Whole));
    }

    #[test]
    fn a_fence_and_a_list_stream_rows_and_prose_does_not() {
        let fence = Shape::of("```\n").unwrap();
        // Every complete line but the last few is final: `keep` lines stay for the live area, so
        // with 5 complete lines and keep 2 the cut is after the third.
        assert_eq!(
            stable_cut("```\na\nb\nc\nd\ne\n", 0, &fence, true, 2),
            Some(10)
        );
        assert_eq!(stable_cut("```\na\nb\n", 0, &fence, true, 3), None);

        let list = Shape::of("- a\n- b\n- c\n- d\n- e\n").unwrap();
        // The cut lands before an item, keeping `keep` complete lines after it.
        assert_eq!(
            stable_cut("- a\n- b\n- c\n- d\n- e\n", 0, &list, false, 1),
            Some(16)
        );
        // A table never cuts: its column widths need every row.
        let table = Shape::of("| a | b |\n").unwrap();
        assert_eq!(
            stable_cut("| a | b |\n|---|---|\n", 0, &table, false, 1),
            None
        );
        // Prose never cuts either: a paragraph re-wraps as it grows.
        let prose = Shape::of("A long paragraph\n").unwrap();
        assert_eq!(
            stable_cut("A long paragraph\nmore\n", 0, &prose, false, 1),
            None
        );
    }

    #[test]
    fn a_streamed_piece_renders_exactly_the_rows_the_block_renders() {
        // A fence: the language label is on the first piece only, and the rows
        // are the whole block's rows, in order.
        let open = "```rust\nlet a = 1;\nlet b = 2;\n";
        let shape = Shape::of(open).unwrap();
        let whole: Vec<String> = render_block(&format!("{open}```\n"), Layout::new(40))
            .iter()
            .map(Row::text)
            .collect();
        let first = piece_rows(open, 0, 19, &shape, Layout::new(40));
        let second = piece_rows(open, 19, open.len(), &shape, Layout::new(40));
        let streamed: Vec<String> = first.iter().chain(second.iter()).map(Row::text).collect();
        assert_eq!(streamed, whole);

        // A list cut in the middle keeps its numbering: the second piece reads
        // `3.` and `4.`, not `1.` and `2.`.
        let list = "1. a\n2. b\n3. c\n4. d\n";
        let shape = Shape::of(list).unwrap();
        let whole: Vec<String> = render_block(list, Layout::new(40))
            .iter()
            .map(Row::text)
            .collect();
        let first: Vec<String> = piece_rows(list, 0, 10, &shape, Layout::new(40))
            .iter()
            .map(Row::text)
            .collect();
        let second: Vec<String> = piece_rows(list, 10, list.len(), &shape, Layout::new(40))
            .iter()
            .map(Row::text)
            .collect();
        assert_eq!(first, vec!["1. a", "2. b"]);
        assert_eq!(second, vec!["3. c", "4. d"]);
        assert_eq!([first, second].concat(), whole);
    }

    #[test]
    fn settling_an_open_fence_hands_out_each_row_once() {
        let mut stream = Stream::new();
        let mut out: Vec<String> = Vec::new();
        for delta in ["```text\n", "line-1\n", "line-2\n", "line-3\n", "line-4\n"] {
            let blocks = stream.push_blocks(delta);
            assert!(blocks.is_empty(), "a fence is one block: {delta:?}");
            out.extend(stream.settle(Layout::new(40), 2).iter().map(Row::text));
        }
        // The label and the first two code rows are out; the rest is live.
        assert_eq!(out, vec!["text", "│ line-1", "│ line-2"]);
        let closed = stream.finish_block().expect("the last block");
        assert_eq!(closed.text, "```text\nline-1\nline-2\nline-3\nline-4\n");
        assert_eq!(closed.delivered, 3, "the label and two code rows went out");
    }

    #[test]
    fn the_tail_of_a_stream_renders_an_open_fence_and_an_open_table() {
        let mut stream = Stream::new();
        assert_eq!(
            stream.push("intro\n\n"),
            vec!["intro\n".to_string()],
            "the finished block is handed out once"
        );
        assert!(stream.push("```sh\ncargo te").is_empty());
        let tail = stream.tail();
        let rows = rendered(&tail, 40);
        assert_eq!(rows, vec!["sh", "│ cargo te"]);
        // An open table stays plain pipe rows in the tail: its widths need
        // every row, which the stream does not have yet.
        let mut stream = Stream::new();
        stream.push("| a | b |\n|---|---|\n| 1 |");
        let tail = stream.tail();
        assert!(tail.contains("| 1 |"), "{tail:?}");
    }

    #[test]
    fn a_stream_that_ends_mid_paragraph_finishes_with_that_block() {
        let mut stream = Stream::new();
        stream.push("first\n\nsecond");
        assert_eq!(stream.finish(), Some("second".to_string()));
        assert_eq!(stream.finish(), None, "finishing twice yields no block");
    }

    #[test]
    fn a_stream_never_splits_a_multibyte_character() {
        // The stream takes `&str` deltas (the provider decodes UTF-8 first), so a chunk boundary is a
        // character boundary; this proves the splitter does not re-encode anything on the way through.
        let mut stream = Stream::new();
        let mut blocks = Vec::new();
        for chunk in "## 中文标题\n\nok\n".chars() {
            blocks.extend(stream.push(&chunk.to_string()));
        }
        blocks.extend(stream.finish());
        assert!(blocks.iter().any(|block| block.contains("中文标题")));
    }

    // ------------------------------------------------------------ model/tone

    #[test]
    fn no_row_is_wider_than_the_width_at_any_narrow_width() {
        let document = "# h\n\n> - a list item with quite a few words in it, enough to wrap\n\n\
                        ```\nlet x = 1;\n```\n\n| alpha | beta |\n|---|---|\n| 1 | 2 |\n";
        for width in [1usize, 5, 10, 20, 30, 40, 60, 80, 300] {
            for row in render(document, Layout::new(width)) {
                assert!(row.width() <= width.max(1), "width {width}: {row:?}");
            }
        }
    }

    #[test]
    fn a_highlighter_seam_exists_and_the_default_changes_nothing() {
        let row = Row::plain("let x = 1;", Tone::Dim);
        assert_eq!(NoHighlight.highlight(&row), row.runs);
    }

    #[test]
    fn the_plain_fallback_keeps_the_text_and_drops_only_the_colour() {
        let text = "# Title\n\n**bold**\n";
        let rows = render(text, Layout::new(80));
        let texts: Vec<String> = rows.iter().map(Row::text).collect();
        assert_eq!(texts, vec!["Title", "", "bold"]);
        let plain = crate::theme::Theme::plain().no_color();
        for row in &rows {
            let line = row.line(plain, &NoHighlight);
            assert_eq!(
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                row.text()
            );
        }
    }

    #[test]
    fn a_row_renders_as_a_styled_line_with_one_span_per_run() {
        let theme = crate::theme::Theme::plain();
        let row = Row {
            runs: vec![
                Run::new("plain ", Tone::Prose),
                Run::new("bold", Tone::Strong),
            ],
        };
        let line = row.line(theme, &NoHighlight);
        assert_eq!(line.spans.len(), 2);
        assert!(
            line.spans[1]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }
    /// asks for a corpus split at *every* boundary.
    #[test]
    fn a_document_split_at_every_character_boundary_renders_the_same_way() {
        let document = "# Head\n\ntext with **bold** and `code`\n\n```sh\ncargo test\n```\n\n\
                        | a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n";
        let expected: Vec<String> = rendered(document, 40);
        let chars: Vec<char> = document.chars().collect();
        for split in 1..chars.len() {
            let (head, tail): (String, String) = (
                chars[..split].iter().collect(),
                chars[split..].iter().collect(),
            );
            let mut stream = Stream::new();
            let mut blocks = stream.push(&head);
            blocks.extend(stream.push(&tail));
            blocks.extend(stream.finish());
            let mut rows = Vec::new();
            for block in blocks {
                if !rows.is_empty() {
                    rows.push(String::new());
                }
                rows.extend(rendered(&block, 40));
            }
            assert_eq!(rows, expected, "split at character {split}");
        }
    }

    #[test]
    fn rendering_a_block_twice_is_stable() {
        for text in [
            "# h\n\npara\n",
            "```\nx\n```\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n",
        ] {
            assert_eq!(render(text, Layout::new(30)), render(text, Layout::new(30)));
        }
    }
}
