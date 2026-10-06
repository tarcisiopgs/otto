//! Fitting text to the columns of a terminal. A column is not a character: a
//! CJK character takes two, and counting characters would push a line past
//! its edge.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The columns `text` takes on a terminal.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn columns_of(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// `text` in at most `columns` columns, cut with an ellipsis when longer.
pub fn cut(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.to_owned();
    }
    let Some(room) = columns.checked_sub(1) else {
        return String::new();
    };
    let mut kept = String::new();
    let mut taken = 0;
    for c in text.chars() {
        if taken + columns_of(c) > room {
            break;
        }
        taken += columns_of(c);
        kept.push(c);
    }
    kept.push('…');
    kept
}

/// `text` in exactly `columns` columns: padded, or cut with an ellipsis.
pub fn fit(text: &str, columns: usize) -> String {
    let mut fitted = cut(text, columns);
    let missing = columns.saturating_sub(width(&fitted));
    fitted.extend(std::iter::repeat_n(' ', missing));
    fitted
}

/// The end of `text` when it is wider than `columns`: a path says the most in
/// its last parts.
pub fn tail(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.to_owned();
    }
    let room = columns.saturating_sub(1);
    let mut kept = Vec::new();
    let mut taken = 0;
    for c in text.chars().rev() {
        if taken + columns_of(c) > room {
            break;
        }
        taken += columns_of(c);
        kept.push(c);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// `text` over lines of at most `columns`, broken between words. A word wider
/// than a line is the only thing cut in two.
pub fn wrapped(text: &str, columns: usize) -> Vec<String> {
    let columns = columns.max(1);
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        for piece in rows(word, columns) {
            match lines.last_mut() {
                // Only a whole word joins the line before it.
                Some(line)
                    if piece.len() == word.len() && width(line) + 1 + width(piece) <= columns =>
                {
                    line.push(' ');
                    line.push_str(piece);
                }
                _ => lines.push(piece.to_owned()),
            }
        }
    }
    lines
}

/// One line as the rows it takes at `columns` wide. A character is never
/// split, and one wider than the row has a row to itself.
pub fn rows(line: &str, columns: usize) -> Vec<&str> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    let (mut start, mut taken) = (0, 0);
    for (index, c) in line.char_indices() {
        if taken > 0 && taken + columns_of(c) > columns {
            rows.push(&line[start..index]);
            (start, taken) = (index, 0);
        }
        taken += columns_of(c);
    }
    rows.push(&line[start..]);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_counts_columns_not_characters() {
        assert_eq!(width("report"), 6);
        assert_eq!(width("ação"), 4);
        assert_eq!(width("日本語"), 6);
    }

    #[test]
    fn cut_keeps_the_start_and_marks_what_it_dropped() {
        assert_eq!(cut("abc", 4), "abc");
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("日本語日本語", 5), "日本…");
        assert_eq!(cut("abcdef", 0), "");
    }

    #[test]
    fn fit_fills_exactly_the_columns_it_is_given() {
        assert_eq!(fit("ab", 4), "ab  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("日本", 6), "日本  ");
        assert_eq!(width(&fit("日本語日本語", 5)), 5);
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("/a/b/file.md", 20), "/a/b/file.md");
        assert_eq!(tail("/a/b/c/file.md", 8), "…file.md");
        assert_eq!(tail("日本語", 5), "…本語");
    }

    #[test]
    fn wrapped_breaks_between_words() {
        assert_eq!(wrapped("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrapped("日本語 日本語", 6), ["日本語", "日本語"]);
        assert_eq!(wrapped("  spaced   out  ", 20), ["spaced out"]);
        assert!(wrapped("", 5).is_empty());
    }

    #[test]
    fn wrapped_cuts_only_a_word_longer_than_a_line() {
        assert_eq!(wrapped("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrapped("ab cdefghi j", 4), ["ab", "cdef", "ghi", "j"]);
    }

    #[test]
    fn rows_break_a_line_at_the_width() {
        assert_eq!(rows("0123456789abcdef", 10), ["0123456789", "abcdef"]);
        assert_eq!(rows("ação", 10), ["ação"]);
        assert_eq!(rows("", 10), [""]);
        // A wide character does not straddle the edge.
        assert_eq!(rows("日本語", 4), ["日本", "語"]);
        assert_eq!(rows("a日本", 4), ["a日", "本"]);
        // One too wide for the row still gets a row.
        assert_eq!(rows("日本", 1), ["日", "本"]);
    }
}
