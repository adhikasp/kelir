//! App methods: input.
use crate::tui::app::*;
use crate::tui::*;

impl App {
    pub fn press(&mut self, press: Press) {
        match press {
            Press::Edit(key) => {
                let typed = matches!(key, Key::Char(c) if !c.is_control());
                match self.editor.handle(key) {
                    Outcome::Submitted(text) => self.submit(&text),
                    Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                    Outcome::Edited | Outcome::Ignored => {
                        if typed {
                            self.adopt_typed_image();
                        }
                    }
                }
            }
            Press::Backspace => match self.editor.handle(Key::RemoveChip) {
                Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                _ => match self.editor.handle(Key::Backspace) {
                    Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                    Outcome::Submitted(text) => self.submit(&text),
                    Outcome::Edited | Outcome::Ignored => {}
                },
            },
            Press::CtrlC => self.ctrl_c(),
            Press::ToggleTail => self.toggle_tail(),
            Press::Stop => {
                if self.activity.kind.working() {
                    self.push_text("» stop (Esc)");
                    self.dispatch(&Action::Stop(None));
                }
            }
            Press::Redraw => {}
            Press::PageUp => {
                let viewport = self.viewport_height();
                self.scroll_by(-(viewport.max(1) as isize), viewport);
            }
            Press::PageDown => {
                let viewport = self.viewport_height();
                self.scroll_by(viewport.max(1) as isize, viewport);
            }
            Press::ScrollUp => {
                let viewport = self.viewport_height();
                self.scroll_by(-(keys::WHEEL_ROWS as isize), viewport);
            }
            Press::ScrollDown => {
                let viewport = self.viewport_height();
                self.scroll_by(keys::WHEEL_ROWS as isize, viewport);
            }
            Press::Top => self.scroll_to(0, self.viewport_height()),
            Press::Bottom => {
                self.scroll = None;
                self.pending_new = 0;
            }
            Press::CopyLast => {
                let answer = self.last_answer();
                if !answer.is_empty() {
                    let encoded = osc52(&answer);
                    self.status_records.push(encoded);
                    self.note = "last answer copied".to_string();
                } else {
                    self.note = "no answer yet".to_string();
                }
            }
            Press::DumpTranscript => {
                if self.dump_transcript().is_none() {
                    self.note = "no session dir to save the transcript".to_string();
                }
            }
        }
    }

    /// Take the image path that was just typed at the cursor and turn it into
    /// a chip: the file is saved into the session, the path text removed.
    pub(super) fn adopt_typed_image(&mut self) {
        if self.session_dir.is_none() {
            return;
        }
        let Some(line) = self.editor.lines().get(self.editor.cursor().0) else {
            return;
        };
        let col = self.editor.cursor().1;
        let typed: String = line.chars().take(col).collect();
        let Some(candidate) = attachments::path_tail(typed.trim_end()) else {
            return;
        };
        let Some(paste) = attachments::pasted_image(&candidate) else {
            return;
        };
        if self.absorbed.contains(&candidate) {
            return;
        }
        let Some(saved) = self.save_image(&paste.path) else {
            return;
        };
        if self.editor.take_word_before_cursor().is_none() {
            return;
        }
        self.absorbed.insert(candidate);
        let chip = Chip::new(saved.seq, &saved.rel, &saved.pointer);
        self.press(Press::Edit(Key::Chip(chip)));
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(paste) = attachments::pasted_image(text)
            && let Some(saved) = self.save_image(&paste.path)
        {
            self.press(Press::Edit(Key::Chip(Chip::new(
                saved.seq,
                &saved.rel,
                &saved.pointer,
            ))));
            return;
        }
        self.press(Press::Edit(Key::Paste(text.to_string())));
    }

    pub(super) fn save_image(&mut self, path: &std::path::Path) -> Option<attachments::Saved> {
        let dir = self.session_dir.clone()?;
        if self.editor.chips().len() >= attachments::MAX_PER_PROMPT {
            self.note = format!("at most {} images per prompt", attachments::MAX_PER_PROMPT);
            return None;
        }
        let seq = (self.editor.chips().len() as u32 + 1).max(1);
        match attachments::save(&dir, seq, path, &mut self.seen_images) {
            Ok(saved) => Some(saved),
            Err(message) => {
                self.note = crate::util::one_line_visible(&message, 60);
                None
            }
        }
    }

    pub(super) fn discard_chip(&mut self, chip: &Chip) {
        if self
            .editor
            .chips()
            .iter()
            .any(|other| other.rel == chip.rel)
        {
            return;
        }
        if let Some(dir) = &self.session_dir {
            let path = dir.join(&chip.rel);
            let _ = std::fs::remove_file(path);
            self.seen_images.retain(|_, rel| rel != &chip.rel);
            let stem = chip
                .rel
                .rsplit('/')
                .next()
                .and_then(|name| name.split_once('-'))
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_default();
            self.absorbed.retain(|text| {
                !text.contains(&chip.rel) && (stem.is_empty() || !text.contains(&stem))
            });
        }
    }

    pub fn submit(&mut self, line: &str) {
        let chips = self.editor.take_chips();
        let mut text = line.to_string();
        for chip in &chips {
            text = text.replace(&chip.label, &chip.pointer);
        }
        text = self.absorb_image_paths(&text);
        match commands::parse(&text) {
            Ok(Parsed::Action(action)) => {
                self.separate();
                match &action {
                    // The echo remembers the raw text: `EventBody::User` carries
                    // that, and a rendered row would never match it.
                    Action::Submit(text) => self.push_user(text),
                    Action::SayNow(text) => self.push_user_now(text),
                    _ => self.push_text(&action.line()),
                }
                self.dispatch(&action);
            }
            Ok(Parsed::Note(note)) => self.push_text(&note),
            Ok(Parsed::View(view)) => match view {
                commands::View::Expand(ordinal) => self.expand(ordinal),
            },
            Err(message) => {
                if !message.is_empty() {
                    self.push_text(&format!("kelir: {message}"));
                }
            }
        }
    }

    pub fn absorb_image_paths(&mut self, text: &str) -> String {
        if self.session_dir.is_none() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text.to_string();
        while let Some((start, matched)) = next_path(&rest) {
            out.push_str(&rest[..start]);
            let reusable = self.absorbed.contains(&matched);
            match attachments::pasted_image(&matched) {
                Some(paste) if !reusable => match self.save_image(&paste.path) {
                    Some(saved) => {
                        self.absorbed.insert(matched.clone());
                        out.push_str(&saved.pointer);
                    }
                    None => out.push_str(&matched),
                },
                _ => out.push_str(&matched),
            }
            rest = rest[start + matched.len()..].to_string();
        }
        out.push_str(&rest);
        out
    }

    pub fn dispatch(&mut self, action: &Action) {
        let request =
            Request::new(action.verb(), action.args(), "tui").with_token(Some(self.token.clone()));
        let (reply, answer) = oneshot::channel();
        if self
            .verbs
            .send(Incoming {
                request,
                reply,
                action: None,
            })
            .is_err()
        {
            self.quit = true;
            return;
        }
        let to_tui = self.to_tui.clone();
        tokio::spawn(async move {
            if let Ok(response) = answer.await {
                let _ = to_tui.send(View::Live(Live::Reply(response)));
            }
        });
    }

    pub(super) fn ctrl_c(&mut self) {
        let now = Instant::now();
        let twice = self
            .last_ctrl_c
            .map(|last| now.duration_since(last) <= view::DOUBLE_CTRL_C)
            .unwrap_or(false);
        if twice {
            self.last_ctrl_c = None;
            self.push_text("» quit (second Ctrl-C)");
            self.dispatch(&Action::Quit);
        } else {
            self.last_ctrl_c = Some(now);
            self.push_text("» stop (Ctrl-C again within a second to quit)");
            self.dispatch(&Action::Stop(None));
        }
    }

    pub fn toggle_tail(&mut self) {
        if self.show_tail {
            self.show_tail = false;
            return;
        }
        let text = match &self.last_out {
            Some(path) => match view::read_tail(std::path::Path::new(path), view::TAIL_BYTES) {
                Ok(text) => text,
                Err(error) => format!("cannot read {path}: {error}"),
            },
            None => "no tool output yet".to_string(),
        };
        self.tail = crate::tui::scrollback::wrap(&text, self.width).into();
        self.show_tail = true;
    }

    pub fn last_call(&self) -> Option<&CallLine> {
        self.calls.values().max_by_key(|call| call.ordinal)
    }

    pub(super) fn read_output(&self, call: &CallLine) -> String {
        if call.out_path.is_empty() {
            return String::new();
        }
        view::read_tail(std::path::Path::new(&call.out_path), view::TAIL_BYTES).unwrap_or_default()
    }

    pub fn expand(&mut self, which: Option<u64>) {
        let ordinal = match which {
            Some(ordinal) => ordinal,
            None => match self.calls.keys().max() {
                Some(ordinal) => *ordinal,
                None => {
                    self.push_text("kelir: no tool call to expand yet");
                    return;
                }
            },
        };
        let Some(call) = self.calls.get(&ordinal).cloned() else {
            self.push_text(&format!("kelir: no call #{ordinal}"));
            return;
        };
        let text = self.read_output(&call);
        let lines = call.expanded(self.theme, self.width, &text, tools_view::EXPAND_ROWS);
        self.push_lines(lines);
        self.push_text("  (a committed line cannot unfold: /expand appends the output instead)");
    }

    /// Advance the spinner while a working state lasts.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.activity.kind.working() {
            return false;
        }
        self.frame = self.frame.wrapping_add(1);
        if self.activity.kind == Kind::RateLimited
            && let Some(deadline) = self.retry_deadline
        {
            self.activity
                .tick_retry(deadline.saturating_duration_since(now));
        }
        true
    }
}
