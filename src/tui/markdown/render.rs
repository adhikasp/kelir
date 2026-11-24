//! The agent's markdown, rendered into styled rows.
//! Three pieces, each testable alone:

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

use super::*;

use crate::tui::theme::Theme;

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
    pub glyphs: crate::tui::theme::Glyphs,
}

impl Layout {
    pub fn new(width: usize) -> Self {
        Self::with_glyphs(width, crate::tui::theme::Glyphs::unicode())
    }

    pub fn with_glyphs(width: usize, glyphs: crate::tui::theme::Glyphs) -> Self {
        Self {
            width: width.max(1),
            glyphs,
        }
    }
}

pub(crate) fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

pub(crate) fn text_width(text: &str) -> usize {
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

pub(crate) struct Renderer {
    layout: Layout,
    theme_glyphs: crate::tui::theme::Glyphs,
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
pub(crate) struct TableState {
    pub(crate) head: Vec<Vec<Run>>,
    pub(crate) body: Vec<Vec<Vec<Run>>>,
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
