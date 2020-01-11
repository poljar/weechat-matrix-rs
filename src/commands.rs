use std::collections::HashMap;

use clap::App as Argparse;
use clap::AppSettings as ArgParseSettings;
use clap::{SubCommand, Arg, ArgMatches};

use crate::plugin;
use crate::MatrixServer;
use weechat::Weechat;
use weechat::{ArgsWeechat, Buffer, CommandDescription, CommandHook};

pub struct Commands {
    matrix: CommandHook<()>,
}

impl Commands {
    pub fn hook_all(weechat: &Weechat) -> Commands {
        let matrix_desc = CommandDescription {
            name: "matrix",
            description: "Matrix chat protocol command",
            args:
                "server add <server-name> <hostname>[:<port>] ||\
                 server delete|list|listfull <server-name> ||\
                 connect <server-name> ||\
                 disconnect <server-name> ||\
                 reconnect <server-name> ||\
                 help <matrix-command>",
            args_description:
"    server: list, add, or remove Matrix servers
    connect: connect to Matrix servers
 disconnect: disconnect from one or all Matrix servers
  reconnect: reconnect to server(s)
       help: show detailed command help\n
Use /matrix help [command] to find out more.\n",
            completion:
                "server |add|delete|list|listfull ||
                 connect ||
                 disconnect ||
                 reconnect ||
                 help",
        };

        let matrix = weechat.hook_command(
            matrix_desc,
            Commands::matrix_command_cb,
            None,
        );

        Commands { matrix }
    }

    fn server_command(buffer: &Buffer, args: &ArgMatches, server: &mut HashMap<String, MatrixServer>) {
        match args.subcommand() {
            ("add", Some(subargs)) => {
                buffer.print("Adding server");
            }
            ("delete", Some(subargs)) => {
                buffer.print("Deleting server");
            }
            _ => (),
        }
    }

    fn matrix_command_cb(_data: &(), buffer: Buffer, args: ArgsWeechat) {
        let weechat = unsafe { Weechat::weechat() };
        let server_command = SubCommand::with_name("server")
            .subcommand(SubCommand::with_name("add")
                .arg(Arg::with_name("name").value_name("server-name")
                     .required(true))
                .arg(Arg::with_name("homeserver").value_name("homeserver-address")
                     .required(true))
            )
            .subcommand(SubCommand::with_name("delete"));

        let argparse = Argparse::new("matrix")
            .global_setting(ArgParseSettings::ColorNever)
            .global_setting(ArgParseSettings::DisableHelpFlags)
            .global_setting(ArgParseSettings::DisableVersion)
            .global_setting(ArgParseSettings::VersionlessSubcommands)
            .setting(ArgParseSettings::SubcommandRequiredElseHelp)
            .subcommand(server_command)
            .subcommand(SubCommand::with_name("connect"))
            .subcommand(SubCommand::with_name("disconnect"));

        let matches = match argparse.get_matches_from_safe(args) {
            Ok(m) => m,
            Err(e) => {
                weechat.print(&e.to_string());
                return;
            }
        };
        let plugin = plugin();
        let mut servers = &mut plugin.servers;

        match matches.subcommand() {
            ("connect", Some(subargs)) => {
                weechat.print("Connecting");
                for server in servers.values_mut() {
                    server.connect();
                }
            },
            ("disconnect", Some(subargs)) => {
                weechat.print("Disconnecting");
                for server in servers.values_mut() {
                    server.disconnect();
                }
            },
            ("server", Some(subargs)) => {
                Commands::server_command(&buffer, subargs, &mut servers);
            },
            _ => unreachable!(),
        }
    }
}
