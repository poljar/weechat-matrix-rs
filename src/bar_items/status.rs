use weechat::{
    buffer::Buffer,
    hooks::{BarItem, BarItemCallback},
    Weechat,
};

use crate::Servers;

pub(super) struct Status {
    servers: Servers,
}

impl Status {
    pub(super) fn create(servers: Servers) -> Result<BarItem, ()> {
        let status = Status { servers };
        BarItem::new("matrix_modes", status)
    }
}

impl BarItemCallback for Status {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer) -> String {
        let servers = self.servers.borrow();

        let mut signs = Vec::new();

        for server in servers.values() {
            let server = server.inner();

            for room in server.rooms().values() {
                if let Ok(b) = room.buffer_handle().upgrade() {
                    if buffer == &b {
                        if room.is_encrypted() {
                            signs.push(
                                server.config().look().encrypted_room_sign(),
                            );
                        }

                        if room.is_busy() {
                            signs.push("⏳".to_owned());
                        }

                        break;
                    }
                }
            }
        }

        signs.join("")
    }
}
