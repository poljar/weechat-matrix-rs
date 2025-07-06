use weechat::{
    buffer::Buffer,
    hooks::{BarItem, BarItemCallback},
    Weechat,
};

use crate::{BufferOwner, Servers};

pub(super) struct BufferName {
    servers: Servers,
}

impl BufferName {
    pub(super) fn create(servers: Servers) -> Result<BarItem, ()> {
        let status = BufferName { servers };
        BarItem::new("buffer_name", status)
    }
}

impl BarItemCallback for BufferName {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer) -> String {
        match self.servers.buffer_owner(buffer) {
            BufferOwner::Server(server) => {
                let color = if server.is_connection_secure() {
                    "status_name_ssl"
                } else {
                    "status_name"
                };

                format!(
                    "{color}server{del_color}[{color}{name}{del_color}]",
                    color = Weechat::color(color),
                    del_color = Weechat::color("bar_delim"),
                    name = server.name()
                )
            }

            BufferOwner::Room(server, _) => {
                let color = if server.is_connection_secure() {
                    "status_name_ssl"
                } else {
                    "status_name"
                };

                format!("{}{}", Weechat::color(color), buffer.short_name())
            }

            BufferOwner::Verification(_, _) => {
                // TODO special format this
                format!("{}{}", Weechat::color("status_name"), buffer.name())
            }

            BufferOwner::None => {
                format!("{}{}", Weechat::color("status_name"), buffer.name())
            }
        }
    }
}
