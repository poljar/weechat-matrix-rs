use std::{borrow::Cow, cell::RefCell, rc::Rc};

use matrix_sdk::{
    ruma::{EventId, OwnedRoomAliasId, TransactionId, UserId},
    Room, StoreError,
};
use tokio::runtime::Handle;
use weechat::{
    buffer::{Buffer, BufferHandle, BufferLine, LineData},
    Prefix, Weechat,
};

use crate::{render::RenderedEvent, utils::ToTag};

#[derive(Clone)]
pub struct RoomBuffer {
    room: Room,
    runtime: Handle,
    pub(super) inner: Rc<RefCell<Option<BufferHandle>>>,
}

impl RoomBuffer {
    pub fn new(room: Room, runtime: Handle) -> Self {
        Self {
            room,
            runtime,
            inner: Rc::new(RefCell::new(None)),
        }
    }

    pub fn buffer_handle(&self) -> BufferHandle {
        self.inner
            .borrow()
            .as_ref()
            .expect("Room struct wasn't initialized properly")
            .clone()
    }

    pub fn short_name(&self) -> String {
        self.inner
            .borrow()
            .as_ref()
            .and_then(|b| b.upgrade().ok().map(|b| b.short_name().to_string()))
            .unwrap_or_default()
    }

    /// Replace the local echo of an event with a fully rendered one.
    pub fn replace_local_echo(
        &self,
        transaction_id: &TransactionId,
        rendered: RenderedEvent,
    ) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            let uuid_tag = Cow::from(format!("matrix_echo_{}", transaction_id));
            let line_contains_uuid =
                |l: &BufferLine| l.tags().contains(&uuid_tag);

            let mut lines = buffer.lines();
            let mut current_line = lines.rfind(line_contains_uuid);

            // We go in reverse order here since we also use rfind(). We got from
            // the bottom of the buffer to the top since we're expecting these
            // lines to be freshly printed and thus at the bottom.
            let mut line_num = rendered.content.lines.len();

            while let Some(line) = &current_line {
                line_num -= 1;
                let rendered_line = &rendered.content.lines[line_num];

                line.set_message(&rendered_line.message);
                current_line = lines.next_back().filter(line_contains_uuid);
            }
        }
    }

    pub fn replace_edit(
        &self,
        event_id: &EventId,
        sender: &UserId,
        event: RenderedEvent,
    ) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            let sender_tag = Cow::from(sender.to_tag());
            let event_id_tag = Cow::from(event_id.to_tag());

            let lines: Vec<BufferLine> = buffer
                .lines()
                .filter(|l| l.tags().contains(&event_id_tag))
                .collect();

            if lines
                .get(0)
                .map(|l| l.tags().contains(&sender_tag))
                .unwrap_or(false)
            {
                self.replace_event_helper(&buffer, lines, event);
            }
        }
    }

    fn replace_event_helper(
        &self,
        buffer: &Buffer,
        lines: Vec<BufferLine<'_>>,
        event: RenderedEvent,
    ) {
        use std::cmp::Ordering;
        let date = lines.get(0).map(|l| l.date()).unwrap_or_default();

        for (line, new) in lines.iter().zip(event.content.lines.iter()) {
            let data = LineData {
                // Our prefixes always come with a \t character, but when we
                // replace stuff we're able to replace the prefix and the
                // message separately, so trim the whitespace in the prefix.
                prefix: Some(event.prefix.trim_end()),
                message: Some(&new.message),
                ..Default::default()
            };

            line.update(data);
        }

        match lines.len().cmp(&event.content.lines.len()) {
            Ordering::Greater => {
                for line in &lines[event.content.lines.len()..] {
                    line.set_message("");
                }
            }
            Ordering::Less => {
                for line in &event.content.lines[lines.len()..] {
                    let message = format!("{}{}", &event.prefix, &line.message);
                    let tags: Vec<&str> =
                        line.tags.iter().map(|t| t.as_str()).collect();
                    buffer.print_date_tags(date, &tags, &message)
                }

                self.sort_messages()
            }
            Ordering::Equal => (),
        }
    }

    pub fn sort_messages(&self) {
        struct LineCopy {
            date: i64,
            date_printed: i64,
            tags: Vec<String>,
            prefix: String,
            message: String,
        }

        impl<'a> From<BufferLine<'a>> for LineCopy {
            fn from(line: BufferLine) -> Self {
                Self {
                    date: line.date(),
                    date_printed: line.date_printed(),
                    message: line.message().to_string(),
                    prefix: line.prefix().to_string(),
                    tags: line.tags().iter().map(|t| t.to_string()).collect(),
                }
            }
        }

        // TODO update the highlight once Weechat starts supporting it.
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            let mut lines: Vec<LineCopy> =
                buffer.lines().map(|l| l.into()).collect();
            lines.sort_by_key(|l| l.date);

            for (line, new) in buffer.lines().zip(lines.drain(..)) {
                let tags =
                    new.tags.iter().map(|t| t.as_str()).collect::<Vec<&str>>();
                let data = LineData {
                    prefix: Some(&new.prefix),
                    message: Some(&new.message),
                    date: Some(new.date),
                    date_printed: Some(new.date_printed),
                    tags: Some(&tags),
                };
                line.update(data)
            }
        }
    }

    pub fn set_topic(&self) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.set_title(&self.room.topic().unwrap_or_default());
        }
    }

    pub fn set_alias(&self) {
        if let Some(alias) = self.alias() {
            if let Ok(b) = self.buffer_handle().upgrade() {
                b.set_localvar("alias", alias.as_str());
            }
        }
    }

    fn alias(&self) -> Option<OwnedRoomAliasId> {
        self.room.canonical_alias()
    }

    pub fn calculate_buffer_name(&self) -> Result<String, StoreError> {
        let room = self.room.clone();
        let room_name = self.runtime.block_on(room.display_name())?.to_string();

        let room_name = if room_name == "#" {
            "##".to_owned()
        } else if room_name.starts_with('#')
            || self.runtime.block_on(room.is_direct()).unwrap_or(false)
        {
            room_name
        } else {
            format!("#{}", room_name)
        };

        Ok(room_name.to_string())
    }

    pub fn update_buffer_name(&self) {
        let buffer = self.buffer_handle();

        let buffer = if let Ok(b) = buffer.upgrade() {
            b
        } else {
            return;
        };

        match self.calculate_buffer_name() {
            Ok(name) => buffer.set_short_name(&name),
            Err(e) => {
                Weechat::print(&format!(
                    "{}: Error fetching the room name from the store: {}",
                    Weechat::prefix(Prefix::Error),
                    e.to_string(),
                ));
            }
        }
    }

    pub fn replace_verification_event(
        &self,
        event_id: &EventId,
        event: RenderedEvent,
    ) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            let event_id_tag = Cow::from(event_id.to_tag());

            let lines: Vec<BufferLine> = buffer
                .lines()
                .filter(|l| l.tags().contains(&event_id_tag))
                .collect();

            self.replace_event_helper(&buffer, lines, event);
        }
    }

    pub fn print_rendered_event(&self, rendered: RenderedEvent) {
        let buffer = self.buffer_handle();

        if let Ok(buffer) = buffer.upgrade() {
            for line in rendered.content.lines {
                let message = format!("{}{}", &rendered.prefix, &line.message);
                let tags: Vec<&str> =
                    line.tags.iter().map(|t| t.as_str()).collect();
                buffer.print_date_tags(
                    rendered.message_timestamp,
                    &tags,
                    &message,
                )
            }
        }
    }
}
