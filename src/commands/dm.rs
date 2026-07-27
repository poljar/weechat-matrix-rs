use std::borrow::Cow;

use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
};

use super::invite::normalize_invitee;
use crate::{MatrixServer, Servers, PLUGIN_NAME};

pub struct DirectMessageCommand {
    servers: Servers,
    kind: DirectMessageCommandKind,
}

#[derive(Clone, Copy)]
enum DirectMessageCommandKind {
    Query,
    Msg,
}

impl DirectMessageCommandKind {
    fn command(self) -> &'static str {
        match self {
            Self::Query => "/query",
            Self::Msg => "/msg",
        }
    }
}

impl DirectMessageCommand {
    pub fn query(servers: &Servers) -> Result<CommandRun, ()> {
        Self::create(servers, DirectMessageCommandKind::Query)
    }

    pub fn msg(servers: &Servers) -> Result<CommandRun, ()> {
        Self::create(servers, DirectMessageCommandKind::Msg)
    }

    fn create(
        servers: &Servers,
        kind: DirectMessageCommandKind,
    ) -> Result<CommandRun, ()> {
        CommandRun::new(
            &format!("{} *", kind.command()),
            DirectMessageCommand {
                servers: servers.clone(),
                kind,
            },
        )
    }

    fn matrix_server(&self, buffer: &Buffer) -> Option<MatrixServer> {
        self.servers.find_server(buffer)
    }

    fn default_domain(&self, buffer: &Buffer, server: &MatrixServer) -> String {
        self.servers
            .find_room(buffer)
            .and_then(|room| {
                room.room_id()
                    .server_name()
                    .map(|server_name| server_name.as_str().to_owned())
            })
            .or_else(|| server.user_id_domain())
            .unwrap_or_default()
    }

    fn print_error(buffer: &Buffer, message: &str) {
        buffer.print(&format!(
            "{}{}: {}",
            Weechat::prefix(Prefix::Error),
            PLUGIN_NAME,
            message
        ));
    }

    fn run_matrix_command(
        &self,
        buffer: &Buffer,
        user: &str,
        message: Option<String>,
    ) {
        let Some(server) = self.matrix_server(buffer) else {
            return;
        };

        let domain = self.default_domain(buffer, &server);
        let user_id = match normalize_invitee(user, &domain) {
            Ok(user_id) => user_id,
            Err(error) => {
                Self::print_error(buffer, &error);
                return;
            }
        };

        Weechat::spawn(async move {
            let Some(room) = server.get_or_create_dm(user_id).await else {
                return;
            };

            if let Ok(buffer) = room.buffer_handle().upgrade() {
                buffer.switch_to();
            }

            if let Some(message) = message.filter(|message| !message.is_empty())
            {
                room.send_message(RoomMessageEventContent::text_plain(message))
                    .await;
            }
        })
        .detach();
    }
}

impl CommandRunCallback for DirectMessageCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        command: Cow<str>,
    ) -> ReturnCode {
        if self.matrix_server(buffer).is_none() {
            return ReturnCode::Ok;
        }

        let Some(arguments) = command.strip_prefix(self.kind.command()) else {
            return ReturnCode::Ok;
        };

        let arguments = arguments.trim();
        if arguments.is_empty() {
            Self::print_error(
                buffer,
                &format!("Usage: {} <user-id> [message]", self.kind.command()),
            );
            return ReturnCode::OkEat;
        }

        let (user, message) = match self.kind {
            DirectMessageCommandKind::Query => (arguments, None),
            DirectMessageCommandKind::Msg => parse_msg_arguments(arguments),
        };

        self.run_matrix_command(buffer, user, message);

        ReturnCode::OkEat
    }
}

fn parse_msg_arguments(arguments: &str) -> (&str, Option<String>) {
    let mut parts = arguments.splitn(2, char::is_whitespace);
    let user = parts.next().unwrap_or_default();
    let message = parts.next().map(str::trim_start).map(str::to_owned);

    (user, message)
}

#[cfg(test)]
mod tests {
    use super::parse_msg_arguments;

    #[test]
    fn msg_arguments_preserve_message_body() {
        assert_eq!(
            parse_msg_arguments("@friend hi there"),
            ("@friend", Some("hi there".to_owned()))
        );
    }

    #[test]
    fn msg_arguments_allow_opening_without_message() {
        assert_eq!(parse_msg_arguments("@friend"), ("@friend", None));
    }
}
