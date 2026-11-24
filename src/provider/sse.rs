//! Incremental Server-Sent Events parser.
//! Bytes arrive in arbitrary pieces, so the parser buffers until a full event block is present.
//! Both `\n\n` and `\r\n\r\n` framing are accepted, `data:` lines are joined with `\n`, and
//! comment/keepalive blocks are skipped.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }

    /// The next complete event, if one has arrived.
    pub fn next_event(&mut self) -> Option<SseEvent> {
        loop {
            let (index, sep_len) = self.boundary()?;
            let block: Vec<u8> = self.buf.drain(..index).collect();
            self.buf.drain(..sep_len);
            if let Some(event) = parse_block(&block) {
                return Some(event);
            }
            // Comment-only or empty block: try the next one.
        }
    }

    /// Drain the rest, for diagnostics when a stream ends mid-block.
    pub fn take_remainder(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    fn boundary(&self) -> Option<(usize, usize)> {
        let crlf = find(&self.buf, b"\r\n\r\n").map(|i| (i, 4));
        let lf = find(&self.buf, b"\n\n").map(|i| (i, 2));
        match (crlf, lf) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_block(block: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(block);
    let mut event = None;
    let mut data: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, rest) = match line.split_once(':') {
            Some((field, rest)) => (field, rest.strip_prefix(' ').unwrap_or(rest)),
            None => (line, ""),
        };
        match field {
            "data" => data.push(rest.to_string()),
            "event" => event = Some(rest.to_string()),
            _ => {}
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(parser: &mut SseParser) -> Vec<SseEvent> {
        let mut out = Vec::new();
        while let Some(event) = parser.next_event() {
            out.push(event);
        }
        out
    }

    #[test]
    fn several_events_in_one_chunk() {
        let mut p = SseParser::new();
        p.push(b"data: a\n\ndata: b\n\ndata: [DONE]\n\n");
        p.push(b"event: delta\r\ndata: x\r\n\r\n");
        let events = collect(&mut p);
        assert_eq!(events.len(), 4);
        assert_eq!(events[1].data, "b");
        assert_eq!(events[2].data, "[DONE]");
        assert_eq!(events[3].event.as_deref(), Some("delta"));
        assert_eq!(events[3].data, "x");
    }

    #[test]
    fn value_without_space_is_kept_verbatim() {
        let mut p = SseParser::new();
        p.push(b"data:{\"a\": 1}\n\n");
        assert_eq!(p.next_event().unwrap().data, "{\"a\": 1}");
    }

    #[test]
    fn leftover_without_blank_line_yields_nothing() {
        let mut p = SseParser::new();
        p.push(b"data: torn");
        assert_eq!(p.next_event(), None);
        assert_eq!(p.take_remainder(), b"data: torn".to_vec());
    }

    #[test]
    fn unknown_fields_and_ids_are_ignored() {
        let mut p = SseParser::new();
        p.push(b"id: 7\nretry: 100\ndata: z\n\n");
        assert_eq!(p.next_event().unwrap().data, "z");
    }

    #[test]
    fn comments_and_keepalives_are_skipped() {
        let mut p = SseParser::new();
        p.push(b": keepalive\n\n");
        assert_eq!(p.next_event(), None);
        p.push(b"data: real\n\n");
        assert_eq!(p.next_event().unwrap().data, "real");
    }
}
