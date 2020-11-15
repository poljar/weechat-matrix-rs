use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct BufferClearCommand {
    servers: Servers,
}

impl BufferClearCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/buffer clear",
            BufferClearCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for BufferClearCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
    ) -> ReturnCode {
        if let Some(room) = self.servers.find_room(buffer) {
            room.reset_prev_batch();
        }

        ReturnCode::Ok
    }
}
