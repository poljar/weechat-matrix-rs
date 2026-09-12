mod buffer_name;
mod buffer_plugin;
mod status;
mod typing_notice;

use weechat::hooks::BarItem;

use crate::Servers;
use buffer_name::BufferName;
use buffer_plugin::BufferPlugin;
use status::Status;
use typing_notice::TypingNotice;

pub struct BarItems {
    #[allow(dead_code)]
    status: BarItem,
    #[allow(dead_code)]
    buffer_name: BarItem,
    #[allow(dead_code)]
    buffer_plugin: BarItem,
    #[allow(dead_code)]
    typing_notice: BarItem,
}

impl BarItems {
    pub fn hook_all(servers: Servers) -> Result<Self, ()> {
        Ok(Self {
            status: Status::create(servers.clone())?,
            buffer_name: BufferName::create(servers.clone())?,
            buffer_plugin: BufferPlugin::create(servers.clone())?,
            typing_notice: TypingNotice::create(servers)?,
        })
    }
}
