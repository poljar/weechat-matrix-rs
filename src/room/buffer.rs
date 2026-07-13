use std::{borrow::Cow, cell::RefCell, rc::Rc};

use futures_util::StreamExt;
use matrix_sdk::{
    room::ParentSpace,
    ruma::{EventId, OwnedRoomAliasId, OwnedUserId, TransactionId, UserId},
    Error, Room,
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

    /// Return the sender ID for an event that is still in the buffer.
    ///
    /// Reply rendering uses this local lookup so it never blocks on a
    /// homeserver request. Callers retain the event-id fallback when the target
    /// line is no longer available.
    pub fn reply_sender_id(&self, event_id: &EventId) -> Option<OwnedUserId> {
        let buffer_handle = self.buffer_handle();
        let buffer = buffer_handle.upgrade().ok()?;
        let event_id_tag = Cow::from(event_id.to_tag());
        let line = buffer
            .lines()
            .rfind(|line| line.tags().contains(&event_id_tag))?;

        reply_sender_id_from_tags(&line.tags())
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
                let tags: Vec<&str> =
                    rendered_line.tags.iter().map(String::as_str).collect();

                line.set_message(&rendered_line.message);
                line.set_tags(&tags);
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
            date: isize,
            date_printed: isize,
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

    pub fn update_parent_spaces(&self) {
        let spaces = self.runtime.block_on(parent_spaces(self.room.clone()));

        if let Ok(buffer) = self.buffer_handle().upgrade() {
            match spaces {
                Ok(spaces) => {
                    let ids = spaces
                        .iter()
                        .map(|s| s.id.as_str())
                        .collect::<Vec<_>>()
                        .join(",");
                    let names = spaces
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(",");

                    buffer.set_localvar("space_ids", &ids);
                    buffer.set_localvar("spaces", &names);

                    if let Some(space) = spaces.first() {
                        buffer.set_localvar("space_id", space.id.as_str());
                        buffer.set_localvar("space", &space.name);
                    } else {
                        buffer.set_localvar("space_id", "");
                        buffer.set_localvar("space", "");
                    }
                }
                Err(e) => {
                    Weechat::print(&format!(
                        "{}: Error fetching parent spaces from the store: {}",
                        Weechat::prefix(Prefix::Error),
                        e,
                    ));
                }
            }
        }
    }

    fn alias(&self) -> Option<OwnedRoomAliasId> {
        self.room.canonical_alias()
    }

    pub fn calculate_buffer_name(&self) -> String {
        let room = self.room.clone();
        let is_direct =
            self.runtime.block_on(room.is_direct()).unwrap_or(false);

        let room_name = room
            .name()
            .as_deref()
            .and_then(non_empty_room_name)
            .or_else(|| {
                room.canonical_alias()
                    .and_then(|alias| non_empty_room_name(alias.alias()))
            })
            .or_else(|| {
                is_direct
                    .then(|| self.runtime.block_on(room.display_name()).ok())
                    .flatten()
                    .and_then(|name| non_empty_room_name(&name.to_string()))
            })
            .unwrap_or_else(|| room.room_id().to_string());

        format_buffer_name(&room_name, is_direct)
    }
}

fn reply_sender_id_from_tags<T: AsRef<str>>(tags: &[T]) -> Option<OwnedUserId> {
    tags.iter().find_map(|tag| {
        tag.as_ref()
            .strip_prefix("matrix_sender_")
            .and_then(|sender| UserId::parse(sender).ok())
    })
}

fn non_empty_room_name(name: &str) -> Option<String> {
    let name = name.trim();

    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

fn format_buffer_name(room_name: &str, is_direct: bool) -> String {
    let room_name = if room_name == "#" {
        "##".to_owned()
    } else if room_name.starts_with('#')
        || room_name.starts_with('!')
        || is_direct
    {
        room_name.to_owned()
    } else {
        format!("#{}", room_name)
    };

    room_name
}

impl RoomBuffer {
    pub fn update_buffer_name(&self) {
        let buffer = self.buffer_handle();

        let buffer = if let Ok(b) = buffer.upgrade() {
            b
        } else {
            return;
        };

        buffer.set_short_name(&self.calculate_buffer_name());
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

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::user_id;

    use super::{format_buffer_name, reply_sender_id_from_tags};

    #[test]
    fn reply_sender_uses_matrix_sender_tag() {
        let tags = ["matrix_text", "matrix_sender_@alice:example.org"];

        assert_eq!(
            Some(user_id!("@alice:example.org").to_owned()),
            reply_sender_id_from_tags(&tags)
        );
    }

    #[test]
    fn reply_sender_is_unknown_without_matrix_sender_tag() {
        let tags = ["matrix_text", "matrix_reply"];

        assert_eq!(None, reply_sender_id_from_tags(&tags));
    }

    #[test]
    fn preserves_named_channel_prefix() {
        assert_eq!(format_buffer_name("OSGeo", false), "#OSGeo");
    }

    #[test]
    fn preserves_direct_room_names() {
        assert_eq!(format_buffer_name("Alice", true), "Alice");
    }

    #[test]
    fn preserves_alias_like_names() {
        assert_eq!(format_buffer_name("#lounge", false), "#lounge");
        assert_eq!(format_buffer_name("#", false), "##");
    }

    #[test]
    fn preserves_room_id_fallbacks() {
        assert_eq!(
            format_buffer_name("!roomid:matrix.osgeo.org", false),
            "!roomid:matrix.osgeo.org"
        );
    }
}

struct ParentSpaceInfo {
    id: String,
    name: String,
}

async fn parent_spaces(room: Room) -> Result<Vec<ParentSpaceInfo>, Error> {
    let mut stream = room.parent_spaces().await?;
    let mut spaces = Vec::new();

    while let Some(parent) = stream.next().await {
        match parent? {
            ParentSpace::Reciprocal(room)
            | ParentSpace::WithPowerlevel(room)
            | ParentSpace::Illegitimate(room) => {
                let id = room.room_id().to_string();
                let name = room
                    .display_name()
                    .await
                    .map(|name| name.to_string())
                    .unwrap_or_else(|_| id.clone());

                spaces.push(ParentSpaceInfo { id, name });
            }
            ParentSpace::Unverifiable(room_id) => {
                let id = room_id.to_string();
                spaces.push(ParentSpaceInfo {
                    id: id.clone(),
                    name: id,
                });
            }
        }
    }

    Ok(spaces)
}
