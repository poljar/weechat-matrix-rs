use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use matrix_sdk::ruma::{
    events::{
        relation::InReplyTo,
        room::message::{Relation, RoomMessageEventContent},
    },
    EventId, OwnedEventId,
};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::Servers;

const EVENT_ID_TAG_PREFIX: &str = "matrix_id_";

pub struct ReplyCommand {
    servers: Servers,
}

impl ReplyCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("reply")
            .description("Reply to a Matrix event in the current room.")
            .add_argument("[event-id|index] <message>")
            .arguments_description(
                "event-id: The Matrix event ID to reply to.
    index: 1-based recent event index. 1, 0, or -1 select the latest event; \
           2 or -2 select the event before that.
  message: Reply message text. If no event ID or index is given, all text is \
           sent as a reply to the latest Matrix event in the buffer.",
            );

        Command::new(
            settings,
            ReplyCommand {
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

    fn parse_arguments(
        buffer: &Buffer,
        arguments: Option<Vec<&str>>,
    ) -> Result<(OwnedEventId, String), String> {
        let Some(arguments) =
            arguments.filter(|arguments| !arguments.is_empty())
        else {
            return Err("Usage: /reply [event-id|index] <message>".to_owned());
        };

        let (event_id, message) =
            if let Some((first, rest)) = arguments.split_first() {
                if first.starts_with('$') && EventId::parse(*first).is_err() {
                    return Err(format!("Invalid Matrix event ID: {}", first));
                }

                if let Ok(event_id) = EventId::parse(*first) {
                    (event_id, rest.join(" "))
                } else if let Some(index) = Self::parse_index(first) {
                    let event_id = Self::event_id_at_index(buffer, index)
                        .ok_or_else(|| {
                            format!(
                                "No Matrix event found at reply index {}.",
                                first
                            )
                        })?;

                    (event_id, rest.join(" "))
                } else {
                    let event_id =
                        Self::latest_event_id(buffer).ok_or_else(|| {
                            "No Matrix event found to reply to.".to_owned()
                        })?;

                    (event_id, arguments.join(" "))
                }
            } else {
                unreachable!("empty arguments were filtered above");
            };

        if message.is_empty() {
            Err("Reply message cannot be empty.".to_owned())
        } else {
            Ok((event_id, message))
        }
    }

    fn reply(&self, buffer: &Buffer, event_id: OwnedEventId, message: String) {
        if let Some(room) = self.servers.find_room(buffer) {
            Weechat::spawn(async move {
                room.send_message(reply_content(event_id, message)).await
            })
            .detach();
        } else {
            Weechat::print(
                "The /reply command needs to be run in a Matrix room buffer.",
            );
        }
    }
}

impl CommandCallback for ReplyCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("reply")
            .setting(ArgParseSettings::NoBinaryName)
            .arg(Arg::with_name("arguments").multiple(true));

        parse_and_run(argparse, arguments, |args| match Self::parse_arguments(
            buffer,
            args.values_of("arguments").map(|v| v.collect()),
        ) {
            Ok((event_id, message)) => self.reply(buffer, event_id, message),
            Err(error) => buffer.print(&format!(
                "{}matrix: {}",
                Weechat::prefix(Prefix::Error),
                error
            )),
        });
    }
}

fn reply_content(
    event_id: OwnedEventId,
    message: String,
) -> RoomMessageEventContent {
    let mut content = RoomMessageEventContent::text_plain(message);
    content.relates_to = Some(Relation::Reply {
        in_reply_to: InReplyTo::new(event_id),
    });
    content
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{events::room::message::Relation, owned_event_id};

    use super::{reply_content, ReplyCommand};

    #[test]
    fn reply_index_accepts_recent_event_forms() {
        assert_eq!(Some(1), ReplyCommand::parse_index("1"));
        assert_eq!(Some(1), ReplyCommand::parse_index("0"));
        assert_eq!(Some(1), ReplyCommand::parse_index("-1"));
        assert_eq!(Some(2), ReplyCommand::parse_index("2"));
        assert_eq!(Some(2), ReplyCommand::parse_index("-2"));
        assert_eq!(None, ReplyCommand::parse_index("abc"));
    }

    #[test]
    fn reply_content_sets_reply_relation() {
        let event_id = owned_event_id!("$replyevent:example.org");
        let content = reply_content(event_id.clone(), "Thanks".to_owned());

        let Some(Relation::Reply { in_reply_to }) = content.relates_to else {
            panic!("reply command must create a Matrix reply relation");
        };

        assert_eq!(event_id, in_reply_to.event_id);
    }
}
