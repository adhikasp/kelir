//! The agent's markdown, rendered into styled rows.
//! Three pieces, each testable alone:

pub mod render;
pub mod stream;
pub mod table;
pub mod wrap;

pub use render::*;
pub use stream::*;
pub(crate) use table::*;
pub use wrap::*;

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
    fn markdown_blocks_render_with_their_markers_and_tones() {
        // Heading levels, list markers, task boxes, rules, HTML and images.
        let cases: [(&str, &[&str]); 7] = [
            ("# One\n\n## Two\n", &["One", "", "Two"]),
            (
                "- one\n- two\n  - nested\n    1. deep\n",
                &["• one", "• two", "  ◦ nested", "    1. deep"],
            ),
            ("3. three\n4. four\n", &["3. three", "4. four"]),
            ("- [ ] todo\n- [x] done\n", &["• [ ] todo", "• [x] done"]),
            ("```\na\n```\n", &["│ a"]),
            ("one\ntwo\n", &["one two"]),
            (
                "<div>raw</div>\n\n![alt text](a.png)\n",
                &["<div>raw</div>", "", "[image: alt text]"],
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(rendered(input, 80), expected, "{input:?}");
        }
        let rows = render("# One\n\n## Two\n", Layout::new(80));
        let tones: Vec<Tone> = rows
            .iter()
            .flat_map(|row| row.runs.iter().map(|run| run.tone))
            .collect();
        assert_eq!(tones.first(), Some(&Tone::H1));
        assert!(tones.contains(&Tone::H2));
        let rows = render("- [x] done\n", Layout::new(80));
        assert!(rows[0].runs.iter().any(|run| run.tone == Tone::Done));
        assert_eq!(rendered("***\n", 80), vec!["─".repeat(60)]);
        assert_eq!(rendered("***\n", 30), vec!["─".repeat(30)]);
        assert_eq!(rendered("one  \ntwo\n", 80), vec!["one", "two"]);
    }

    #[test]
    fn inline_markers_lose_their_delimiters_and_keep_their_tone() {
        let tones = tones_of(
            "a **bold** *it* `code` ~~gone~~ [link](https://e.test/x)\n",
            200,
        );
        let joined: String = tones.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(joined, "a bold it code gone link (https://e.test/x)");
        for (text, tone) in [
            ("bold", Tone::Strong),
            ("it", Tone::Emphasis),
            ("code", Tone::Code),
            ("gone", Tone::Strike),
        ] {
            assert!(tones.contains(&(text.into(), tone)), "{text}");
        }
        assert!(
            tones
                .iter()
                .any(|(text, tone)| text == "link" && *tone == Tone::Link)
        );
        let joined: String = tones_of("[https://e.test](https://e.test)\n", 200)
            .iter()
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(joined, "https://e.test");
        let tones = tones_of("an unclosed **bold here\n", 200);
        let joined: String = tones.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(joined, "an unclosed **bold here");
        assert!(tones.iter().all(|(_, tone)| *tone == Tone::Prose));
    }

    #[test]
    fn fences_tables_and_quotes_respect_the_width() {
        let long = "x".repeat(120);
        let rows = render(&format!("```rust\n{long}\n```\n"), Layout::new(40));
        assert_eq!(rows[0].text(), "rust");
        assert!(rows[1].text().starts_with("│ "));
        assert_eq!(rows[1].width(), 40);
        assert!(rows[1].text().ends_with('…'));
        assert_eq!(rows.len(), 2);
        assert_eq!(rendered("```py\nprint(1)\n", 80), vec!["py", "│ print(1)"]);

        let table = "| a | bb |\n|---|---|\n| 1 | 2 |\n| 中文 | x |\n";
        let wide_rows = render(table, Layout::new(80));
        let wide: Vec<String> = wide_rows.iter().map(Row::text).collect();
        assert!(wide[0].starts_with('┌'), "{wide:?}");
        let narrow_rows = render(table, Layout::new(20));
        let narrow: Vec<String> = narrow_rows.iter().map(Row::text).collect();
        assert!(!narrow[0].starts_with('┌'), "{narrow:?}");
        assert!(narrow[0].contains(" | "), "{narrow:?}");
        assert_eq!(wide_rows[1].runs[1].tone, Tone::Strong);
        assert_eq!(narrow_rows[0].runs[0].tone, Tone::Strong);

        for input in [
            "> quoted line that is long enough to need wrapping here\n",
            "> - quoted bullet that is long enough to wrap at thirty\n> - second\n",
        ] {
            let rows = rendered(input, 30);
            assert!(rows.len() > 1, "{rows:?}");
            assert!(rows.iter().all(|row| row.starts_with("▎ ")), "{rows:?}");
        }
        assert!(rendered("> - quoted bullet\n", 30)[0].starts_with("▎ • "));
    }

    #[test]
    fn wrapping_loses_no_characters_at_any_width() {
        let text = "alpha **beta gamma** delta `code here` omega\n";
        for width in [12usize, 20, 30, 80] {
            let rows = render(text, Layout::new(width));
            let joined: String = rows.iter().map(Row::text).collect::<Vec<_>>().join(" ");
            let words: Vec<&str> = joined.split_whitespace().collect();
            assert_eq!(
                words,
                ["alpha", "beta", "gamma", "delta", "code", "here", "omega"],
                "width {width}"
            );
            let strong: String = rows
                .iter()
                .flat_map(|row| row.runs.iter())
                .filter(|run| run.tone == Tone::Strong)
                .map(|run| run.text.as_str())
                .collect::<String>()
                .replace(' ', "");
            assert_eq!(strong, "betagamma", "width {width}");
        }
        let document = "# h\n\n> - a list item with quite a few words in it, enough to wrap\n\n\
                        ```\nlet x = 1;\n```\n\n| alpha | beta |\n|---|---|\n| 1 | 2 |\n";
        for width in [1usize, 5, 10, 20, 30, 40, 60, 80, 300] {
            for row in render(document, Layout::new(width)) {
                assert!(row.width() <= width.max(1), "width {width}: {row:?}");
            }
        }
        let text = "word ".repeat(200);
        for width in [1usize, 7, 10, 40, 300] {
            let rows = render(&text, Layout::new(width));
            let joined: String = rows.iter().map(Row::text).collect::<Vec<_>>().join(" ");
            assert_eq!(
                joined.split_whitespace().collect::<String>(),
                "word".repeat(200),
                "width {width}"
            );
        }
        let text = "中文".repeat(20);
        let rows = render(&text, Layout::new(30));
        assert!(rows.len() > 1, "{rows:?}");
        assert!(rows.iter().all(|row| row.width() <= 30));
        assert_eq!(rows.iter().map(Row::text).collect::<String>(), text);
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
}
