use std::borrow::Cow;

use matrix_sdk::ruma::api::client::room::create_room;
use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct CreateCommand {
    servers: Servers,
}

impl CreateCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/create",
            CreateCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for CreateCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
    ) -> ReturnCode {
        if let Some(server) = buffer.get_localvar("server") {
            if let Some(server) = self.servers.get(&server) {
                if let Some(conn) = server.connection() {
                    let create_room = create_room::v3::Request::new();

                    let ret = self
                        .servers
                        .runtime()
                        .block_on(conn.client().create_room(create_room));

                    if ret.is_ok() {
                        return ReturnCode::Ok;
                    }
                }
            }
        }

        ReturnCode::Error
    }
}
