//! Pick a recent event in a room buffer the way `/reply` and `/redact` do:
//! by event ID, by 1-based index from the newest event, or by `/pattern/`.

use matrix_sdk::ruma::{EventId, OwnedEventId};
use weechat::{buffer::Buffer, Weechat};

const EVENT_ID_TAG_PREFIX: &str = "matrix_id_";

/// Resolve `argument` to an event in `buffer`.
pub(crate) fn select_event(
    buffer: &Buffer,
    argument: &str,
) -> Result<OwnedEventId, String> {
    if argument.starts_with('$') {
        return EventId::parse(argument)
            .map_err(|_| format!("Invalid Matrix event ID: {argument}"));
    }

    if let Some(index) = parse_index(argument) {
        return event_id_at_index(buffer, index).ok_or_else(|| {
            format!("No Matrix event found at index {argument}.")
        });
    }

    match parse_pattern(argument)? {
        Some(pattern) => {
            event_id_matching_pattern(buffer, pattern).ok_or_else(|| {
                format!("No unredacted Matrix event matches /{pattern}/.")
            })
        }
        None => Err(format!(
            "Expected an event ID, an index or /pattern/, got {argument}."
        )),
    }
}

/// `1`, `0` and `-1` select the latest event; `2` or `-2` the one before.
fn parse_index(argument: &str) -> Option<usize> {
    let index = argument.parse::<isize>().ok()?;

    match index {
        0 | -1 => Some(1),
        n if n > 0 => Some(n as usize),
        n => n.checked_abs().map(|n| n as usize),
    }
}

fn parse_pattern(argument: &str) -> Result<Option<&str>, String> {
    let Some(pattern) = argument
        .strip_prefix('/')
        .and_then(|argument| argument.strip_suffix('/'))
    else {
        return Ok(None);
    };

    if pattern.is_empty() {
        Err("The pattern cannot be empty.".to_owned())
    } else {
        Ok(Some(pattern))
    }
}

fn line_event_id(line: &weechat::buffer::BufferLine) -> Option<OwnedEventId> {
    let tags = line.tags();

    if tags.iter().any(|tag| tag.as_ref() == "matrix_redacted") {
        return None;
    }

    tags.iter()
        .find_map(|tag| tag.as_ref().strip_prefix(EVENT_ID_TAG_PREFIX))
        .and_then(|event_id| EventId::parse(event_id).ok())
}

fn event_id_at_index(buffer: &Buffer, index: usize) -> Option<OwnedEventId> {
    buffer
        .lines()
        .rev()
        .filter_map(|line| line_event_id(&line))
        .nth(index.saturating_sub(1))
}

fn event_id_matching_pattern(
    buffer: &Buffer,
    pattern: &str,
) -> Option<OwnedEventId> {
    buffer.lines().rev().find_map(|line| {
        if !Weechat::remove_color(&line.message()).contains(pattern) {
            return None;
        }
        line_event_id(&line)
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_index, parse_pattern};

    #[test]
    fn index_accepts_recent_event_forms() {
        assert_eq!(parse_index("1"), Some(1));
        assert_eq!(parse_index("0"), Some(1));
        assert_eq!(parse_index("-1"), Some(1));
        assert_eq!(parse_index("4"), Some(4));
        assert_eq!(parse_index("-4"), Some(4));
        assert_eq!(parse_index("$abc"), None);
        assert_eq!(parse_index("/x/"), None);
    }

    #[test]
    fn pattern_uses_slash_delimiters() {
        assert_eq!(parse_pattern("/edit/"), Ok(Some("edit")));
        assert_eq!(parse_pattern("edit"), Ok(None));
        assert!(parse_pattern("//").is_err());
    }
}
