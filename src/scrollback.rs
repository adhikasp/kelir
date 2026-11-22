//! Turning text into terminal rows.
//! [`wrap`] breaks a string into rows of a given width: wide characters count two columns and a
//! break lands on a space when there is one.

use unicode_width::UnicodeWidthChar;

fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// One character's display width (two columns for CJK, zero for combining
/// marks and control characters).
pub fn char_width(c: char) -> usize {
    width_of(c)
}

/// `Line`'s display width, without allocating.
pub fn display_width(line: &str) -> usize {
    line.chars().map(width_of).sum()
}

/// Break `text` into rows no wider than `width` columns.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        wrap_line(line, width, &mut out);
    }
    out
}

fn wrap_line(line: &str, width: usize, out: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        out.push(String::new());
        return;
    }
    let mut start = 0usize;
    let mut last_space: Option<usize> = None;
    let mut column = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        let char_width = width_of(chars[index]);
        // A space is only a break opportunity, never the reason to break: a row may hold exactly
        // `width` columns and the space after it belongs to the break (it is dropped, not printed).
        if column + char_width > width && index > start && chars[index] != ' ' {
            let end = match last_space {
                Some(space) if space > start => space,
                _ => index,
            };
            let row: String = chars[start..end].iter().collect();
            out.push(row.trim_end().to_string());
            start = end;
            if last_space == Some(end) {
                start += 1;
            }
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
            last_space = None;
            column = 0;
            index = start;
            continue;
        }
        if chars[index] == ' ' {
            last_space = Some(index);
        }
        column += char_width;
        index += 1;
    }
    let row: String = chars[start..].iter().collect();
    out.push(row.trim_end().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_are_not_touched() {
        assert_eq!(wrap("hello", 80), vec!["hello"]);
        assert_eq!(wrap("", 80), vec![""]);
    }

    #[test]
    fn newlines_are_row_breaks() {
        assert_eq!(wrap("a\nb\n", 80), vec!["a", "b", ""]);
        assert_eq!(wrap("\n\n", 80), vec!["", "", ""]);
    }

    #[test]
    fn a_long_line_breaks_at_spaces() {
        assert_eq!(wrap("one two three", 8), vec!["one two", "three"]);
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(
            wrap("aaa bbb ccc ddd", 7),
            vec!["aaa bbb", "ccc ddd"],
            "the space at the break is dropped, not repeated"
        );
    }
    #[test]
    fn a_word_longer_than_the_row_is_broken() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("ab cdefg", 3), vec!["ab", "cde", "fg"]);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        assert_eq!(display_width("日本"), 4);
        assert_eq!(display_width("aé"), 2);
        assert_eq!(wrap("日本語", 4), vec!["日本", "語"]);
        // A row of width 1 still gets the wide character.
        assert_eq!(wrap("日", 1), vec!["日"]);
    }

    #[test]
    fn trailing_spaces_are_dropped_and_leading_ones_kept() {
        assert_eq!(wrap("ab   ", 10), vec!["ab"]);
        assert_eq!(wrap("  ab", 10), vec!["  ab"]);
    }
}
