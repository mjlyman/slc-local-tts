//! SLC text cleanup, the speakable check and flight-level normalization.

/// Longest raw request text (in chars) the server accepts.
pub const MAX_TEXT_CHARS: usize = 5000;

/// Turns SLC's raw text into a single tidy line: control characters
/// (including NUL and the CRLF SLC appends) become spaces, then the result
/// is trimmed and every whitespace run collapses to one space.
pub fn clean(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    // split_whitespace skips empty pieces, so this trims and collapses at once.
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when the text has at least one letter or digit. Punctuation-only
/// lines ("...", "—") cannot be spoken and get silence instead.
pub fn is_speakable(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

const DIGIT_WORDS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

/// Spells out flight levels: "FL350" becomes "flight level three five zero".
///
/// A match is a word boundary, `FL` (any case), an optional single space,
/// then 2 or 3 ASCII digits and another word boundary. A boundary means the
/// neighbouring char is not alphanumeric, or there is none.
pub fn normalize_aviation(text: &str) -> String {
    // Work on a Vec<char> so every index is a char index. Indexing a &str by
    // position would be a byte index and could land inside a multi-byte char
    // (curly quotes, accents), which panics.
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some((words, next)) = match_flight_level(&chars, i) {
            out.push_str(&words);
            i = next;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Tries to match a flight level starting at `start`. On success returns the
/// spoken words and the index just past the match.
fn match_flight_level(chars: &[char], start: usize) -> Option<(String, usize)> {
    let at_boundary = start == 0 || !chars[start - 1].is_alphanumeric();
    if !at_boundary {
        return None;
    }
    let is = |idx: usize, want: char| {
        chars
            .get(idx)
            .is_some_and(|c| c.to_ascii_lowercase() == want)
    };
    if !(is(start, 'f') && is(start + 1, 'l')) {
        return None;
    }
    let mut pos = start + 2;
    if chars.get(pos) == Some(&' ') {
        pos += 1;
    }
    let digits_start = pos;
    while pos < chars.len() && chars[pos].is_ascii_digit() && pos - digits_start < 3 {
        pos += 1;
    }
    let digit_count = pos - digits_start;
    if !(2..=3).contains(&digit_count) {
        return None;
    }
    // The char after the digits must not continue the word ("FL3500", "FL35x").
    if chars.get(pos).is_some_and(|c| c.is_alphanumeric()) {
        return None;
    }
    let spelled: Vec<&str> = chars[digits_start..pos]
        .iter()
        .map(|c| DIGIT_WORDS[c.to_digit(10).unwrap() as usize])
        .collect();
    Some((format!("flight level {}", spelled.join(" ")), pos))
}

/// Full pipeline: clean, return `None` if there is nothing to say, otherwise
/// normalize aviation phrases.
pub fn prepare(text: &str) -> Option<String> {
    let cleaned = clean(text);
    if !is_speakable(&cleaned) {
        return None;
    }
    Some(normalize_aviation(&cleaned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_handles_slc_quirks() {
        assert_eq!(clean("Welcome aboard.\r\n"), "Welcome aboard.");
        assert_eq!(clean("a\t\tb  c"), "a b c");
        assert_eq!(clean("cap\u{0}tain"), "cap tain");
        assert_eq!(clean("captain’s — 31°C & more"), "captain’s — 31°C & more");
    }

    #[test]
    fn unspeakable_inputs() {
        for s in ["", " ", "\r\n", "...", "—"] {
            assert_eq!(prepare(s), None, "input {s:?}");
        }
        assert!(!is_speakable("..."));
        assert!(is_speakable("a"));
    }

    #[test]
    fn speakable_digits() {
        assert_eq!(prepare("384"), Some("384".to_string()));
    }

    #[test]
    fn flight_levels() {
        let cases = [
            (
                "Cruising at FL350 today",
                "Cruising at flight level three five zero today",
            ),
            ("FL 90", "flight level nine zero"),
            ("fl350", "flight level three five zero"),
            ("FL3500", "FL3500"),
            ("FLAT", "FLAT"),
            ("FL350.", "flight level three five zero."),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_aviation(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn flight_level_edge_cases() {
        assert_eq!(normalize_aviation("XFL350"), "XFL350");
        assert_eq!(normalize_aviation("FL9"), "FL9");
        assert_eq!(
            normalize_aviation("Climb FL350 then FL 90, descend FL240"),
            "Climb flight level three five zero then flight level nine zero, descend flight level two four zero"
        );
        assert_eq!(
            normalize_aviation("“FL350” — café FL90"),
            "“flight level three five zero” — café flight level nine zero"
        );
        assert_eq!(normalize_aviation("FL"), "FL");
        assert_eq!(normalize_aviation("FL 3"), "FL 3");
    }

    #[test]
    fn prepare_cleans_then_normalizes() {
        assert_eq!(
            prepare(" FL350\r\n"),
            Some("flight level three five zero".to_string())
        );
    }
}
