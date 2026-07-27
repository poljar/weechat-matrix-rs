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

    fn print_error(buffer: &Buffer, message: &str) {
        buffer.print(&format!("{}{}", Weechat::prefix(Prefix::Error), message));
    }

    fn invite(&self, buffer: &Buffer, input: &str) {
        let Some(room) = self.servers.find_room(buffer) else {
            Self::print_error(
                buffer,
                "The /invite command needs to be run in a Matrix room buffer.",
            );
            return;
        };

        let domain = room
            .room_id()
            .server_name()
            .map(|server_name| server_name.as_str())
            .unwrap_or_default();

        let user_id = match normalize_invitee(input, domain) {
            Ok(user_id) => user_id,
            Err(error) => {
                Self::print_error(buffer, &error);
                return;
            }
        };

        Weechat::spawn(async move { room.invite_user(user_id).await }).detach();
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
            .arg(Arg::with_name("user-id").required(true));

        parse_and_run(parser, arguments, |matches| {
            let user_id = matches
                .value_of("user-id")
                .expect("User ID not set but was required");

            self.invite(buffer, user_id);
        });
    }
}

pub(crate) fn normalize_invitee(
    input: &str,
    default_domain: &str,
) -> Result<OwnedUserId, String> {
    let input = input.trim();
    let candidate = if input.contains(':') {
        input.to_owned()
    } else {
        let localpart = input.strip_prefix('@').unwrap_or(input);
        format!("@{}:{}", localpart, default_domain)
    };

    UserId::parse(candidate.as_str()).map_err(|error| {
        format!("Invalid Matrix user ID `{}`: {}", candidate, error)
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_invitee;

    #[test]
    fn keeps_fully_qualified_user_ids() {
        let user = normalize_invitee("@strk:osgeo.org", "matrix.org").unwrap();
        assert_eq!("@strk:osgeo.org", user.as_str());
    }

    #[test]
    fn appends_room_domain_to_local_user_ids() {
        let user = normalize_invitee("@strk", "matrix.org").unwrap();
        assert_eq!("@strk:matrix.org", user.as_str());
    }

    #[test]
    fn accepts_localparts_without_at_sign() {
        let user = normalize_invitee("strk", "matrix.org").unwrap();
        assert_eq!("@strk:matrix.org", user.as_str());
    }

    #[test]
    fn reports_the_candidate_user_id_on_error() {
        let error = normalize_invitee("@strk", "").unwrap_err();
        assert!(error.contains("@strk:"));
    }
}
