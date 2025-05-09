use clap::{Arg, ArgMatches, Command as ArgParse};

use matrix_sdk::ruma::UserId;
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use super::parse_and_run;
use crate::Servers;

pub struct UnbanCommand {
    servers: Servers,
}

impl UnbanCommand {
    pub const NAME: &'static str = "unban";
    pub const DESCRIPTION: &'static str = "Unban user from room";
    pub const COMPLETION: &'static str = "%(matrix-users) %-";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new(Self::NAME)
            .description(Self::DESCRIPTION)
            .add_argument("<user> [reason]")
            .arguments_description(
                "user: Matrix ID to unban\nreason: Reason for the unban [optional]",
            )
            .add_completion(Self::COMPLETION);

        Command::new(
            settings,
            Self {
                servers: servers.clone(),
            },
        )
    }

    pub fn run(&self, buffer: &Buffer, args: &ArgMatches) {
        let user = args
            .get_one::<String>("user")
            .expect("User not set but was required");
        let reason = args.get_one::<String>("reason");
        let user_id = UserId::parse(user).expect("Couldn't parse UserId");

        if let Some(room) = self.servers.find_room(buffer) {
            let room = room.room().clone();
            let reason = reason.map(|s| s.to_owned());
            let ban = || async move {
                room.unban_user(&user_id, reason.as_deref()).await
            };

            if let Err(err) = self.servers.runtime().block_on(ban()) {
                Weechat::print(&format!("Failed to unban user: {err}"));
            }
        }
    }
}

impl CommandCallback for UnbanCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = ArgParse::new(Self::NAME)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .disable_help_subcommand(true)
            .about(Self::DESCRIPTION)
            .arg(Arg::new("user").required(true))
            .arg(
                Arg::new("reason")
                    .required(false)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true),
            );

        parse_and_run(argparse, arguments, |matches| self.run(buffer, matches));
    }
}
