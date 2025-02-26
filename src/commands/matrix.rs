use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use url::Url;

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::{parse_and_run, verification::VerificationCommand};
use crate::{
    commands::{DevicesCommand, KeysCommand},
    config::ConfigHandle,
    MatrixServer, Servers, PLUGIN_NAME,
};

pub struct MatrixCommand {
    servers: Servers,
    config: ConfigHandle,
}

impl MatrixCommand {
    pub fn create(
        servers: &Servers,
        config: &ConfigHandle,
    ) -> Result<Command, ()> {
        let matrix_settings = CommandSettings::new("matrix")
            .description("Matrix chat protocol command.")
            .add_argument("server add <server-name> <hostname>[:<port>]")
            .add_argument("server delete|list|listfull <server-name>")
            .add_argument("connect <server-name>")
            .add_argument("devices delete|list|set-name")
            .add_argument("keys import|export <file> <passphrase>")
            .add_argument("disconnect <server-name>")
            .add_argument("reconnect <server-name>")
            .add_argument("help <matrix-command> [<matrix-subcommand>]")
            .arguments_description(&format!(
                "      server: List, add, or remove Matrix servers.
     connect: Connect to Matrix servers.
  disconnect: Disconnect from one or all Matrix servers.
   reconnect: Reconnect to server(s).
     devices: {}
        keys: {}
verification: {}
        help: Show detailed command help.\n
Use /matrix [command] help to find out more.\n",
                DevicesCommand::DESCRIPTION,
                KeysCommand::DESCRIPTION,
                VerificationCommand::DESCRIPTION,
            ))
            .add_completion("server add|delete|list|listfull")
            .add_completion("devices list|delete|set-name %(matrix-users)")
            .add_completion(&format!("keys {}", KeysCommand::COMPLETION))
            .add_completion(&format!(
                "verification {}",
                VerificationCommand::COMPLETION
            ))
            .add_completion("connect %(matrix_servers)")
            .add_completion("disconnect %(matrix_servers)")
            .add_completion("reconnect %(matrix_servers)")
            .add_completion(
                "help server|connect|disconnect|reconnect|keys|devices",
            );

        Command::new(
            matrix_settings,
            MatrixCommand {
                servers: servers.clone(),
                config: config.clone(),
            },
        )
    }

    fn add_server(&self, args: &ArgMatches) {
        let server_name = args
            .value_of("name")
            .expect("Server name not set but was required");
        let homeserver = args
            .value_of("homeserver")
            .expect("Homeserver not set but was required");
        let homeserver = Url::parse(homeserver)
            .expect("Can't parse Homeserver even if validation passed");

        let mut config_borrow = self.config.borrow_mut();
        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");

        let server = MatrixServer::new(
            server_name,
            &self.config,
            &mut section,
            self.servers.clone(),
        );

        self.servers.insert(server);

        let homeserver_option = section
            .search_option(&format!("{}.homeserver", server_name))
            .expect("Homeserver option wasn't created");
        homeserver_option.set(homeserver.as_str(), true);

        Weechat::print(&format!(
            "{}: Server {}{}{} has been added.",
            PLUGIN_NAME,
            Weechat::color("chat_server"),
            server_name,
            Weechat::color("reset")
        ));
    }

    fn delete_server(&self, args: &ArgMatches) {
        let server_name = args
            .value_of("name")
            .expect("Server name not set but was required");

        let connected = {
            if let Some(s) = self.servers.get(server_name) {
                s.connected()
            } else {
                Weechat::print(&format!(
                    "{}: No such server {}{}{} found.",
                    PLUGIN_NAME,
                    Weechat::color("chat_server"),
                    server_name,
                    Weechat::color("reset")
                ));
                return;
            }
        };

        if connected {
            Weechat::print(&format!(
                "{}: Server {}{}{} is still connected.",
                PLUGIN_NAME,
                Weechat::color("chat_server"),
                server_name,
                Weechat::color("reset")
            ));
            return;
        }

        let server = self.servers.remove(server_name).unwrap();

        drop(server);

        Weechat::print(&format!(
            "{}: Server {}{}{} has been deleted.",
            PLUGIN_NAME,
            Weechat::color("chat_server"),
            server_name,
            Weechat::color("reset")
        ));
    }

    fn list_servers(&self, details: bool) {
        if self.servers.borrow().is_empty() {
            return;
        }

        Weechat::print("\nAll Matrix servers:");

        // TODO print out some stats if the server is connected.
        for server in self.servers.borrow().values() {
            Weechat::print(&format!("    {}", server.get_info_str(details)));
        }
    }

    fn server_command(&self, args: &ArgMatches) {
        match args.subcommand() {
            ("add", Some(subargs)) => self.add_server(subargs),
            ("delete", Some(subargs)) => self.delete_server(subargs),
            ("list", _) => self.list_servers(false),
            ("listfull", _) => self.list_servers(true),
            _ => self.list_servers(false),
        }
    }

    fn server_not_found(&self, server_name: &str) {
        Weechat::print(&format!(
            "{}{}: Server \"{}{}{}\" not found.",
            Weechat::prefix(Prefix::Error),
            PLUGIN_NAME,
            Weechat::color("chat_server"),
            server_name,
            Weechat::color("reset")
        ));
    }

    fn connect_command(&self, args: &ArgMatches) {
        let server_names = args
            .values_of("name")
            .expect("Server names not set but were required");

        for server_name in server_names {
            if let Some(s) = self.servers.get(server_name) {
                match s.connect() {
                    Ok(_) => (),
                    Err(e) => Weechat::print(&format!("{:?}", e)),
                }
            } else {
                self.server_not_found(server_name)
            }
        }
    }

    fn disconnect_command(&self, args: &ArgMatches) {
        let server_name = args
            .value_of("name")
            .expect("Server name not set but was required");

        if let Some(s) = self.servers.get(server_name) {
            s.disconnect();
        } else {
            self.server_not_found(server_name)
        }
    }

    fn run(&self, buffer: &Buffer, args: &ArgMatches) {
        match args.subcommand() {
            ("connect", Some(subargs)) => self.connect_command(subargs),
            ("disconnect", Some(subargs)) => self.disconnect_command(subargs),
            ("server", Some(subargs)) => self.server_command(subargs),
            ("devices", Some(subargs)) => {
                DevicesCommand::run(buffer, &self.servers, subargs)
            }
            ("keys", Some(subargs)) => {
                KeysCommand::run(buffer, &self.servers, subargs)
            }
            ("verification", Some(subargs)) => {
                VerificationCommand::run(buffer, &self.servers, subargs)
            }
            _ => unreachable!(),
        }
    }
}

impl CommandCallback for MatrixCommand {
    fn callback(
        &mut self,
        _weechat: &Weechat,
        buffer: &Buffer,
        arguments: Args,
    ) {
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
                            .validator(MatrixServer::parse_url),
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
            )
            .subcommand(
                SubCommand::with_name("list")
                    .about("List the configured Matrix servers."),
            )
            .subcommand(
                SubCommand::with_name("listfull")
                    .about("List detailed information about the configured Matrix servers."),
            );

        let argparse = Argparse::new("matrix")
            .about("Matrix chat protocol command.")
            .global_settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
                ArgParseSettings::VersionlessSubcommands,
            ])
            .setting(ArgParseSettings::SubcommandRequiredElseHelp)
            .subcommand(server_command)
            .subcommand(
                SubCommand::with_name("devices")
                    .about(DevicesCommand::DESCRIPTION)
                    .settings(DevicesCommand::SETTINGS)
                    .subcommands(DevicesCommand::subcommands()),
            )
            .subcommand(
                SubCommand::with_name("keys")
                    .about(KeysCommand::DESCRIPTION)
                    .settings(KeysCommand::SETTINGS)
                    .subcommands(KeysCommand::subcommands()),
            )
            .subcommand(
                SubCommand::with_name("verification")
                    .about(VerificationCommand::DESCRIPTION)
                    .subcommands(VerificationCommand::subcommands()),
            )
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

        parse_and_run(argparse, arguments, |args| self.run(buffer, args));
    }
}
