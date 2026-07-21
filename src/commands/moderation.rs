use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches};
use matrix_sdk::ruma::{OwnedUserId, UserId};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::{Servers, PLUGIN_NAME};

#[derive(Clone, Copy)]
enum ModerationAction {
    Ban,
    Kick,
    Unban,
}

impl ModerationAction {
    fn command(&self) -> &'static str {
        match self {
            ModerationAction::Ban => "ban",
            ModerationAction::Kick => "kick",
            ModerationAction::Unban => "unban",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            ModerationAction::Ban => "Ban a Matrix user from the current room.",
            ModerationAction::Kick => {
                "Kick a Matrix user from the current room."
            }
            ModerationAction::Unban => {
                "Unban a Matrix user from the current room."
            }
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            ModerationAction::Ban => "ban",
            ModerationAction::Kick => "kick",
            ModerationAction::Unban => "unban",
        }
    }
}

pub struct ModerationCommand {
    servers: Servers,
    action: ModerationAction,
}

impl ModerationCommand {
    fn create(
        servers: &Servers,
        action: ModerationAction,
    ) -> Result<Command, ()> {
        let settings = CommandSettings::new(action.command())
            .description(action.description())
            .add_argument("<user-id> [reason]")
            .arguments_description(format!(
                "user-id: The Matrix user ID to {}.\nreason: Optional reason.",
                action.verb()
            ))
            .add_completion("%(matrix-users) %-");

        Command::new(
            settings,
            ModerationCommand {
                servers: servers.clone(),
                action,
            },
        )
    }

    pub fn ban(servers: &Servers) -> Result<Command, ()> {
        Self::create(servers, ModerationAction::Ban)
    }

    pub fn kick(servers: &Servers) -> Result<Command, ()> {
        Self::create(servers, ModerationAction::Kick)
    }

    pub fn unban(servers: &Servers) -> Result<Command, ()> {
        Self::create(servers, ModerationAction::Unban)
    }

    fn parse_reason(args: &ArgMatches) -> Option<String> {
        args.values_of("reason")
            .map(|values| values.collect::<Vec<_>>().join(" "))
            .filter(|reason| !reason.is_empty())
    }

    fn run(
        &self,
        buffer: &Buffer,
        user_id: OwnedUserId,
        reason: Option<String>,
    ) {
        let Some(room) = self.servers.find_room(buffer) else {
            Weechat::print(&format!(
                "{}{}: /{} needs to be run in a Matrix room buffer.",
                Weechat::prefix(Prefix::Error),
                PLUGIN_NAME,
                self.action.command()
            ));
            return;
        };

        let action = self.action;
        Weechat::spawn(async move {
            match action {
                ModerationAction::Ban => room.ban_user(user_id, reason).await,
                ModerationAction::Kick => room.kick_user(user_id, reason).await,
                ModerationAction::Unban => {
                    room.unban_user(user_id, reason).await
                }
            };
        })
        .detach();
    }
}

impl CommandCallback for ModerationCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let parser = Argparse::new(self.action.command())
            .about(self.action.description())
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(Arg::with_name("user-id").required(true).validator(|u| {
                UserId::parse(u).map(|_| ()).map_err(|_| {
                    "The given user is not a valid user ID".to_owned()
                })
            }))
            .arg(
                Arg::with_name("reason")
                    .multiple(true)
                    .allow_hyphen_values(true),
            );

        parse_and_run(parser, arguments, |matches| {
            let user_id = matches
                .value_of("user-id")
                .map(|u| {
                    UserId::parse(u)
                        .expect("Argument was already validated as a user ID")
                })
                .expect("User ID not set but was required");
            let reason = Self::parse_reason(matches);

            self.run(buffer, user_id, reason);
        });
    }
}
