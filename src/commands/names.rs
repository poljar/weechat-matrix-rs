use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct NamesCommand {
    servers: Servers,
}

impl NamesCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "2000|/names",
            NamesCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for NamesCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
    ) -> ReturnCode {
        if let Some(room) = self.servers.find_room(buffer) {
            let names = room.names();
            let message = if names.is_empty() {
                "No Matrix room members are known yet.".to_owned()
            } else {
                format!("Matrix room members: {}", names.join(", "))
            };

            buffer.print(&message);
            ReturnCode::OkEat
        } else {
            ReturnCode::Ok
        }
    }
}
