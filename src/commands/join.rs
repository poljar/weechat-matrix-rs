use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
};

use crate::{Servers, PLUGIN_NAME};

pub struct JoinCommand {
    servers: Servers,
}

impl JoinCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/join *",
            JoinCommand {
                servers: servers.clone(),
            },
        )
    }

    pub fn join_room(
        servers: &Servers,
        buffer: &Buffer,
        room_id_or_alias: String,
        allow_single_server_fallback: bool,
    ) -> bool {
        let server = servers.find_server(buffer).or_else(|| {
            if allow_single_server_fallback {
                let servers = servers.borrow();

                if servers.len() == 1 {
                    return servers.values().next().cloned();
                }
            }

            None
        });

        if let Some(server) = server {
            let domain = server.user_id_domain();
            let room_id_or_alias =
                qualify_local_room_alias(&room_id_or_alias, domain.as_deref());

            Weechat::spawn(async move {
                server.join_room(room_id_or_alias).await;
            })
            .detach();

            true
        } else {
            false
        }
    }
}

fn qualify_local_room_alias(
    room_id_or_alias: &str,
    default_domain: Option<&str>,
) -> String {
    if room_id_or_alias.starts_with('#') && !room_id_or_alias.contains(':') {
        if let Some(default_domain) = default_domain {
            return format!("{}:{}", room_id_or_alias, default_domain);
        }
    }

    room_id_or_alias.to_owned()
}

impl CommandRunCallback for JoinCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        command: Cow<str>,
    ) -> ReturnCode {
        let Some(room_id_or_alias) = command.strip_prefix("/join ") else {
            return ReturnCode::Ok;
        };

        let room_id_or_alias = room_id_or_alias.trim();

        if room_id_or_alias.is_empty() {
            return ReturnCode::Ok;
        }

        if Self::join_room(
            &self.servers,
            buffer,
            room_id_or_alias.to_owned(),
            false,
        ) {
            ReturnCode::OkEat
        } else {
            ReturnCode::Ok
        }
    }
}

pub fn print_no_join_server_error() {
    Weechat::print(&format!(
        "{}{}: Run /matrix join from a Matrix buffer or configure exactly one server.",
        Weechat::prefix(Prefix::Error),
        PLUGIN_NAME
    ));
}

#[cfg(test)]
mod tests {
    use super::qualify_local_room_alias;

    #[test]
    fn appends_current_domain_to_local_room_alias() {
        assert_eq!(
            qualify_local_room_alias("#project-room", Some("matrix.org")),
            "#project-room:matrix.org"
        );
    }

    #[test]
    fn preserves_fully_qualified_room_alias() {
        assert_eq!(
            qualify_local_room_alias(
                "#project-room:example.org",
                Some("matrix.org")
            ),
            "#project-room:example.org"
        );
    }

    #[test]
    fn preserves_room_id() {
        assert_eq!(
            qualify_local_room_alias(
                "!opaque-room-id:example.org",
                Some("matrix.org")
            ),
            "!opaque-room-id:example.org"
        );
    }

    #[test]
    fn leaves_local_alias_unqualified_without_a_current_domain() {
        assert_eq!(
            qualify_local_room_alias("#project-room", None),
            "#project-room"
        );
    }
}
