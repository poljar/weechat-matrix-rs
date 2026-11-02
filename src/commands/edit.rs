use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use matrix_sdk::ruma::{EventId, OwnedEventId};
use regex::Regex;
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::{room::thread_root_localvar, Servers};

const EVENT_ID_TAG_PREFIX: &str = "matrix_id_";

/// The pattern of a `/pattern/` or `/pattern/replacement/` argument.
enum Pattern<'a> {
    Substring(&'a str),
    Regex(Regex),
}

impl<'a> Pattern<'a> {
    fn new(pattern: &'a str, regex: bool) -> Result<Self, String> {
        if regex {
            Regex::new(pattern).map(Self::Regex).map_err(|error| {
                format!("Invalid pattern /{pattern}/: {error}")
            })
        } else {
            Ok(Self::Substring(pattern))
        }
    }

    fn is_match(&self, message: &str) -> bool {
        match self {
            Self::Substring(pattern) => message.contains(pattern),
            Self::Regex(regex) => regex.is_match(message),
        }
    }

    fn replace_all(&self, message: &str, replacement: &str) -> String {
        match self {
            Self::Substring(pattern) => message.replace(pattern, replacement),
            Self::Regex(regex) => {
                regex.replace_all(message, replacement).into_owned()
            }
        }
    }
}

/// What the text of an edit asks for.
#[derive(Debug, Eq, PartialEq)]
enum EditText<'a> {
    /// `/pattern/replacement/` or `s/pattern/replacement/`: replace the
    /// matched text and keep the rest of the message.
    Replace {
        pattern: &'a str,
        replacement: &'a str,
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

    let Some((pattern, rest)) = body.split_once('/') else {
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

    if let Some(replacement) = rest.strip_suffix('/') {
        return Ok(EditText::Replace {
            pattern,
            replacement,
        });
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
            "Unterminated replacement in {text}; use /pattern/replacement/ \
             or /pattern/ message."
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
  pattern: A case-sensitive substring enclosed in slashes, or a regular \
           expression with matrix-rust.input.edit_regex on. The most recent \
           unredacted message matching it is selected.
  message: New plain-text message body. The shorthand /pattern/replacement/ \
           (or s/pattern/replacement/) selects the most recent matching \
           message and replaces that text, spaces included; an empty \
           replacement deletes it. An unclosed pattern is an error, not a \
           message. Typing s/pattern/replacement/ as a message does the same \
           (matrix-rust.input.sed_edit).",
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
        pattern: &Pattern<'_>,
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
                },
            ) => {
                let original = Self::message_of_event(buffer, &event_id)
                    .ok_or_else(|| {
                        format!("Event {event_id} is not shown in this buffer.")
                    })?;
                let matcher = Pattern::new(pattern, regex)?;
                if !matcher.is_match(&original) {
                    return Err(format!(
                        "Event {event_id} does not match /{pattern}/."
                    ));
                }

                (event_id, matcher.replace_all(&original, replacement))
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
                },
            ) => {
                let matcher = Pattern::new(pattern, regex)?;
                let (event_id, original) = Self::event_matching_pattern(
                    buffer, &matcher,
                )
                .ok_or_else(|| {
                    format!("No unredacted Matrix event matches /{pattern}/.")
                })?;

                (event_id, matcher.replace_all(&original, replacement))
            }
            (None, EditText::PatternThenMessage { pattern, message }) => {
                let matcher = Pattern::new(pattern, regex)?;
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
    use super::{parse_edit_text, EditCommand, EditText, Pattern};

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
            }),
            parse_edit_text("/teh/the/")
        );
        assert_eq!(
            Ok(EditText::Replace {
                pattern: ", if it's based on an MA thesis, ",
                replacement: ", ",
            }),
            parse_edit_text("s/, if it's based on an MA thesis, /, /")
        );
        assert_eq!(
            Ok(EditText::Replace {
                pattern: " mako",
                replacement: "",
            }),
            parse_edit_text("/ mako//")
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
        assert_eq!(
            Err("The edit pattern cannot be empty.".to_owned()),
            parse_edit_text("//the/")
        );
        assert_eq!(
            Err("Edit message cannot be empty for /teh/.".to_owned()),
            parse_edit_text("/teh/")
        );
    }

    #[test]
    fn substring_patterns_replace_every_occurrence() {
        let pattern = Pattern::Substring("teh");
        assert_eq!(
            "the cat ate the kibble",
            pattern.replace_all("teh cat ate teh kibble", "the")
        );
        assert_eq!(
            "hi there",
            Pattern::Substring(" mako").replace_all("hi mako there", "")
        );
    }

    #[test]
    fn regex_patterns_match_and_replace_with_groups() {
        let pattern = Pattern::new(r"(\w+) cat", true).unwrap();

        assert!(pattern.is_match("teh cat ate"));
        assert!(!pattern.is_match("cat"));
        assert_eq!(
            "teh dog ate teh kibble",
            pattern.replace_all("teh cat ate teh kibble", "$1 dog")
        );
        assert!(Pattern::new("(", true).is_err());
        assert!(Pattern::new("(", false).unwrap().is_match("a ( b"));
    }

    #[test]
    fn edit_pattern_matches_message_substrings() {
        let pattern = Pattern::Substring("/edit");
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
