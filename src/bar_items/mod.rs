mod buffer_name;
mod buffer_plugin;
mod status;

use weechat::hooks::BarItem;

use crate::Servers;
use buffer_name::BufferName;
use buffer_plugin::BufferPlugin;
use status::Status;

pub struct BarItems {
    #[used]
    status: BarItem,
    #[used]
    buffer_name: BarItem,
    #[used]
    buffer_plugin: BarItem,
}

impl BarItems {
    pub fn hook_all(servers: Servers) -> Result<Self, ()> {
        Ok(Self {
            status: Status::create(servers.clone())?,
            buffer_name: BufferName::create(servers.clone())?,
            buffer_plugin: BufferPlugin::create(servers)?,
        })
    }
}
