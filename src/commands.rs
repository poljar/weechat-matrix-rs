use clap::App as Argparse;
use clap::AppSettings as ArgParseSettings;
use clap::{Arg, ArgMatches, SubCommand};
use url::Url;

use crate::config::{Config, ConfigHandle};
use crate::PLUGIN_NAME;
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
            description: "Matrix chat protocol command.",
            args: "server add <server-name> <hostname>[:<port>]||\
                 server delete|list|listfull <server-name> ||\
                 connect <server-name> ||\
                 disconnect <server-name> ||\
                 reconnect <server-name> ||\
                 help <matrix-command> [<matrix-subcommand>]",
            args_description:
                "     server: List, add, or remove Matrix servers.
    connect: Connect to Matrix servers.
 disconnect: Disconnect from one or all Matrix servers.
  reconnect: Reconnect to server(s).
       help: Show detailed command help.\n
Use /matrix [command] help to find out more.\n",
            completion: "server |add|delete|list|listfull ||
                 connect ||
                 disconnect ||
                 reconnect ||
                 help server|connect|disconnect|reconnect",
        };

        let matrix = weechat.hook_command(
            matrix_desc,
            Commands::matrix_command_cb,
            Some((servers.clone_weak(), config.clone_weak())),
        );

        Commands { _matrix: matrix }
    }

    fn add_server(args: &ArgMatches, servers: &Servers, config: &ConfigHandle) {
        let weechat = unsafe { Weechat::weechat() };

        let server_name = args
            .value_of("name")
            .expect("Server name not set but was required");
        let homeserver = args
            .value_of("homeserver")
            .expect("Homeserver not set but was required");
        let homeserver = Url::parse(homeserver)
            .expect("Can't parse Homeserver even if validation passed");

        let config = config.upgrade();
        let mut config_borrow = config.borrow_mut();
        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");

        let server = MatrixServer::new(server_name, &config, &mut section);

        let mut servers = servers.borrow_mut();
        servers.insert(server_name.to_owned(), server);

        let homeserver_option = section
            .search_option(&format!("{}.homeserver", server_name))
            .expect("Homeserver option wasn't created");
        homeserver_option.set(homeserver.as_str(), true);

        weechat.print(&format!(
            "{}: Server {}{}{} has been added.",
            PLUGIN_NAME,
            weechat.color("chat_server"),
            server_name,
            weechat.color("reset")
        ));
    }

    fn delete_server(args: &ArgMatches, servers: &Servers) {
        let weechat = unsafe { Weechat::weechat() };

        let server_name = args
            .value_of("name")
            .expect("Server name not set but was required");

        let mut servers = servers.borrow_mut();

        let connected = {
            let server = servers.get(server_name);

            if let Some(s) = server {
                s.connected()
            } else {
                weechat.print(&format!(
                    "{}: No such server {}{}{} found.",
                    PLUGIN_NAME,
                    weechat.color("chat_server"),
                    server_name,
                    weechat.color("reset")
                ));
                return;
            }
        };

        if connected {
            weechat.print(&format!(
                "{}: Server {}{}{} is still connected.",
                PLUGIN_NAME,
                weechat.color("chat_server"),
                server_name,
                weechat.color("reset")
            ));
            return;
        }

        let server = servers.remove(server_name).unwrap();

        drop(server);

        weechat.print(&format!(
            "{}: Server {}{}{} has been deleted.",
            PLUGIN_NAME,
            weechat.color("chat_server"),
            server_name,
            weechat.color("reset")
        ));
    }

    fn list_servers(servers: &Servers) {
        let weechat = unsafe { Weechat::weechat() };

        if servers.borrow().is_empty() {
            return;
        }

        weechat.print("\nAll Matrix servers:");

        // TODO print out some stats if the server is connected.
        for server in servers.borrow().keys() {
            weechat.print(&format!(
                "    {}{}",
                weechat.color("chat_server"),
                server
            ));
        }
    }

    fn server_command(
        args: &ArgMatches,
        servers: &Servers,
        config: &ConfigHandle,
    ) {
        match args.subcommand() {
            ("add", Some(subargs)) => {
                Commands::add_server(subargs, servers, config)
            }
            ("delete", Some(subargs)) => {
                Commands::delete_server(subargs, servers)
            }
            ("list", _) => Commands::list_servers(servers),
            _ => Commands::list_servers(servers),
        }
    }

    fn matrix_command_cb(
        data: &(ServersHandle, ConfigHandle),
        buffer: Buffer,
        args: ArgsWeechat,
    ) {
        let weechat = unsafe { Weechat::weechat() };
        let server_command = SubCommand::with_name("server")
            .about("List, add or delete Matrix servers.")
            .subcommand(
                SubCommand::with_name("add")
                    .about("Add a new Matrix server.")
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
            .subcommand(
                SubCommand::with_name("delete")
                    .about("Delete an existing Matrix server.")
                    .arg(
                        Arg::with_name("name")
                            .value_name("server-name")
                            .required(true),
                    ),
            );

        let argparse = Argparse::new("matrix")
            .about("Matrix chat protocol command.")
            .global_setting(ArgParseSettings::ColorNever)
            .global_setting(ArgParseSettings::DisableHelpFlags)
            .global_setting(ArgParseSettings::DisableVersion)
            .global_setting(ArgParseSettings::VersionlessSubcommands)
            .setting(ArgParseSettings::SubcommandRequiredElseHelp)
            .subcommand(server_command)
            .subcommand(
                SubCommand::with_name("connect")
                    .about("Connect to Matrix servers.")
                    .arg(
                        Arg::with_name("name")
                            .value_name("server-name")
                            .required(true)
                            .multiple(true),
                    ),
            )
            .subcommand(
                SubCommand::with_name("disconnect")
                    .about("Disconnect from one or all Matrix servers")
                    .arg(
                        Arg::with_name("name")
                            .value_name("server-name")
                            .required(true),
                    ),
            );

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
                let mut servers = servers.borrow_mut();

                let server_name = subargs
                    .value_of("name")
                    .expect("Server name not set but was required");

                let server = servers.get_mut(server_name);

                if let Some(s) = server {
                    s.disconnect();
                } else {
                    weechat.print(&format!(
                        "{}{}: Server \"{}{}{}\" not found.",
                        weechat.prefix("error"),
                        PLUGIN_NAME,
                        weechat.color("chat_server"),
                        server_name,
                        weechat.color("reset")
                    ));
                }
            }
            ("server", Some(subargs)) => {
                Commands::server_command(subargs, &servers, config);
            }
            _ => unreachable!(),
        }
    }
}
