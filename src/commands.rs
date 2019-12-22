use clap::App as Argparse;
use clap::AppSettings as ArgParseSettings;
use clap::SubCommand;

use crate::{Servers, ServersHandle};
use weechat::Weechat;
use weechat::{ArgsWeechat, Buffer, CommandDescription, CommandHook};

pub struct Commands {
    _matrix: CommandHook<ServersHandle>,
}

impl Commands {
    pub fn hook_all(weechat: &Weechat, servers: &Servers) -> Commands {
        let matrix_desc = CommandDescription {
            name: "matrix",
            ..Default::default()
        };

        let matrix = weechat.hook_command(
            matrix_desc,
            Commands::matrix_command_cb,
            Some(servers.clone_weak()),
        );

        Commands { _matrix: matrix }
    }

    fn matrix_command_cb(
        servers: &ServersHandle,
        buffer: Buffer,
        args: ArgsWeechat,
    ) {
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
        if let Some(matches) = matches.subcommand_matches("connect") {
            let servers = servers.upgrade();
            for server in servers.borrow().values() {
                weechat.print(&format!("Connecting {}", server.name()));
                server.connect();
            }
        } else if let Some(matches) = matches.subcommand_matches("disconnect") {
            let servers = servers.upgrade();
            weechat.print("Disconnecting");
            for server in servers.borrow().values() {
                server.disconnect();
            }
        } else {
            weechat.print("Unknown subcommand");
        }
    }
}
