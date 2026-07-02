use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
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
            let display_name = buffer
                .get_localvar("nick")
                .map(|nick| nick.into_owned())
                .unwrap_or_else(|| room.room_id().to_string());
            let matrix_room = room.room().clone();

            match self.servers.runtime().block_on(matrix_room.leave()) {
                Ok(()) => {
                    buffer.print(&format!(
                        "{}{} has left the room",
                        Weechat::prefix(Prefix::Quit),
                        display_name,
                    ));
                    ReturnCode::OkEat
                }
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
