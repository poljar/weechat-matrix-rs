use clap::{App as Argparse, Arg, ArgMatches};
use matrix_sdk::ruma::{OwnedDeviceId, UserId};
use weechat::{buffer::Buffer, Weechat};

use crate::Servers;

pub struct VerifyCommand;

impl VerifyCommand {
    pub const DESCRIPTION: &'static str =
        "Mark an exact Matrix device as locally verified";

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        let user_id = UserId::parse(
            args.value_of("contact")
                .expect("Contact wasn't provided despite being required"),
        )
        .expect("Contact wasn't a valid Matrix user ID");
        let device_id: OwnedDeviceId = args
            .value_of("device-id")
            .expect("Device ID wasn't provided despite being required")
            .into();

        if let Some(server) = servers.find_server(buffer) {
            Weechat::spawn(async move {
                server.mark_device_verified(user_id, device_id).await;
            })
            .detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn parser() -> Argparse<'static, 'static> {
        Argparse::new("verify")
            .about(Self::DESCRIPTION)
            .arg(Arg::with_name("contact").required(true).validator(
                |contact| {
                    UserId::parse(contact).map(|_| ()).map_err(|_| {
                        "The contact isn't a valid Matrix user ID".to_owned()
                    })
                },
            ))
            .arg(Arg::with_name("device-id").required(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_contact_and_device() {
        let matches = VerifyCommand::parser()
            .get_matches_from_safe(vec![
                "verify",
                "@alice:example.org",
                "ALICEDEVICE",
            ])
            .expect("valid verify command");

        assert_eq!(matches.value_of("contact"), Some("@alice:example.org"));
        assert_eq!(matches.value_of("device-id"), Some("ALICEDEVICE"));
    }

    #[test]
    fn rejects_missing_or_invalid_target() {
        assert!(VerifyCommand::parser()
            .get_matches_from_safe(vec!["verify", "alice", "ALICEDEVICE"])
            .is_err());
        assert!(VerifyCommand::parser()
            .get_matches_from_safe(vec!["verify", "@alice:example.org"])
            .is_err());
    }
}
