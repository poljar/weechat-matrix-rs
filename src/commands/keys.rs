use std::path::PathBuf;

use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};

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
    pub const SETTINGS: &'static [ArgParseSettings] = &[
        ArgParseSettings::DisableHelpFlags,
        ArgParseSettings::DisableVersion,
        ArgParseSettings::VersionlessSubcommands,
        ArgParseSettings::SubcommandRequiredElseHelp,
    ];

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
            .args
            .get("passphrase")
            .map(|p| p.vals.get(0).map(|p| p.clone().into_string().ok()))
            .flatten()
            .flatten()
            .expect("No passphrase found");

        let file = args
            .args
            .get("file")
            .map(|f| f.vals.get(0))
            .flatten()
            .expect("No file found");
        let file = Weechat::expand_home(&file.to_string_lossy());
        let file = PathBuf::from(file);
        (file, passphrase)
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
                ("import", args) => {
                    let (file, passphrase) = Self::upcast_args(
                        args.expect("No args were provided to the subcommand"),
                    );
                    Self::import(server, file, passphrase);
                }
                ("export", args) => {
                    let (file, passphrase) = Self::upcast_args(
                        args.expect("No args were provided to the subcommand"),
                    );
                    Self::export(server, file, passphrase);
                }
                _ => unreachable!(),
            }
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn subcommands() -> Vec<Argparse<'static, 'static>> {
        vec![
            SubCommand::with_name("import")
                .about("Import the E2EE keys from the given file.")
                .arg(Arg::with_name("file").required(true))
                .arg(Arg::with_name("passphrase").required(true)),
            SubCommand::with_name("export")
                // TODO add the ability to export keys only for a given room.
                .about("Export your E2EE keys to the given file.")
                .arg(Arg::with_name("file").required(true))
                .arg(Arg::with_name("passphrase").required(true)),
        ]
    }
}

impl CommandCallback for KeysCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("keys")
            .about(Self::DESCRIPTION)
            .settings(Self::SETTINGS)
            .subcommands(Self::subcommands());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, matches)
        });
    }
}
