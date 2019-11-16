use clap::App as Argparse;
use clap::AppSettings as ArgParseSettings;
use clap::SubCommand;

use crate::plugin;
use weechat::Weechat;
use weechat::{ArgsWeechat, Buffer, CommandDescription, CommandHook};

pub struct Commands {
    matrix: CommandHook<()>,
}

impl Commands {
    pub fn hook_all(weechat: &Weechat) -> Commands {
        let matrix_desc = CommandDescription {
            name: "matrix",
            ..Default::default()
        };

        let matrix = weechat.hook_command(
            matrix_desc,
            Commands::matrix_command_cb,
            None,
        );

        Commands { matrix }
    }

    fn matrix_command_cb(_data: &(), buffer: Buffer, args: ArgsWeechat) {
        let weechat = unsafe { Weechat::weechat() };
        let argparse = Argparse::new("matrix")
            .setting(ArgParseSettings::ColorNever)
            .subcommand(SubCommand::with_name("connect"))
            .subcommand(SubCommand::with_name("disconnect"));

        let matches = match argparse.get_matches_from_safe(args) {
            Ok(m) => m,
            Err(e) => {
                weechat
                    .print(&format!("Error parsing command arguments {}", e));
                return;
            }
        };
        let mut plugin = plugin();

        if let Some(matches) = matches.subcommand_matches("connect") {
            weechat.print("Connecting");
            for server in plugin.servers.values_mut() {
                server.connect();
            }
        } else if let Some(matches) = matches.subcommand_matches("disconnect") {
            weechat.print("Disconnecting");
            for server in plugin.servers.values_mut() {
                server.disconnect();
            }
        } else {
            weechat.print("Unknown subcommand");
        }
    }
}
