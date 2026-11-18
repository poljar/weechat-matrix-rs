use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use matrix_sdk::ruma::{EventId, OwnedEventId};
use fancy_regex::{Captures, Regex};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::{room::thread_root_localvar, Servers};

const EVENT_ID_TAG_PREFIX: &str = "matrix_id_";

/// A Perl-style pattern: `\/` is a literal slash and `flags` are the
/// letters after the last slash (`i`, `m`, `s`, `x`; `g` is handled by the
/// caller).
struct Pattern(Regex);

impl Pattern {
    /// With `regex` off, `pattern` is literal text.
    fn new(pattern: &str, flags: &str, regex: bool) -> Result<Self, String> {
        let inline: String =
            flags.chars().filter(|flag| "imsx".contains(*flag)).collect();
        let source = pattern.replace("\\/", "/");
        let source = if regex {
            source
        } else {
            fancy_regex::escape(&source).into_owned()
        };
        let source = if inline.is_empty() {
            source
        } else {
            format!("(?{inline}){source}")
        };

        Regex::new(&source)
            .map(Self)
            .map_err(|error| format!("Invalid pattern /{pattern}/: {error}"))
    }

    fn is_match(&self, message: &str) -> bool {
        self.0.is_match(message).unwrap_or(false)
    }

    /// Replace the first match in `message`, or every match if `global`.
    fn substitute(
        &self,
        message: &str,
        replacement: &Replacement,
        global: bool,
    ) -> Result<String, String> {
        let mut output = String::with_capacity(message.len());
        let mut last = 0;

        for captures in self.0.captures_iter(message) {
            let captures = captures
                .map_err(|error| format!("Pattern failed to match: {error}"))?;
            let Some(matched) = captures.get(0) else {
                continue;
            };
            output.push_str(&message[last..matched.start()]);
            replacement.expand(&captures, &mut output);
            last = matched.end();
            if !global {
                break;
            }
        }
        output.push_str(&message[last..]);

        Ok(output)
    }
}

/// One piece of a parsed replacement.
#[derive(Debug, Eq, PartialEq)]
enum ReplacementPart {
    Literal(String),
    Group(usize),
    Named(String),
}

/// A Perl-style replacement: `$1`, `${1}`, `\1`, `${name}` and `$&` refer
/// to groups; `\/`, `\\`, `\$`, `\n` and `\t` are escapes.
#[derive(Debug, Eq, PartialEq)]
struct Replacement(Vec<ReplacementPart>);

impl Replacement {
    /// With `regex` off, only `\/` is special.
    fn new(text: &str, regex: bool) -> Self {
        if !regex {
            let literal = text.replace("\\/", "/");
            return Self(vec![ReplacementPart::Literal(literal)]);
        }

        let mut parts = Vec::new();
        let mut literal = String::new();
        let flush = |literal: &mut String, parts: &mut Vec<_>| {
            if !literal.is_empty() {
                parts.push(ReplacementPart::Literal(std::mem::take(literal)));
            }
        };
        let mut chars = text.chars().peekable();

        while let Some(c) = chars.next() {
            match (c, chars.peek().copied()) {
                ('\\', Some(next)) => {
                    chars.next();
                    match next {
                        'n' => literal.push('\n'),
                        't' => literal.push('\t'),
                        '1'..='9' => {
                            flush(&mut literal, &mut parts);
                            let group = next as usize - '0' as usize;
                            parts.push(ReplacementPart::Group(group));
                        }
                        other => literal.push(other),
                    }
                }
                ('$', Some('&')) => {
                    chars.next();
                    flush(&mut literal, &mut parts);
                    parts.push(ReplacementPart::Group(0));
                }
                ('$', Some(next)) if next.is_ascii_digit() => {
                    let mut digits = String::new();
                    while let Some(digit) =
                        chars.next_if(|c| c.is_ascii_digit())
                    {
                        digits.push(digit);
                    }
                    flush(&mut literal, &mut parts);
                    parts.push(ReplacementPart::Group(
                        digits.parse().unwrap_or(usize::MAX),
                    ));
                }
                ('$', Some('{')) => {
                    let rest: String = chars.clone().skip(1).collect();
                    if let Some((name, _)) = rest.split_once('}') {
                        for _ in 0..name.chars().count() + 2 {
                            chars.next();
                        }
                        flush(&mut literal, &mut parts);
                        parts.push(match name.parse() {
                            Ok(group) => ReplacementPart::Group(group),
                            Err(_) => ReplacementPart::Named(name.to_owned()),
                        });
                    } else {
                        literal.push('$');
                    }
                }
                (other, _) => literal.push(other),
            }
        }
        flush(&mut literal, &mut parts);

        Self(parts)
    }

    fn expand(&self, captures: &Captures<'_, str>, output: &mut String) {
        for part in &self.0 {
            let group = match part {
                ReplacementPart::Literal(text) => {
                    output.push_str(text);
                    continue;
                }
                ReplacementPart::Group(index) => captures.get(*index),
                ReplacementPart::Named(name) => captures.name(name),
            };
            if let Some(group) = group {
                output.push_str(group.as_str());
            }
        }
    }
}

/// Split `text` at its first `/` that is not escaped as `\/`.
pub(crate) fn split_at_slash(text: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        match c {
            '/' if !escaped => {
                return Some((&text[..index], &text[index + 1..]))
            }
            '\\' => escaped = !escaped,
            _ => escaped = false,
        }
    }
    None
}

/// Whether `flags` holds only substitution flags.
pub(crate) fn are_flags(flags: &str) -> bool {
    flags.chars().all(|flag| "gimsx".contains(flag))
}

/// What the text of an edit asks for.
#[derive(Debug, Eq, PartialEq)]
enum EditText<'a> {
    /// `/pattern/replacement/flags` or `s/pattern/replacement/flags`:
    /// replace the matched text and keep the rest of the message.
    Replace {
        pattern: &'a str,
        replacement: &'a str,
        flags: &'a str,
    },
    /// `/pattern/ message`: replace the whole matching message.
    PatternThenMessage { pattern: &'a str, message: &'a str },
    /// A new message body.
    Message(&'a str),
}

/// Parse the text of an edit. An opened but unclosed pattern is an error,
/// so a mistyped pattern can't replace a message with itself as text.
fn parse_edit_text(text: &str) -> Result<EditText<'_>, String> {
    let (sed, body) = if let Some(body) = text.strip_prefix("s/") {
        (true, body)
    } else if let Some(body) = text.strip_prefix('/') {
        (false, body)
    } else {
        return Ok(EditText::Message(text));
    };

    let Some((pattern, rest)) = split_at_slash(body) else {
        // `s/word` can only be a message; `/word` is a broken pattern.
        return if sed {
            Ok(EditText::Message(text))
        } else {
            Err(format!(
                "Unterminated pattern in {text}; use /pattern/replacement/ \
                 or /pattern/ message."
            ))
        };
    };

    if pattern.is_empty() {
        return Err("The edit pattern cannot be empty.".to_owned());
    }

    if let Some((replacement, flags)) = split_at_slash(rest) {
        if are_flags(flags) {
            return Ok(EditText::Replace {
                pattern,
                replacement,
                flags,
            });
        }
    }

    if rest.is_empty() {
        return Err(format!("Edit message cannot be empty for /{pattern}/."));
    }

    match rest.strip_prefix(' ') {
        Some(message) if !sed => Ok(EditText::PatternThenMessage {
            pattern,
            message: message.trim_start(),
        }),
        _ => Err(format!(
            "Unterminated replacement or unknown flags in {text}; use \
             /pattern/replacement/ with flags from gimsx, or /pattern/ \
             message."
        )),
    }
}

pub struct EditCommand {
    servers: Servers,
}

impl EditCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("edit")
            .description("Edit a Matrix event in the current room.")
            .add_argument("[event-id|index|/pattern/] <message>")
            .arguments_description(
                "event-id: The Matrix event ID to edit.
    index: 1-based recent unredacted event index. 1, 0, or -1 select the latest \
           event; 2 or -2 select the event before that.
  pattern: A Perl-style regular expression enclosed in slashes (literal \
           text with matrix-rust.input.edit_regex off); write a literal \
           slash as \\/. The most recent unredacted message \
           matching it is selected.
  message: New plain-text message body. The shorthand \
           /pattern/replacement/flags (or s/pattern/replacement/flags) \
           selects the most recent matching message and replaces the first \
           match; an empty replacement deletes it. Flags: g replaces every \
           match, i ignores case, m, s and x as in Perl. The replacement \
           may use $1, \\1, ${name} and $&. An unclosed pattern is an \
           error, not a message. Typing s/pattern/replacement/ as a \
           message does the same (matrix-rust.input.sed_edit).",
            );

        Command::new(
            settings,
            EditCommand {
                servers: servers.clone(),
            },
        )
    }

    fn latest_event_id(buffer: &Buffer) -> Option<OwnedEventId> {
        Self::event_id_at_index(buffer, 1)
    }

    fn event_id_at_index(
        buffer: &Buffer,
        index: usize,
    ) -> Option<OwnedEventId> {
        buffer
            .lines()
            .rev()
            .filter_map(|line| {
                let tags = line.tags();

                if tags.iter().any(|tag| tag.as_ref() == "matrix_redacted") {
                    return None;
                }

                tags.iter()
                    .find_map(|tag| {
                        tag.as_ref().strip_prefix(EVENT_ID_TAG_PREFIX)
                    })
                    .and_then(|event_id| EventId::parse(event_id).ok())
            })
            .nth(index.saturating_sub(1))
    }

    fn parse_index(argument: &str) -> Option<usize> {
        let index = argument.parse::<isize>().ok()?;

        match index {
            0 | -1 => Some(1),
            n if n > 0 => Some(n as usize),
            n => n.checked_abs().map(|n| n as usize),
        }
    }

    /// The text of `event_id` as shown in `buffer`: every line of a
    /// multi-line message, without colors.
    fn message_of_event(buffer: &Buffer, event_id: &EventId) -> Option<String> {
        let tag = format!("{EVENT_ID_TAG_PREFIX}{event_id}");
        let lines: Vec<String> = buffer
            .lines()
            .filter(|line| line.tags().iter().any(|t| t.as_ref() == tag))
            .map(|line| Weechat::remove_color(&line.message()))
            .collect();

        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// The most recent unredacted event with a line matching `pattern`,
    /// with the event's whole text.
    fn event_matching_pattern(
        buffer: &Buffer,
        pattern: &Pattern,
    ) -> Option<(OwnedEventId, String)> {
        let event_id = buffer.lines().rev().find_map(|line| {
            let tags = line.tags();

            if tags.iter().any(|tag| tag.as_ref() == "matrix_redacted")
                || !pattern.is_match(&Weechat::remove_color(&line.message()))
            {
                return None;
            }

            tags.iter()
                .find_map(|tag| tag.as_ref().strip_prefix(EVENT_ID_TAG_PREFIX))
                .and_then(|event_id| EventId::parse(event_id).ok())
        })?;
        let message = Self::message_of_event(buffer, &event_id)?;

        Some((event_id, message))
    }

    pub(crate) fn parse_arguments(
        buffer: &Buffer,
        arguments: Option<Vec<&str>>,
        regex: bool,
    ) -> Result<(OwnedEventId, String), String> {
        let Some(arguments) =
            arguments.filter(|arguments| !arguments.is_empty())
        else {
            return Err(
                "Usage: /edit [event-id|index|/pattern/] <message>".to_owned()
            );
        };

        let Some((first, rest)) = arguments.split_first() else {
            unreachable!("empty arguments were filtered above");
        };

        if first.starts_with('$') && EventId::parse(*first).is_err() {
            return Err(format!("Invalid Matrix event ID: {}", first));
        }

        let target = if let Ok(event_id) = EventId::parse(*first) {
            Some(event_id)
        } else if let Some(index) = Self::parse_index(first) {
            Some(Self::event_id_at_index(buffer, index).ok_or_else(|| {
                format!("No Matrix event found at edit index {}.", first)
            })?)
        } else {
            None
        };
        let text = if target.is_some() {
            rest.join(" ")
        } else {
            arguments.join(" ")
        };

        let (event_id, message) = match (target, parse_edit_text(&text)?) {
            (
                Some(event_id),
                EditText::Replace {
                    pattern,
                    replacement,
                    flags,
                },
            ) => {
                let original = Self::message_of_event(buffer, &event_id)
                    .ok_or_else(|| {
                        format!("Event {event_id} is not shown in this buffer.")
                    })?;
                let matcher = Pattern::new(pattern, flags, regex)?;
                if !matcher.is_match(&original) {
                    return Err(format!(
                        "Event {event_id} does not match /{pattern}/."
                    ));
                }
                let message = matcher.substitute(
                    &original,
                    &Replacement::new(replacement, regex),
                    flags.contains('g'),
                )?;

                (event_id, message)
            }
            (Some(event_id), EditText::Message(message)) => {
                (event_id, message.to_owned())
            }
            (Some(_), EditText::PatternThenMessage { .. }) => {
                return Err("Give an event ID or index, or a /pattern/, \
                            not both."
                    .to_owned());
            }
            (
                None,
                EditText::Replace {
                    pattern,
                    replacement,
                    flags,
                },
            ) => {
                let matcher = Pattern::new(pattern, flags, regex)?;
                let (event_id, original) = Self::event_matching_pattern(
                    buffer, &matcher,
                )
                .ok_or_else(|| {
                    format!("No unredacted Matrix event matches /{pattern}/.")
                })?;
                let message = matcher.substitute(
                    &original,
                    &Replacement::new(replacement, regex),
                    flags.contains('g'),
                )?;

                (event_id, message)
            }
            (None, EditText::PatternThenMessage { pattern, message }) => {
                let matcher = Pattern::new(pattern, "", regex)?;
                let (event_id, _) = Self::event_matching_pattern(
                    buffer, &matcher,
                )
                .ok_or_else(|| {
                    format!("No unredacted Matrix event matches /{pattern}/.")
                })?;

                (event_id, message.to_owned())
            }
            (None, EditText::Message(message)) => {
                let event_id =
                    Self::latest_event_id(buffer).ok_or_else(|| {
                        "No Matrix event found to edit.".to_owned()
                    })?;

                (event_id, message.to_owned())
            }
        };

        if message.is_empty() {
            Err("Edit message cannot be empty.".to_owned())
        } else {
            Ok((event_id, message))
        }
    }

    fn parser() -> Argparse<'static, 'static> {
        Argparse::new("edit")
            .settings(&[
                ArgParseSettings::AllowLeadingHyphen,
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(Arg::with_name("arguments").multiple(true))
    }

    fn edit_regex(&self, buffer: &Buffer) -> bool {
        self.servers
            .find_room(buffer)
            .is_some_and(|room| room.edit_regex())
    }

    fn edit(&self, buffer: &Buffer, event_id: OwnedEventId, message: String) {
        if let Some(room) = self.servers.find_room(buffer) {
            let thread_root = thread_root_localvar(buffer);
            Weechat::spawn(async move {
                room.send_edit(event_id, message, thread_root).await
            })
            .detach();
        } else {
            Weechat::print(
                "The /edit command needs to be run in a Matrix room buffer.",
            );
        }
    }
}

impl CommandCallback for EditCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        parse_and_run(Self::parser(), arguments, |args| {
            match Self::parse_arguments(
                buffer,
                args.values_of("arguments").map(|v| v.collect()),
                self.edit_regex(buffer),
            ) {
                Ok((event_id, message)) => self.edit(buffer, event_id, message),
                Err(error) => buffer.print(&format!(
                    "{}matrix: {}",
                    Weechat::prefix(Prefix::Error),
                    error
                )),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_edit_text, EditCommand, EditText, Pattern, Replacement,
    };

    #[test]
    fn edit_index_accepts_recent_event_forms() {
        assert_eq!(Some(1), EditCommand::parse_index("1"));
        assert_eq!(Some(1), EditCommand::parse_index("0"));
        assert_eq!(Some(1), EditCommand::parse_index("-1"));
        assert_eq!(Some(2), EditCommand::parse_index("2"));
        assert_eq!(Some(2), EditCommand::parse_index("-2"));
        assert_eq!(None, EditCommand::parse_index("abc"));
    }

    #[test]
    fn edit_text_recognizes_replacements_with_spaces() {
        assert_eq!(
            Ok(EditText::Replace {
                pattern: "teh",
                replacement: "the",
                flags: "",
            }),
            parse_edit_text("/teh/the/")
        );
        assert_eq!(
            Ok(EditText::Replace {
                pattern: ", if it's based on an MA thesis, ",
                replacement: ", ",
                flags: "",
            }),
            parse_edit_text("s/, if it's based on an MA thesis, /, /")
        );
        assert_eq!(
            Ok(EditText::Replace {
                pattern: " mako",
                replacement: "",
                flags: "",
            }),
            parse_edit_text("/ mako//")
        );
        assert_eq!(
            Ok(EditText::Replace {
                pattern: "and\\/or",
                replacement: "a\\/b",
                flags: "gi",
            }),
            parse_edit_text("s/and\\/or/a\\/b/gi")
        );
    }

    #[test]
    fn edit_text_keeps_pattern_then_message_and_plain_text() {
        assert_eq!(
            Ok(EditText::PatternThenMessage {
                pattern: "teh cat",
                message: "the dog",
            }),
            parse_edit_text("/teh cat/ the dog")
        );
        assert_eq!(
            Ok(EditText::Message("teh/the/")),
            parse_edit_text("teh/the/")
        );
        assert_eq!(Ok(EditText::Message("hello")), parse_edit_text("hello"));
        assert_eq!(Ok(EditText::Message("s/he")), parse_edit_text("s/he"));
    }

    #[test]
    fn edit_text_rejects_broken_patterns_instead_of_sending_them() {
        assert!(parse_edit_text("/teh").is_err());
        assert!(parse_edit_text("/teh/the").is_err());
        assert!(parse_edit_text("s/teh/the").is_err());
        assert!(parse_edit_text("s/teh/the/q").is_err());
        assert!(parse_edit_text("s/a/b\\/").is_err());
        assert_eq!(
            Err("The edit pattern cannot be empty.".to_owned()),
            parse_edit_text("//the/")
        );
        assert_eq!(
            Err("Edit message cannot be empty for /teh/.".to_owned()),
            parse_edit_text("/teh/")
        );
    }

    fn substitute(
        pattern: &str,
        replacement: &str,
        flags: &str,
        message: &str,
    ) -> String {
        Pattern::new(pattern, flags, true)
            .unwrap()
            .substitute(
                message,
                &Replacement::new(replacement, true),
                flags.contains('g'),
            )
            .unwrap()
    }

    #[test]
    fn substitution_replaces_the_first_match_unless_global() {
        assert_eq!(
            "the cat ate teh kibble",
            substitute("teh", "the", "", "teh cat ate teh kibble")
        );
        assert_eq!(
            "the cat ate the kibble",
            substitute("teh", "the", "g", "teh cat ate teh kibble")
        );
        assert_eq!("hi there", substitute(" mako", "", "", "hi mako there"));
    }

    #[test]
    fn substitution_follows_perl_syntax() {
        assert_eq!("b a", substitute(r"(\w+) (\w+)", r"$2 \1", "", "a b"));
        assert_eq!("[a] b", substitute(r"\w+", "[$&]", "", "a b"));
        assert_eq!(
            "x-1",
            substitute(r"(?<n>\d)(?<l>\w)", "${l}-${n}", "", "1x")
        );
        assert_eq!("the the", substitute(r"(\w+) \1", "$1", "", "the the the"));
        assert_eq!("X", substitute("teh", "X", "i", "TEH"));
        assert_eq!("a or b", substitute(r"and\/or", "or", "", "a and/or b"));
        assert_eq!("a/b", substitute("-", r"\/", "", "a-b"));
        assert_eq!("$5", substitute("cost", r"\$5", "", "cost"));
        assert_eq!("bar", substitute("foo(?=bar)", "", "", "foobar"));
        assert!(Pattern::new("(", "", true).is_err());
    }

    #[test]
    fn literal_mode_takes_pattern_and_replacement_as_text() {
        let pattern = Pattern::new("a.b (", "g", false).unwrap();
        assert!(!pattern.is_match("axb ("));
        assert_eq!(
            "$1 and $1",
            pattern
                .substitute(
                    "a.b ( and a.b (",
                    &Replacement::new("$1", false),
                    true
                )
                .unwrap()
        );
    }

    #[test]
    fn edit_pattern_matches_message_text() {
        let pattern = Pattern::new("/edit", "", true).unwrap();
        assert!(pattern.is_match("Something like /edit /pattern/replacement/"));
        assert!(!pattern.is_match("Something like /reply /pattern/"));
    }

    #[test]
    fn command_parser_accepts_negative_edit_indexes() {
        let matches = EditCommand::parser()
            .get_matches_from_safe(vec!["edit", "-2", "message"])
            .unwrap();
        let arguments: Vec<_> =
            matches.values_of("arguments").unwrap().collect();

        assert_eq!(vec!["-2", "message"], arguments);
    }

    #[test]
    fn command_parser_accepts_pattern_replacement() {
        let matches = EditCommand::parser()
            .get_matches_from_safe(vec!["edit", "/teh/the/"])
            .unwrap();
        let arguments: Vec<_> =
            matches.values_of("arguments").unwrap().collect();

        assert_eq!(vec!["/teh/the/"], arguments);
    }
}
