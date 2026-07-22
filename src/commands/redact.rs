use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use matrix_sdk::ruma::{EventId, OwnedEventId};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::Servers;

const EVENT_ID_TAG_PREFIX: &str = "matrix_id_";

fn message_matches_pattern(message: &str, pattern: &str) -> bool {
    message.contains(pattern)
}

pub struct RedactCommand {
    servers: Servers,
}

impl RedactCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("redact")
            .description("Redact a Matrix event in the current room.")
            .add_argument("[event-id|index|/pattern/] [reason...]")
            .arguments_description(
                "event-id: The Matrix event ID to redact.
    index: 1-based recent unredacted event index. 1, 0, or -1 select the latest \
           event; 2 or -2 select the event before that.
  pattern: A case-sensitive substring enclosed in slashes. The most recent \
           unredacted message containing it is selected.
   reason: Optional reason for the redaction. If no event ID or index is given, \
           all text is used as the reason for redacting the latest unredacted \
           event.",
            );

        Command::new(
            settings,
            RedactCommand {
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

    fn parse_pattern(argument: &str) -> Result<Option<&str>, String> {
        let Some(pattern) = argument
            .strip_prefix('/')
            .and_then(|argument| argument.strip_suffix('/'))
        else {
            return Ok(None);
        };

        if pattern.is_empty() {
            Err("The redaction pattern cannot be empty.".to_owned())
        } else {
            Ok(Some(pattern))
        }
    }

    fn event_id_matching_pattern(
        buffer: &Buffer,
        pattern: &str,
    ) -> Option<OwnedEventId> {
        buffer.lines().rev().find_map(|line| {
            let tags = line.tags();

            if tags.iter().any(|tag| tag.as_ref() == "matrix_redacted")
                || !message_matches_pattern(
                    &Weechat::remove_color(&line.message()),
                    pattern,
                )
            {
                return None;
            }

            tags.iter()
                .find_map(|tag| tag.as_ref().strip_prefix(EVENT_ID_TAG_PREFIX))
                .and_then(|event_id| EventId::parse(event_id).ok())
        })
    }

    fn parse_arguments(
        buffer: &Buffer,
        arguments: Option<Vec<&str>>,
    ) -> Result<(OwnedEventId, Option<String>), String> {
        let Some(arguments) = arguments else {
            return Self::latest_event_id(buffer)
                .map(|event_id| (event_id, None))
                .ok_or_else(|| "No Matrix event found to redact.".to_owned());
        };

        let Some((first, rest)) = arguments.split_first() else {
            return Self::latest_event_id(buffer)
                .map(|event_id| (event_id, None))
                .ok_or_else(|| "No Matrix event found to redact.".to_owned());
        };

        if first.starts_with('$') && EventId::parse(*first).is_err() {
            return Err(format!("Invalid Matrix event ID: {}", first));
        }

        if let Ok(event_id) = EventId::parse(*first) {
            let reason = if rest.is_empty() {
                None
            } else {
                Some(rest.join(" "))
            };

            Ok((event_id, reason))
        } else if let Some(index) = Self::parse_index(first) {
            let reason = if rest.is_empty() {
                None
            } else {
                Some(rest.join(" "))
            };

            Self::event_id_at_index(buffer, index)
                .map(|event_id| (event_id, reason))
                .ok_or_else(|| {
                    format!(
                        "No Matrix event found at redaction index {}.",
                        first
                    )
                })
        } else if let Some(pattern) = Self::parse_pattern(first)? {
            let reason = if rest.is_empty() {
                None
            } else {
                Some(rest.join(" "))
            };

            Self::event_id_matching_pattern(buffer, pattern)
                .map(|event_id| (event_id, reason))
                .ok_or_else(|| {
                    format!("No unredacted Matrix event matches /{}/.", pattern)
                })
        } else {
            Self::latest_event_id(buffer)
                .map(|event_id| (event_id, Some(arguments.join(" "))))
                .ok_or_else(|| "No Matrix event found to redact.".to_owned())
        }
    }

    fn redact(
        &self,
        buffer: &Buffer,
        event_id: OwnedEventId,
        reason: Option<String>,
    ) {
        if let Some(room) = self.servers.find_room(buffer) {
            Weechat::spawn(async move {
                room.send_redaction(event_id, reason).await
            })
            .detach();
        } else {
            Weechat::print(
                "The /redact command needs to be run in a Matrix room buffer.",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{message_matches_pattern, RedactCommand};

    #[test]
    fn redaction_index_accepts_recent_event_forms() {
        assert_eq!(Some(1), RedactCommand::parse_index("1"));
        assert_eq!(Some(1), RedactCommand::parse_index("0"));
        assert_eq!(Some(1), RedactCommand::parse_index("-1"));
        assert_eq!(Some(2), RedactCommand::parse_index("2"));
        assert_eq!(Some(2), RedactCommand::parse_index("-2"));
    }

    #[test]
    fn redaction_index_rejects_non_indices() {
        assert_eq!(None, RedactCommand::parse_index("reason"));
        assert_eq!(None, RedactCommand::parse_index("$event:example.org"));
    }

    #[test]
    fn redaction_pattern_uses_slash_delimiters() {
        assert_eq!(
            Ok(Some("needle")),
            RedactCommand::parse_pattern("/needle/")
        );
        assert_eq!(Ok(None), RedactCommand::parse_pattern("needle"));
        assert_eq!(Ok(None), RedactCommand::parse_pattern("/needle"));
    }

    #[test]
    fn redaction_pattern_rejects_empty_match() {
        assert_eq!(
            Err("The redaction pattern cannot be empty.".to_owned()),
            RedactCommand::parse_pattern("//")
        );
    }

    #[test]
    fn redaction_pattern_matches_message_substrings() {
        assert!(message_matches_pattern(
            "the self-hosted version",
            "self-hosted"
        ));
        assert!(!message_matches_pattern(
            "the hosted version",
            "self-hosted"
        ));
    }
}

impl CommandCallback for RedactCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let parser = Argparse::new("redact")
            .about("Redact a Matrix event in the current room.")
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(
                Arg::with_name("arguments")
                    .multiple(true)
                    .allow_hyphen_values(true),
            );

        parse_and_run(parser, arguments, |matches| {
            let arguments = matches.values_of("arguments").map(|a| a.collect());

            match Self::parse_arguments(buffer, arguments) {
                Ok((event_id, reason)) => self.redact(buffer, event_id, reason),
                Err(error) => Weechat::print(&format!(
                    "{}{}",
                    Weechat::prefix(Prefix::Error),
                    error
                )),
            }
        });
    }
}
