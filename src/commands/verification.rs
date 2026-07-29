use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use matrix_sdk::ruma::{OwnedUserId, UserId};

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

    pub const COMPLETION: &'static str =
        "start %(matrix-users)|info %(matrix-users)|accept|confirm|cancel";
    pub const SETTINGS: &'static [ArgParseSettings] = &[
        ArgParseSettings::DisableHelpFlags,
        ArgParseSettings::DisableVersion,
        ArgParseSettings::VersionlessSubcommands,
        ArgParseSettings::SubcommandRequiredElseHelp,
    ];

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("verification")
            .description(Self::DESCRIPTION)
            .add_argument("verification start <contact>")
            .add_argument("verification info [contact]")
            .add_argument("verification accept|confirm|cancel")
            .arguments_description(
                "  start: start an interactive verification with a contact
                  info: show verification state for one or all known contacts
                accept: accept the verification request
                confirm: confirm that the emojis match on both sides or \
                confirm that the other side has scanned our QR code
                cancel: cancel the verification flow or request",
            )
            .add_completion(Self::COMPLETION)
            .add_completion("help start|info|accept|confirm|cancel");

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

    fn start(servers: &Servers, buffer: &Buffer, user_id: OwnedUserId) {
        if let Some(server) = servers.find_server(buffer) {
            Weechat::spawn(async move {
                server.start_verification(user_id).await;
            })
            .detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    fn info(servers: &Servers, buffer: &Buffer, user_id: Option<OwnedUserId>) {
        if let Some(server) = servers.find_server(buffer) {
            Weechat::spawn(async move {
                server.verification_info(user_id).await;
            })
            .detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        match args.subcommand() {
            ("start", Some(args)) => {
                let user_id =
                    UserId::parse(args.value_of("contact").expect(
                        "Contact wasn't provided despite being required",
                    ))
                    .expect("Contact wasn't a valid Matrix user ID");
                Self::start(servers, buffer, user_id);
            }
            ("info", Some(args)) => {
                let user_id = args.value_of("contact").map(|contact| {
                    UserId::parse(contact)
                        .expect("Contact wasn't a valid Matrix user ID")
                });
                Self::info(servers, buffer, user_id);
            }
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
            SubCommand::with_name("start")
                .about("Start an interactive verification with a contact")
                .arg(Arg::with_name("contact").required(true).validator(
                    |contact| {
                        UserId::parse(contact).map(|_| ()).map_err(|_| {
                            "The contact isn't a valid Matrix user ID"
                                .to_owned()
                        })
                    },
                )),
            SubCommand::with_name("info")
                .about("Show verification state for one or all known contacts")
                .arg(Arg::with_name("contact").required(false).validator(
                    |contact| {
                        UserId::parse(contact).map(|_| ()).map_err(|_| {
                            "The contact isn't a valid Matrix user ID"
                                .to_owned()
                        })
                    },
                )),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> Argparse<'static, 'static> {
        Argparse::new("verification")
            .settings(VerificationCommand::SETTINGS)
            .subcommands(VerificationCommand::subcommands())
    }

    #[test]
    fn parses_verification_start_contact() {
        let matches = parser()
            .get_matches_from_safe(vec![
                "verification",
                "start",
                "@alice:example.org",
            ])
            .expect("valid verification start command");
        let args = matches.subcommand_matches("start").expect("start args");

        assert_eq!(args.value_of("contact"), Some("@alice:example.org"));
    }

    #[test]
    fn rejects_invalid_verification_start_contact() {
        assert!(parser()
            .get_matches_from_safe(vec!["verification", "start", "alice"])
            .is_err());
    }

    #[test]
    fn parses_verification_info_with_optional_contact() {
        let all = parser()
            .get_matches_from_safe(vec!["verification", "info"])
            .expect("valid verification info command");
        assert_eq!(
            all.subcommand_matches("info")
                .expect("info args")
                .value_of("contact"),
            None
        );

        let one = parser()
            .get_matches_from_safe(vec![
                "verification",
                "info",
                "@alice:example.org",
            ])
            .expect("valid verification info contact");
        assert_eq!(
            one.subcommand_matches("info")
                .expect("info args")
                .value_of("contact"),
            Some("@alice:example.org")
        );
    }

    #[test]
    fn rejects_invalid_verification_info_contact() {
        assert!(parser()
            .get_matches_from_safe(vec!["verification", "info", "alice"])
            .is_err());
    }
}

impl CommandCallback for VerificationCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("verification")
            .about(Self::DESCRIPTION)
            .settings(Self::SETTINGS)
            .subcommands(Self::subcommands());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, &matches)
        });
    }
}
