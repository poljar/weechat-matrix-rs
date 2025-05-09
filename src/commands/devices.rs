use clap::{Arg, ArgMatches, Command as ArgParse};
use matrix_sdk::ruma::{OwnedDeviceId, OwnedUserId, UserId};

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use crate::Servers;

use super::parse_and_run;

pub struct DevicesCommand {
    servers: Servers,
}

impl DevicesCommand {
    pub const DESCRIPTION: &'static str =
        "List, delete or rename Matrix devices";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("devices")
            .description(Self::DESCRIPTION)
            .add_argument("list")
            .add_argument("delete <device-id>")
            .add_argument("set-name <device-id> <name>")
            .arguments_description(
                "device-id: The unique id of the device that should be deleted.
     name: The name that the device name should be set to.",
            )
            .add_completion("list %(matrix-users)")
            .add_completion("delete %(matrix-own-devices)")
            .add_completion("set-name %(matrix-own-devices)")
            .add_completion("help list|delete|set-name");

        Command::new(
            settings,
            DevicesCommand {
                servers: servers.clone(),
            },
        )
    }

    fn delete(servers: &Servers, buffer: &Buffer, devices: Vec<OwnedDeviceId>) {
        let server = servers.find_server(buffer);

        if let Some(s) = server {
            let devices = || async move {
                s.delete_devices(devices).await;
            };
            Weechat::spawn(devices()).detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    fn list(servers: &Servers, buffer: &Buffer, user_id: Option<OwnedUserId>) {
        let server = servers.find_server(buffer);

        if let Some(s) = server {
            let devices = || async move {
                s.devices(user_id).await;
            };
            Weechat::spawn(devices()).detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        match args.subcommand() {
            Some(("list", args)) => {
                let user_id = args.get_one::<String>("user-id").map(|u| {
                    UserId::parse(u).expect("Argument wasn't a valid user id")
                });

                Self::list(servers, buffer, user_id);
            }
            Some(("delete", args)) => {
                let devices: Vec<&str> = args
                    .get_many("device-id")
                    .expect("Args didn't contain any device ids")
                    .copied()
                    .collect();
                let devices: Vec<OwnedDeviceId> =
                    devices.iter().map(|d| (*d).into()).collect();
                Self::delete(servers, buffer, devices);
            }
            _ => Weechat::print(&format!(
                "{}Subcommand isn't implemented",
                Weechat::prefix(Prefix::Error)
            )),
        }
    }

    pub fn subcommands() -> Vec<ArgParse> {
        fn parse_user_id(u: &str) -> Result<OwnedUserId, String> {
            UserId::parse(u)
                .map_err(|_| "The given user isn't a valid user ID".to_owned())
        }
        vec![
            ArgParse::new("list")
                .arg(
                    Arg::new("user-id")
                        .required(false)
                        .value_parser(parse_user_id),
                )
                .about("List your own Matrix devices on the server."),
            ArgParse::new("delete")
                .about("Delete the given device")
                .arg(Arg::new("device-id").num_args(1..).required(true)),
            ArgParse::new("set-name")
                .about("Set the human readable name of the given device")
                .arg(Arg::new("device-id").required(true))
                .arg(Arg::new("name").required(true)),
        ]
    }
}

impl CommandCallback for DevicesCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = ArgParse::new("devices")
            .about(Self::DESCRIPTION)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .subcommand_required(true)
            .subcommands(Self::subcommands());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, matches)
        });
    }
}
