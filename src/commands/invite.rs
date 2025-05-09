use clap::{Arg, ArgMatches, Command as ArgParse};
use matrix_sdk::ruma::UserId;
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use crate::Servers;

use super::parse_and_run;

pub struct InviteCommand {
    servers: Servers,
}

impl InviteCommand {
    const NAME: &str = "invite";
    const DESCRIPTION: &'static str = "Invite user to a room.";
    const COMPLETION: &'static str = "%(matrix-users) %-";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new(Self::NAME)
            .add_argument("<account>")
            .description(Self::DESCRIPTION)
            .add_completion(Self::COMPLETION);

        Command::new(
            settings,
            InviteCommand {
                servers: servers.clone(),
            },
        )
    }

    pub fn run(&self, buffer: &Buffer, args: &ArgMatches) {
        if let Some(room) = self.servers.find_room(buffer) {
            let room = room.room().clone();
            if let Some(account_id) = args.get_one::<String>("user") {
                let Ok(account_id) = UserId::parse(account_id) else {
                    Weechat::print("Invalid Matrix UserId");
                    return;
                };

                let invite =
                    || async move { room.invite_user_by_id(&account_id).await };

                let ret = self.servers.runtime().block_on(invite());
                if let Err(err) = ret {
                    Weechat::print(&format!("Failed to invite user: {err}"));
                };
            }
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }
}

impl CommandCallback for InviteCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = ArgParse::new(Self::NAME)
            .about(Self::DESCRIPTION)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .disable_help_subcommand(true)
            .arg(Arg::new("user").required(true));

        parse_and_run(argparse, arguments, |matches| self.run(buffer, matches));
    }
}
