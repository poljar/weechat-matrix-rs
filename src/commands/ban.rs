use clap::{Arg, ArgMatches, Command as ArgParse};

use matrix_sdk::ruma::UserId;
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use super::parse_and_run;
use crate::Servers;

pub struct BanCommand {
    servers: Servers,
}

impl BanCommand {
    pub const NAME: &'static str = "ban";
    pub const DESCRIPTION: &'static str = "Ban user from room";
    pub const COMPLETION: &'static str = "%(matrix-users) %-";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new(Self::NAME)
            .description(Self::DESCRIPTION)
            .add_argument("<user> [reason]")
            .arguments_description(
                "user: Matrix ID to ban\nreason: Reason for the ban [optional]",
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

        let reason = match args.try_get_many("name") {
            Ok(Some(v)) => Some(v.cloned().collect::<Vec<String>>().join(" ")),
            _ => {
                if let Ok(Some(v)) = args.try_get_one::<String>("name") {
                    Some(v.to_owned())
                } else {
                    None
                }
            }
        };

        let user_id = UserId::parse(user).expect("Couldn't parse UserId");

        if let Some(room) = self.servers.find_room(buffer) {
            let room = room.room().clone();
            let ban = || async move {
                room.ban_user(&user_id, reason.as_deref()).await
            };

            if let Err(err) = self.servers.runtime().block_on(ban()) {
                Weechat::print(&format!("Failed to ban user: {err}"));
            }
        }
    }
}

impl CommandCallback for BanCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = ArgParse::new(Self::NAME)
            .about(Self::DESCRIPTION)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .disable_help_subcommand(true)
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
