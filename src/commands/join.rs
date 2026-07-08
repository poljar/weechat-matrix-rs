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
