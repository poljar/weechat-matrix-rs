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
            if room.close_thread_buffer(buffer) {
                return ReturnCode::OkEat;
            }

            Weechat::spawn(async move { room.leave().await }).detach();

            ReturnCode::OkEat
        } else {
            Weechat::print(
                "The /part command needs to be run in a Matrix room buffer.",
            );
            ReturnCode::Error
        }
    }
}
