use std::{borrow::Cow, path::PathBuf};

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::{room::thread_root_from_buffer, Servers};

pub struct UploadCommand {
    servers: Servers,
}

impl UploadCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/upload",
            UploadCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for UploadCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        cmd: Cow<str>,
    ) -> ReturnCode {
        let Some(room) = self.servers.find_room(buffer) else {
            Weechat::print(
                "The upload command needs to be executed in a room buffer",
            );
            return ReturnCode::Ok;
        };

        let Some(path) = cmd.strip_prefix("/upload").map(str::trim) else {
            return ReturnCode::Ok;
        };

        if path.is_empty() {
            Weechat::print("Usage: /upload <file>");
            return ReturnCode::Ok;
        }

        let path = PathBuf::from(Weechat::expand_home(path));
        let thread_root = thread_root_from_buffer(buffer);
        Weechat::spawn(
            async move { room.send_attachment(path, thread_root).await },
        )
        .detach();

        ReturnCode::Ok
    }
}
