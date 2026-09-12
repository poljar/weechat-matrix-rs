use weechat::{
    buffer::Buffer,
    hooks::{BarItem, BarItemCallback},
    Weechat,
};

use crate::{BufferOwner, Servers};

pub(super) struct TypingNotice {
    servers: Servers,
}

impl TypingNotice {
    pub(super) fn create(servers: Servers) -> Result<BarItem, ()> {
        let status = TypingNotice { servers };
        BarItem::new("matrix_typing_notice", status)
    }
}

impl BarItemCallback for TypingNotice {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer) -> String {
        match self.servers.buffer_owner(buffer) {
            BufferOwner::Room(_, room) => room.typing_notice(),
            _ => String::new(),
        }
    }
}
