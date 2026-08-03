use std::{borrow::Cow, convert::TryFrom};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use matrix_sdk::ruma::OwnedUserId;
use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
};

use crate::{Servers, PLUGIN_NAME};

pub struct MentionSendCommand {
    servers: Servers,
}

impl MentionSendCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/matrix-send *",
            MentionSendCommand {
                servers: servers.clone(),
            },
        )
    }

    fn parse_payload(
        encoded: &str,
    ) -> Result<(String, Vec<OwnedUserId>), String> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "invalid encoded mention payload".to_owned())?;
        let payload: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid mention payload".to_owned())?;
        let body = payload
            .get("body")
            .and_then(|value| value.as_str())
            .filter(|body| !body.is_empty())
            .ok_or_else(|| "mention message body is empty".to_owned())?
            .to_owned();
        let user_ids = payload
            .get("user_ids")
            .and_then(|value| value.as_array())
            .ok_or_else(|| "mention payload has no user IDs".to_owned())?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| "mention user ID is not a string".to_owned())
                    .and_then(|value| {
                        OwnedUserId::try_from(value).map_err(|_| {
                            format!("invalid Matrix user ID: {}", value)
                        })
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;

        if user_ids.is_empty() {
            return Err("mention payload has no user IDs".to_owned());
        }

        Ok((body, user_ids))
    }

    fn print_error(buffer: &Buffer, message: &str) {
        buffer.print(&format!(
            "{}{}: {}",
            Weechat::prefix(Prefix::Error),
            PLUGIN_NAME,
            message
        ));
    }
}

impl CommandRunCallback for MentionSendCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        command: Cow<str>,
    ) -> ReturnCode {
        let Some(room) = self.servers.find_room(buffer) else {
            return ReturnCode::Ok;
        };
        let Some(encoded) = command
            .strip_prefix("/matrix-send ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            Self::print_error(buffer, "missing mention payload");
            return ReturnCode::OkEat;
        };
        let (body, user_ids) = match Self::parse_payload(encoded) {
            Ok(payload) => payload,
            Err(error) => {
                Self::print_error(buffer, &error);
                return ReturnCode::OkEat;
            }
        };
        let content = room.mention_message_content(buffer, body, user_ids);

        Weechat::spawn(async move { room.send_message(content).await })
            .detach();

        ReturnCode::OkEat
    }
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use matrix_sdk::ruma::owned_user_id;

    use super::MentionSendCommand;

    #[test]
    fn mention_payload_keeps_body_and_matrix_ids() {
        let encoded = URL_SAFE_NO_PAD.encode(
            br#"{"body":"hello @Ada","user_ids":["@ada:example.org"]}"#,
        );
        let (body, user_ids) =
            MentionSendCommand::parse_payload(&encoded).unwrap();

        assert_eq!(body, "hello @Ada");
        assert_eq!(user_ids, vec![owned_user_id!("@ada:example.org")]);
    }

    #[test]
    fn mention_payload_rejects_non_matrix_ids() {
        let encoded =
            URL_SAFE_NO_PAD.encode(br#"{"body":"hello","user_ids":["Ada"]}"#);

        assert!(MentionSendCommand::parse_payload(&encoded).is_err());
    }
}
