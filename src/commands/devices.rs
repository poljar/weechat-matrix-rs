use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use matrix_sdk::ruma::identifiers::DeviceIdBox;

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::Servers;

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
            .add_completion("list")
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

    fn delete(servers: &Servers, buffer: &Buffer, devices: Vec<DeviceIdBox>) {
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

    fn list(servers: &Servers, buffer: &Buffer) {
        let server = servers.find_server(buffer);

        if let Some(s) = server {
            let devices = || async move {
                s.devices().await;
            };
            Weechat::spawn(devices()).detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        match args.subcommand() {
            ("list", _) => Self::list(servers, buffer),
            ("delete", args) => {
                let devices = args
                    .and_then(|a| a.args.get("device-id"))
                    .expect("Args didn't contain any device ids");
                let devices: Vec<DeviceIdBox> = devices
                    .vals
                    .iter()
                    .map(|d| d.clone().to_string_lossy().as_ref().into())
                    .collect();
                Self::delete(servers, buffer, devices);
            }
            _ => Weechat::print(&format!(
                "{}Subcommand isn't implemented",
                Weechat::prefix(Prefix::Error)
            )),
        }
    }

    pub fn subcommands() -> Vec<Argparse<'static, 'static>> {
        vec![
            SubCommand::with_name("list")
                .about("List your own Matrix devices on the server."),
            SubCommand::with_name("delete")
                .about("Delete the given device")
                .arg(
                    Arg::with_name("device-id")
                        .require_delimiter(true)
                        .multiple(true)
                        .required(true),
                ),
            SubCommand::with_name("set-name")
                .about("Set the human readable name of the given device")
                .arg(Arg::with_name("device-id").required(true))
                .arg(Arg::with_name("name").required(true)),
        ]
    }
}

impl CommandCallback for DevicesCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("devices")
            .about(Self::DESCRIPTION)
            .global_setting(ArgParseSettings::DisableHelpFlags)
            .global_setting(ArgParseSettings::DisableVersion)
            .global_setting(ArgParseSettings::VersionlessSubcommands)
            .setting(ArgParseSettings::SubcommandRequiredElseHelp)
            .subcommands(Self::subcommands());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, &matches)
        });
    }
}
