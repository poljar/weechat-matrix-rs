use clap::App as Argparse;
use clap::AppSettings as ArgParseSettings;
use clap::{Arg, ArgMatches, SubCommand};
use url::Url;

use crate::config::{Config, ConfigHandle};
use crate::{MatrixServer, Servers, ServersHandle};
use weechat::Weechat;
use weechat::{ArgsWeechat, Buffer, CommandDescription, CommandHook};

pub struct Commands {
    _matrix: CommandHook<(ServersHandle, ConfigHandle)>,
}

impl Commands {
    pub fn hook_all(
        weechat: &Weechat,
        servers: &Servers,
        config: &Config,
    ) -> Commands {
        let matrix_desc = CommandDescription {
            name: "matrix",
            description: "Matrix chat protocol command",
            args: "server add <server-name> <hostname>[:<port>] ||\
                 server delete|list|listfull <server-name> ||\
                 connect <server-name> ||\
                 disconnect <server-name> ||\
                 reconnect <server-name> ||\
                 help <matrix-command>",
            args_description: "    server: list, add, or remove Matrix servers
    connect: connect to Matrix servers
 disconnect: disconnect from one or all Matrix servers
  reconnect: reconnect to server(s)
       help: show detailed command help\n
Use /matrix help [command] to find out more.\n",
            completion: "server |add|delete|list|listfull ||
                 connect ||
                 disconnect ||
                 reconnect ||
                 help",
        };

        let matrix = weechat.hook_command(
            matrix_desc,
            Commands::matrix_command_cb,
            Some((servers.clone_weak(), config.clone_weak())),
        );

        Commands { _matrix: matrix }
    }

    fn server_command(
        buffer: &Buffer,
        args: &ArgMatches,
        servers: &Servers,
        config: &ConfigHandle,
    ) {
        match args.subcommand() {
            ("add", Some(subargs)) => {
                let server_name = subargs
                    .value_of("name")
                    .expect("Server name not set but was required");
                let homeserver = subargs
                    .value_of("homeserver")
                    .expect("Homeserver not set but was required");
                let homeserver = Url::parse(homeserver)
                    .expect("Can't parse Homeserver even if validation passed");

                let config = config.upgrade();
                let mut config_borrow = config.borrow_mut();
                let mut section = config_borrow
                    .search_section_mut("server")
                    .expect("Can't get server section");

                let server =
                    MatrixServer::new(server_name, &config, &mut section);

                let mut servers = servers.borrow_mut();
                servers.insert(server_name.to_owned(), server);

                let homeserver_option = section
                    .search_option(&format!("{}.homeserver", server_name))
                    .expect("Homeserver option wasn't created");
                homeserver_option.set(homeserver.as_str(), true);
            }
            ("delete", Some(subargs)) => {
                buffer.print("Deleting server");
            }
            _ => (),
        }
    }

    fn matrix_command_cb(
        data: &(ServersHandle, ConfigHandle),
        buffer: Buffer,
        args: ArgsWeechat,
    ) {
        let weechat = unsafe { Weechat::weechat() };
        let server_command = SubCommand::with_name("server")
            .subcommand(
                SubCommand::with_name("add")
                    .arg(
                        Arg::with_name("name")
                            .value_name("server-name")
                            .required(true),
                    )
                    .arg(
                        Arg::with_name("homeserver")
                            .required(true)
                            .validator(MatrixServer::parse_homeserver_url),
                    ),
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
        let (servers, config) = data;
        let servers_ref = servers.upgrade();
        let servers = servers_ref;

        match matches.subcommand() {
            ("connect", Some(subargs)) => {
                weechat.print("Connecting");
                let mut servers = servers.borrow_mut();
                for server in servers.values_mut() {
                    match server.connect() {
                        Ok(_) => (),
                        Err(e) => weechat.print(&format!("{:?}", e)),
                    }
                }
            }
            ("disconnect", Some(subargs)) => {
                weechat.print("Disconnecting");
                let mut servers = servers.borrow_mut();
                for server in servers.values_mut() {
                    server.disconnect();
                }
            }
            ("server", Some(subargs)) => {
                Commands::server_command(&buffer, subargs, &servers, config);
            }
            _ => unreachable!(),
        }
    }
}
