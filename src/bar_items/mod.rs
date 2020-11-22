mod buffer_name;
mod status;

use weechat::hooks::BarItem;

use crate::Servers;
use buffer_name::BufferName;
use status::Status;

pub struct BarItems {
    #[used]
    status: BarItem,
    #[used]
    buffer_name: BarItem,
}

impl BarItems {
    pub fn hook_all(servers: Servers) -> Result<Self, ()> {
        Ok(Self {
            status: Status::create(servers.clone())?,
            buffer_name: BufferName::create(servers)?,
        })
    }
}
