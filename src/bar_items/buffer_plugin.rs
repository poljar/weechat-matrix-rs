use weechat::{
    buffer::Buffer,
    hooks::{BarItem, BarItemCallback},
    Weechat,
};

use crate::{BufferOwner, Servers, PLUGIN_NAME};

pub(super) struct BufferPlugin {
    servers: Servers,
}

impl BufferPlugin {
    pub(super) fn create(servers: Servers) -> Result<BarItem, ()> {
        let status = Self { servers };
        BarItem::new("buffer_plugin", status)
    }
}

impl BarItemCallback for BufferPlugin {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer) -> String {
        match self.servers.buffer_owner(buffer) {
            BufferOwner::Server(s)
            | BufferOwner::Room(s, _)
            | BufferOwner::Verification(s, _) => {
                format!(
                    "{plugin_name}{del_color}/{color}{name}",
                    plugin_name = PLUGIN_NAME,
                    del_color = Weechat::color("bar_delim"),
                    color = Weechat::color("bar_fg"),
                    name = s.name()
                )
            }

            BufferOwner::None => "".to_owned(),
        }
    }
}
