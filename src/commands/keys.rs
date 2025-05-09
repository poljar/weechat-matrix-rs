use std::path::PathBuf;

use clap::{Arg, ArgMatches, Command as ArgParse};

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use super::parse_and_run;
use crate::{MatrixServer, Servers};

pub struct KeysCommand {
    servers: Servers,
}

impl KeysCommand {
    pub const DESCRIPTION: &'static str = "Import or export E2EE keys.";
    pub const COMPLETION: &'static str = "import|export %(filename)";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("keys")
            .description(Self::DESCRIPTION)
            .add_argument("import <file> <passphrase>")
            .add_argument("export <file> <passphrase>")
            .arguments_description(
                "file: Path to a file that is or will contain the E2EE keys export",
            )
            .add_completion(Self::COMPLETION)
            .add_completion("help import|export");

        Command::new(
            settings,
            Self {
                servers: servers.clone(),
            },
        )
    }

    fn upcast_args(args: &ArgMatches) -> (PathBuf, String) {
        let passphrase = args
            .get_one::<String>("passphrase")
            .expect("No passphrase found");

        let file = args.get_one::<String>("file").expect("No file found");
        let file = Weechat::expand_home(file);
        let file = PathBuf::from(file);
        (file, passphrase.to_owned())
    }

    fn import(server: MatrixServer, file: PathBuf, passphrase: String) {
        let import = || async move {
            server.import_keys(file, passphrase).await;
        };
        Weechat::spawn(import()).detach();
    }

    fn export(server: MatrixServer, file: PathBuf, passphrase: String) {
        let export = || async move {
            server.export_keys(file, passphrase).await;
        };
        Weechat::spawn(export()).detach();
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        if let Some(server) = servers.find_server(buffer) {
            match args.subcommand() {
                Some(("import", args)) => {
                    let (file, passphrase) = Self::upcast_args(args);
                    Self::import(server, file, passphrase);
                }
                Some(("export", args)) => {
                    let (file, passphrase) = Self::upcast_args(args);
                    Self::export(server, file, passphrase);
                }
                _ => unreachable!(),
            }
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn subcommands() -> Vec<ArgParse> {
        vec![
            ArgParse::new("import")
                .about("Import the E2EE keys from the given file.")
                .arg(Arg::new("file").required(true))
                .arg(Arg::new("passphrase").required(true)),
            ArgParse::new("export")
                // TODO add the ability to export keys only for a given room.
                .about("Export your E2EE keys to the given file.")
                .arg(Arg::new("file").required(true))
                .arg(Arg::new("passphrase").required(true)),
        ]
    }
}

impl CommandCallback for KeysCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = ArgParse::new("keys")
            .about(Self::DESCRIPTION)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .disable_help_subcommand(true)
            .subcommand_required(true)
            .subcommands(Self::subcommands());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, matches)
        });
    }
}
