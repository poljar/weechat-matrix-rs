mod status;

use weechat::hooks::BarItem;

use status::Status;
use crate::Servers;

pub struct BarItems {
    #[used]
    status: BarItem,
}

impl BarItems {
    pub fn hook_all(servers: Servers) -> Result<Self, ()> {
        Ok(Self {
            status: Status::create(servers)?,
        })
    }
}
