use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use super::parse_and_run;
use crate::Servers;

#[derive(Debug, Eq, PartialEq)]
enum TopicAction {
    Show,
    Clear,
    Set(String),
}

pub struct TopicCommand {
    servers: Servers,
}

impl TopicCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("topic")
            .description("Show, set, or clear the current Matrix room topic.")
            .add_argument("[--clear] [topic]")
            .arguments_description(
                "topic: The new topic for the current room. Omit it to show the current topic.\n\
                 --clear: Clear the topic.",
            );

        Command::new(
            settings,
            TopicCommand {
                servers: servers.clone(),
            },
        )
    }

    fn parser() -> Argparse<'static, 'static> {
        Argparse::new("topic")
            .about("Show, set, or clear the current Matrix room topic.")
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(
                Arg::with_name("clear")
                    .long("clear")
                    .help("Clear the current room topic.")
                    .conflicts_with("topic"),
            )
            .arg(
                Arg::with_name("topic")
                    .multiple(true)
                    .allow_hyphen_values(true),
            )
    }

    fn action(matches: &clap::ArgMatches) -> TopicAction {
        if matches.is_present("clear") {
            TopicAction::Clear
        } else {
            matches
                .values_of("topic")
                .map(|t| TopicAction::Set(t.collect::<Vec<_>>().join(" ")))
                .unwrap_or(TopicAction::Show)
        }
    }

    fn show_topic(&self, buffer: &Buffer) {
        if let Some(room) = self.servers.find_room(buffer) {
            match room.room().topic() {
                Some(topic) if !topic.is_empty() => {
                    buffer.print(&format!("Topic: {}", topic));
                }
                _ => {
                    buffer.print("No topic is set.");
                }
            }
        } else {
            Weechat::print(
                "The /topic command needs to be run in a Matrix room buffer.",
            );
        }
    }

    fn set_topic(&self, buffer: &Buffer, topic: String) {
        if let Some(room) = self.servers.find_room(buffer) {
            Weechat::spawn(async move { room.set_topic(topic).await }).detach();
        } else {
            Weechat::print(
                "The /topic command needs to be run in a Matrix room buffer.",
            );
        }
    }
}

impl CommandCallback for TopicCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        parse_and_run(Self::parser(), arguments, |matches| match Self::action(
            matches,
        ) {
            TopicAction::Show => self.show_topic(buffer),
            TopicAction::Clear => self.set_topic(buffer, String::new()),
            TopicAction::Set(topic) => self.set_topic(buffer, topic),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{TopicAction, TopicCommand};

    fn parse(args: &[&str]) -> TopicAction {
        let matches = TopicCommand::parser()
            .get_matches_from_safe(args)
            .expect("topic arguments should parse");

        TopicCommand::action(&matches)
    }

    #[test]
    fn omitted_topic_shows_current_topic() {
        assert_eq!(TopicAction::Show, parse(&["topic"]));
    }

    #[test]
    fn clear_flag_explicitly_clears_topic() {
        assert_eq!(TopicAction::Clear, parse(&["topic", "--clear"]));
    }

    #[test]
    fn clear_flag_rejects_topic_text() {
        assert!(TopicCommand::parser()
            .get_matches_from_safe(&["topic", "--clear", "not empty"])
            .is_err());
    }

    #[test]
    fn non_empty_topic_selects_set_action() {
        assert_eq!(
            TopicAction::Set("new room topic".to_owned()),
            parse(&["topic", "new", "room", "topic"])
        );
    }
}
