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
            let buffer_handle = room.buffer_handle();
            let matrix_room = room.room().clone();

            Weechat::spawn(async move {
                match matrix_room.leave().await {
                    Ok(()) => {
                        if let Ok(buffer) = buffer_handle.upgrade() {
                            buffer.print(&format!(
                                "{}{} has left the room",
                                Weechat::prefix(Prefix::Quit),
                                display_name,
                            ));
                        }
                    }
                    Err(error) => Weechat::print(&format!(
                        "Failed to leave room: {}",
                        error
                    )),
                }
            })
            .detach();

            ReturnCode::OkEat
        } else {
            Weechat::print(
                "The /part command needs to be run in a Matrix room buffer.",
            );
            ReturnCode::Error
        }
    }
}
