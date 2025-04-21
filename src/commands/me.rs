use std::borrow::Cow;

use matrix_sdk::ruma::events::{
    message::MessageEventContent,
    room::message::{NoticeMessageEventContent, RoomMessageEventContent},
};
use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct MeCommand {
    servers: Servers,
}

impl MeCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/me",
            MeCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for MeCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        cmd: Cow<str>,
    ) -> ReturnCode {
        if let Some(room) = self.servers.find_room(buffer) {
            self.servers.runtime().block_on(room.send_message(
                RoomMessageEventContent::emote_plain(
                    cmd.strip_prefix("/me ").map(|s| s.to_string()).unwrap(),
                ),
            ));
        }

        ReturnCode::Ok
    }
}
