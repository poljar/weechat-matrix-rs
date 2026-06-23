use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use matrix_sdk::ruma::{OwnedUserId, UserId};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::Servers;

pub struct InviteCommand {
    servers: Servers,
}

impl InviteCommand {
    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("invite")
            .description("Invite a Matrix user to the current room.")
            .add_argument("<user-id>")
            .arguments_description(
                "user-id: The Matrix user ID to invite to the current room.",
            )
            .add_completion("%(matrix-users)");

        Command::new(
            settings,
            InviteCommand {
                servers: servers.clone(),
            },
        )
    }

    fn invite(&self, buffer: &Buffer, user_id: OwnedUserId) {
        if let Some(room) = self.servers.find_room(buffer) {
            let room = room.room().clone();
            let invited_user = user_id.clone();

            Weechat::spawn(async move {
                match room.invite_user_by_id(&user_id).await {
                    Ok(()) => Weechat::print(&format!(
                        "{}Invited {} to the room.",
                        Weechat::prefix(Prefix::Network),
                        invited_user
                    )),
                    Err(error) => Weechat::print(&format!(
                        "{}Failed to invite {}: {}",
                        Weechat::prefix(Prefix::Error),
                        invited_user,
                        error
                    )),
                }
            })
            .detach();
        } else {
            Weechat::print(
                "The /invite command needs to be run in a Matrix room buffer.",
            );
        }
    }
}

impl CommandCallback for InviteCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let parser = Argparse::new("invite")
            .about("Invite a Matrix user to the current room.")
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(Arg::with_name("user-id").required(true).validator(|u| {
                UserId::parse(u)
                    .map_err(|_| {
                        "The given user isn't a valid user ID".to_owned()
                    })
                    .map(|_| ())
            }));

        parse_and_run(parser, arguments, |matches| {
            let user_id = matches
                .value_of("user-id")
                .map(|u| {
                    UserId::parse(u)
                        .expect("Argument was already validated as a user ID")
                })
                .expect("User ID not set but was required");

            self.invite(buffer, user_id);
        });
    }
}
