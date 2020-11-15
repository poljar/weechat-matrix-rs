use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct PageUpCommand {
    servers: Servers,
}

impl PageUpCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/window page_up",
            PageUpCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for PageUpCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
    ) -> ReturnCode {
        if let Some(room) = self.servers.find_room(buffer) {
            if let Some(window) = buffer.window() {
                if window.is_first_line_displayed() || buffer.num_lines() == 0 {
                    Weechat::spawn(async move { room.get_messages().await })
                        .detach();
                }
            }
        }

        ReturnCode::Ok
    }
}
