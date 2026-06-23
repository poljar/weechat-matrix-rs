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

pub struct RedactCommand {
    servers: Servers,
}

impl RedactCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("redact")
            .description("Redact a Matrix event in the current room.")
            .add_argument("[event-id] [reason...]")
            .arguments_description(
                "event-id: The Matrix event ID to redact. If omitted, the latest \
                 unredacted event in the current buffer is used.
  reason: Optional reason for the redaction. If no event ID is given, all text \
          is used as the reason for redacting the latest unredacted event.",
            );

        Command::new(
            settings,
            RedactCommand {
                servers: servers.clone(),
            },
        )
    }

    fn latest_event_id(buffer: &Buffer) -> Option<OwnedEventId> {
        buffer.lines().rev().find_map(|line| {
            let tags = line.tags();

            if tags.iter().any(|tag| tag.as_ref() == "matrix_redacted") {
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
            let room = room.room().clone();

            match self.servers.runtime().block_on(room.redact(
                &event_id,
                reason.as_deref(),
                None,
            )) {
                Ok(_) => (),
                Err(error) => Weechat::print(&format!(
                    "{}Failed to redact {}: {}",
                    Weechat::prefix(Prefix::Error),
                    event_id,
                    error
                )),
            }
        } else {
            Weechat::print(
                "The /redact command needs to be run in a Matrix room buffer.",
            );
        }
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
