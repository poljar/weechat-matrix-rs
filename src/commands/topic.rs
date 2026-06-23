use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::Servers;

pub struct TopicCommand {
    servers: Servers,
}

impl TopicCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("topic")
            .description("Set or clear the current Matrix room topic.")
            .add_argument("[topic]")
            .arguments_description(
                "topic: The new topic for the current room. Omit it to clear the topic.",
            );

        Command::new(
            settings,
            TopicCommand {
                servers: servers.clone(),
            },
        )
    }

    fn set_topic(&self, buffer: &Buffer, topic: String) {
        if let Some(room) = self.servers.find_room(buffer) {
            let room = room.room().clone();

            match self.servers.runtime().block_on(room.set_room_topic(&topic)) {
                Ok(_) => (),
                Err(error) => Weechat::print(&format!(
                    "{}Failed to set room topic: {}",
                    Weechat::prefix(Prefix::Error),
                    error
                )),
            }
        } else {
            Weechat::print(
                "The /topic command needs to be run in a Matrix room buffer.",
            );
        }
    }
}

impl CommandCallback for TopicCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let parser = Argparse::new("topic")
            .about("Set or clear the current Matrix room topic.")
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(
                Arg::with_name("topic")
                    .multiple(true)
                    .allow_hyphen_values(true),
            );

        parse_and_run(parser, arguments, |matches| {
            let topic = matches
                .values_of("topic")
                .map(|t| t.collect::<Vec<_>>().join(" "));

            self.set_topic(buffer, topic.unwrap_or_default());
        });
    }
}
