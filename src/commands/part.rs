use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct PartCommand {
    servers: Servers,
}

impl PartCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/part",
            PartCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for PartCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
    ) -> ReturnCode {
        if let Some(room) = self.servers.find_room(buffer) {
            let room = room.room().clone();

            match self.servers.runtime().block_on(room.leave()) {
                Ok(()) => ReturnCode::Ok,
                Err(error) => {
                    Weechat::print(&format!("Failed to leave room: {}", error));
                    ReturnCode::Error
                }
            }
        } else {
            Weechat::print(
                "The /part command needs to be run in a Matrix room buffer.",
            );
            ReturnCode::Error
        }
    }
}
