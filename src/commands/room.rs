use clap::{Arg, ArgMatches, Command as ArgParse};
use matrix_sdk::ruma::OwnedMxcUri;
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use crate::{room::RoomHandle, Servers};

use super::parse_and_run;

pub struct RoomCommand {
    servers: Servers,
}

impl RoomCommand {
    const COMMAND: &str = "room";
    const DESCRIPTION: &'static str = "Manipulate rooms";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new(Self::COMMAND)
            .description(Self::DESCRIPTION)
            .add_argument("set <option> <value>")
            .add_argument("create")
            .add_argument("leave")
            .add_completion("set |name|alias|topic|visibility")
            .add_completion("create")
            .add_completion("leave");

        Command::new(
            settings,
            RoomCommand {
                servers: servers.clone(),
            },
        )
    }

    fn set(&self, room: RoomHandle, args: &ArgMatches) {
        let Some(val) = args.get_one::<String>("value") else {
            return;
        };
        match args.get_one::<String>("option").map(|s| s.as_str()) {
            Some("name") => {
                let room = room.room().clone();
                let val = val.to_string();
                let set_name = || async move {
                    let _ = room.set_name(val).await;
                };
                Weechat::spawn(set_name()).detach();
            }
            Some("alias") => {}
            Some("topic") => {
                let room = room.room().clone();
                let val = val.to_owned();
                let set_name = || async move {
                    let _ = room.set_room_topic(&val).await;
                };
                Weechat::spawn(set_name()).detach();
            }
            Some("direct") => {
                let room = room.room().clone();
                let Ok(val) = val.parse::<bool>() else { return };
                let set_name = || async move {
                    let _ = room.set_is_direct(val).await;
                };
                Weechat::spawn(set_name()).detach();
            }

            Some("avatar") => {
                let room = room.room().clone();
                let val = val.to_owned();
                let url = OwnedMxcUri::from(val);
                let set_name = || async move {
                    let _ = room.set_avatar_url(&url, None).await;
                };
                Weechat::spawn(set_name()).detach();
            }
            // Some("info") => {
            //     let room = room.room().clone();
            //     let val = val.to_owned();
            //     let url = OwnedMxcUri::from(val);
            //     let set_name = || async move {
            //         // let info = room.;
            //         // let _ = room.set_avatar_url(&url, None).await;
            //     };
            //     Weechat::spawn(set_name()).detach();
            // }
            _ => {}
        }
    }

    pub fn run(&self, buffer: &Buffer, args: &ArgMatches) {
        if let Some(room) = self.servers.find_room(buffer) {
            if let Some((cmd, args)) = args.subcommand() {
                match cmd {
                    "set" => self.set(room, args),
                    "leave" => {}
                    "create" => {}
                    _ => {}
                }
            }
            // let room = room.room().clone();
            // if let Some(account_id) = args.value_of("account") {
            //     let Ok(account_id) = UserId::parse(account_id) else {
            //         Weechat::print("Invalid Matrix UserId");
            //         return;
            //     };

            //     let invite =
            //         || async move { room.invite_user_by_id(&account_id).await };

            //     Weechat::spawn(invite()).detach();
            // }
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }
}

impl CommandCallback for RoomCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let set_subcommand = ArgParse::new("set")
            .about("Modify room settings")
            .arg(Arg::new("option").value_name("option").required(true))
            .arg(Arg::new("value").value_name("value").required(true));

        let argparse = ArgParse::new(Self::COMMAND)
            .about(Self::DESCRIPTION)
            .disable_help_flag(true)
            .disable_help_subcommand(true)
            .disable_version_flag(true)
            .subcommand(set_subcommand);

        parse_and_run(argparse, arguments, |matches| self.run(buffer, matches));
    }
}
