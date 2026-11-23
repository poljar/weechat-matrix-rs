//! Emoji `:shortcode:` expansion for typed messages.

/// Replace every known `:shortcode:` in `text` with its emoji. Unknown
/// shortcodes and text inside backtick code spans are left alone.
pub fn expand_shortcodes(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    let mut in_code = false;

    while let Some(c) = rest.chars().next() {
        if c == '`' {
            in_code = !in_code;
        } else if c == ':' && !in_code {
            if let Some((emoji, len)) = shortcode_at(rest) {
                output.push_str(emoji);
                rest = &rest[len..];
                continue;
            }
        }
        output.push(c);
        rest = &rest[c.len_utf8()..];
    }

    output
}

/// The emoji for the `:shortcode:` at the start of `text` and the length of
/// the shortcode, including both colons.
fn shortcode_at(text: &str) -> Option<(&'static str, usize)> {
    let name = text.strip_prefix(':')?;
    let end = name.find(|c: char| {
        !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
    })?;
    if end == 0 || !name[end..].starts_with(':') {
        return None;
    }
    let emoji = emojis::get_by_shortcode(&name[..end])?;
    Some((emoji.as_str(), end + 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_shortcodes() {
        assert_eq!(expand_shortcodes("hope so :crossed_fingers:"), "hope so 🤞");
        assert_eq!(expand_shortcodes(":+1: and :-1:"), "👍 and 👎");
        assert_eq!(expand_shortcodes(":smile::wave:"), "😄👋");
    }

    #[test]
    fn leaves_unknown_and_plain_colons() {
        assert_eq!(expand_shortcodes(":notanemoji:"), ":notanemoji:");
        assert_eq!(expand_shortcodes("at 12:30:45 today"), "at 12:30:45 today");
        assert_eq!(expand_shortcodes("https://example.org/"), "https://example.org/");
        assert_eq!(expand_shortcodes("a: b: c"), "a: b: c");
        assert_eq!(expand_shortcodes("ends with :"), "ends with :");
    }

    #[test]
    fn skips_code_spans() {
        assert_eq!(
            expand_shortcodes("type `:smile:` to get :smile:"),
            "type `:smile:` to get 😄"
        );
    }
}
