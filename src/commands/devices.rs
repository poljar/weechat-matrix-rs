use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg, SubCommand};

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use crate::Servers;

pub struct DevicesCommand {
    servers: Servers,
}

impl DevicesCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("devices")
            .description("List, delete or rename Matrix devices.")
            .add_argument("list")
            .add_argument("delete <device-id>")
            .add_argument("set-name <device-id> <name>")
            .arguments_description(
                "device-id: The unique id of the device that should be deleted.
                name:
                ",
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

    fn list(&self, buffer: &Buffer) {
        let server = self.servers.find_server(buffer);

        if let Some(s) = server {
            let devices = || async move {
                s.devices().await;
            };
            Weechat::spawn(devices()).detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }
}

impl CommandCallback for DevicesCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("devices")
            .global_setting(ArgParseSettings::DisableHelpFlags)
            .global_setting(ArgParseSettings::DisableVersion)
            .global_setting(ArgParseSettings::VersionlessSubcommands)
            .setting(ArgParseSettings::SubcommandRequiredElseHelp)
            .subcommand(
                SubCommand::with_name("list")
                    .about("List your own Matrix devices on the server."),
            )
            .subcommand(
                SubCommand::with_name("delete")
                    .about("Delete the given device")
                    .arg(Arg::with_name("device-id").required(true)),
            )
            .subcommand(
                SubCommand::with_name("set-name")
                    .about("Set the human readable name of the given device")
                    .arg(Arg::with_name("device-id").required(true))
                    .arg(Arg::with_name("name").required(true)),
            );

        let matches = match argparse.get_matches_from_safe(arguments) {
            Ok(m) => m,
            Err(e) => {
                Weechat::print(
                    &Weechat::execute_modifier(
                        "color_decode_ansi",
                        "1",
                        &e.to_string(),
                    )
                    .unwrap(),
                );
                return;
            }
        };

        match matches.subcommand() {
            ("list", _) => self.list(buffer),
            _ => Weechat::print(&format!(
                "{}Subcommand isn't implemented",
                Weechat::prefix("error")
            )),
        }
    }
}
