use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg, SubCommand};
use matrix_sdk::ruma::room::JoinRule;
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::Servers;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessAction {
    Public,
    Invite,
    Knock,
    Private,
}

impl AccessAction {
    fn rule(self) -> JoinRule {
        match self {
            Self::Public => JoinRule::Public,
            Self::Invite => JoinRule::Invite,
            Self::Knock => JoinRule::Knock,
            Self::Private => JoinRule::Private,
        }
    }
}

pub struct RoomAccessCommand {
    servers: Servers,
}

impl RoomAccessCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("room")
            .description("Change access rules for the current Matrix room.")
            .add_argument("make_public|make_invite_only")
            .add_argument("set_join_rule <public|invite|knock|private>")
            .arguments_description(
                "make_public: Allow anyone to join the room.\n\
                 make_invite_only: Require an invitation to join.\n\
                 set_join_rule: Set the join rule to public, invite, knock, or private.",
            )
            .add_completion(
                "make_public|make_invite_only|set_join_rule public|invite|knock|private",
            );

        Command::new(
            settings,
            Self {
                servers: servers.clone(),
            },
        )
    }

    fn parser() -> Argparse<'static, 'static> {
        Argparse::new("room")
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .subcommand(SubCommand::with_name("make_public"))
            .subcommand(SubCommand::with_name("make_invite_only"))
            .subcommand(
                SubCommand::with_name("set_join_rule")
                    .arg(Arg::with_name("rule").required(true)),
            )
    }

    fn action(matches: &clap::ArgMatches) -> Result<AccessAction, String> {
        match matches.subcommand() {
            ("make_public", _) => Ok(AccessAction::Public),
            ("make_invite_only", _) => Ok(AccessAction::Invite),
            ("set_join_rule", Some(args)) => match args.value_of("rule") {
                Some("public") => Ok(AccessAction::Public),
                Some("invite") | Some("invite_only") => Ok(AccessAction::Invite),
                Some("knock") => Ok(AccessAction::Knock),
                Some("private") => Ok(AccessAction::Private),
                Some(rule) => Err(format!(
                    "Unsupported join rule `{}`; use public, invite, knock, or private.",
                    rule
                )),
                None => Err("A join rule is required.".to_owned()),
            },
            _ => Err("Usage: /room make_public|make_invite_only|set_join_rule <public|invite|knock|private>".to_owned()),
        }
    }

    fn run(&self, buffer: &Buffer, action: AccessAction) {
        let Some(room) = self.servers.find_room(buffer) else {
            buffer.print(&format!(
                "{}The /room command needs to be run in a Matrix room buffer.",
                Weechat::prefix(Prefix::Error)
            ));
            return;
        };

        Weechat::spawn(async move { room.set_join_rule(action.rule()).await })
            .detach();
    }
}

impl CommandCallback for RoomAccessCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        parse_and_run(Self::parser(), arguments, |matches| match Self::action(
            matches,
        ) {
            Ok(action) => self.run(buffer, action),
            Err(error) => buffer.print(&format!(
                "{}{}",
                Weechat::prefix(Prefix::Error),
                error
            )),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessAction, RoomAccessCommand};

    fn parse(args: &[&str]) -> Result<AccessAction, String> {
        let matches = RoomAccessCommand::parser()
            .get_matches_from_safe(args)
            .map_err(|error| error.to_string())?;
        RoomAccessCommand::action(&matches)
    }

    #[test]
    fn explicit_shortcuts_select_join_rules() {
        assert_eq!(Ok(AccessAction::Public), parse(&["room", "make_public"]));
        assert_eq!(
            Ok(AccessAction::Invite),
            parse(&["room", "make_invite_only"])
        );
    }

    #[test]
    fn generic_command_accepts_public_and_invite_rules() {
        assert_eq!(
            Ok(AccessAction::Public),
            parse(&["room", "set_join_rule", "public"])
        );
        assert_eq!(
            Ok(AccessAction::Invite),
            parse(&["room", "set_join_rule", "invite"])
        );
        assert_eq!(
            Ok(AccessAction::Knock),
            parse(&["room", "set_join_rule", "knock"])
        );
        assert_eq!(
            Ok(AccessAction::Private),
            parse(&["room", "set_join_rule", "private"])
        );
    }

    #[test]
    fn generic_command_rejects_unsupported_rules() {
        assert!(parse(&["room", "set_join_rule", "restricted"]).is_err());
    }
}
