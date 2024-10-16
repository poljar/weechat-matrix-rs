use clap::{
    App as Argparse, AppSettings as ArgParseSettings, ArgMatches, SubCommand,
};

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use super::parse_and_run;
use crate::{BufferOwner, Servers};

pub struct VerificationCommand {
    servers: Servers,
}

enum CommandType {
    Accept,
    Confirm,
    Cancel,
}

impl VerificationCommand {
    pub const DESCRIPTION: &'static str =
        "Control interactive verification flows";

    pub const COMPLETION: &'static str = "accept|confirm|cancel";

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("verification")
            .description(Self::DESCRIPTION)
            .add_argument("verification accept|confirm|cancel")
            .arguments_description(
                "accept: accept the verification request
                confirm: confirm that the emojis match on both sides or \
                confirm that the other side has scanned our QR code
                cancel: cancel the verification flow or request",
            )
            .add_completion(Self::COMPLETION)
            .add_completion("help accept|confirm|cancel");

        Command::new(
            settings,
            VerificationCommand {
                servers: servers.clone(),
            },
        )
    }

    fn verification(servers: &Servers, buffer: &Buffer, command: CommandType) {
        let buffer_owner = servers.buffer_owner(buffer);

        match buffer_owner {
            BufferOwner::Room(_, b) => match command {
                CommandType::Accept => b.accept_verification(),
                CommandType::Confirm => b.confirm_verification(),
                CommandType::Cancel => b.cancel_verification(),
            },
            BufferOwner::Verification(_, b) => match command {
                CommandType::Accept => b.accept(),
                CommandType::Confirm => b.confirm(),
                CommandType::Cancel => b.cancel(),
            },
            BufferOwner::Server(_) | BufferOwner::None => {
                Weechat::print(
                    "The verification command needs to be executed in a room or \
                    verification buffer",
                );
            }
        }
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        match args.subcommand() {
            ("accept", _) => {
                Self::verification(servers, buffer, CommandType::Accept)
            }
            ("confirm", _) => {
                Self::verification(servers, buffer, CommandType::Confirm)
            }
            ("cancel", _) => {
                Self::verification(servers, buffer, CommandType::Cancel)
            }
            _ => unreachable!(),
        }
    }

    pub fn subcommands() -> Vec<Argparse<'static, 'static>> {
        vec![
            SubCommand::with_name("accept")
                .about("Accept a verification request"),
            SubCommand::with_name("confirm").about(
                "Confirm that the emoji matches or that the other side has \
                   scanned our QR code",
            ),
            SubCommand::with_name("cancel")
                .about("Cancel the verification flow"),
        ]
    }
}

impl CommandCallback for VerificationCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("verification")
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
