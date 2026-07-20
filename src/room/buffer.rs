use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc};

use futures_util::StreamExt;
use matrix_sdk::{
    room::ParentSpace,
    ruma::{
        EventId, OwnedEventId, OwnedRoomAliasId, OwnedUserId, TransactionId,
        UserId,
    },
    Error, Room,
};
use tokio::runtime::Handle;
use weechat::{
    buffer::{Buffer, BufferHandle, BufferLine, LineData},
    Prefix, Weechat,
};

use crate::{render::RenderedEvent, utils::ToTag};

use super::{maybe_active_room, SharedRoom};

#[derive(Clone)]
pub struct RoomBuffer {
    room: SharedRoom,
    runtime: Handle,
    pub(super) inner: Rc<RefCell<Option<BufferHandle>>>,
    thread_buffers: Rc<RefCell<HashMap<OwnedEventId, BufferHandle>>>,
}

impl RoomBuffer {
    pub fn new(room: SharedRoom, runtime: Handle) -> Self {
        Self {
            room,
            runtime,
            inner: Rc::new(RefCell::new(None)),
            thread_buffers: Rc::new(RefCell::new(HashMap::new())),
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

    pub fn owns_buffer(&self, buffer: &Buffer) -> bool {
        if self
            .inner
            .borrow()
            .as_ref()
            .and_then(|b| b.upgrade().ok())
            .is_some_and(|b| &b == buffer)
        {
            return true;
        }

        self.thread_buffers
            .borrow()
            .values()
            .any(|handle| handle.upgrade().is_ok_and(|b| &b == buffer))
    }

    pub fn thread_buffer(&self, thread_root: &EventId) -> Option<BufferHandle> {
        self.thread_buffers.borrow().get(thread_root).cloned()
    }

    pub fn set_thread_buffer(
        &self,
        thread_root: OwnedEventId,
        handle: BufferHandle,
    ) {
        self.thread_buffers.borrow_mut().insert(thread_root, handle);
    }

    pub fn remove_thread_buffer(&self, thread_root: &EventId) {
        self.thread_buffers.borrow_mut().remove(thread_root);
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

    /// Return whether an event is already rendered in the buffer.
    pub fn contains_event(&self, event_id: &EventId) -> bool {
        let event_id_tag = Cow::from(event_id.to_tag());

        let main_buffer_contains_event = self
            .buffer_handle()
            .upgrade()
            .is_ok_and(|buffer| buffer_contains_tag(&buffer, &event_id_tag));

        main_buffer_contains_event
            || self.thread_buffers.borrow().values().any(|handle| {
                handle.upgrade().is_ok_and(|buffer| {
                    buffer_contains_tag(&buffer, &event_id_tag)
                })
            })
    }

    /// Replace the local echo of an event with a fully rendered one.
    pub fn replace_local_echo(
        &self,
        transaction_id: &TransactionId,
        rendered: RenderedEvent,
    ) {
        let uuid_tag = Cow::from(format!("matrix_echo_{}", transaction_id));

        if self.buffer_handle().upgrade().is_ok_and(|buffer| {
            replace_local_echo_in_buffer(&buffer, &uuid_tag, &rendered)
        }) {
            return;
        }

        self.thread_buffers.borrow().values().any(|handle| {
            handle.upgrade().is_ok_and(|buffer| {
                replace_local_echo_in_buffer(&buffer, &uuid_tag, &rendered)
            })
        });
    }

    pub fn replace_edit(
        &self,
        event_id: &EventId,
        sender: &UserId,
        event: RenderedEvent,
    ) {
        let sender_tag = Cow::from(sender.to_tag());
        let event_id_tag = Cow::from(event_id.to_tag());

        if self.buffer_handle().upgrade().is_ok_and(|buffer| {
            self.replace_edit_in_buffer(
                &buffer,
                &event_id_tag,
                &sender_tag,
                &event,
            )
        }) {
            return;
        }

        self.thread_buffers.borrow().values().any(|handle| {
            handle.upgrade().is_ok_and(|buffer| {
                self.replace_edit_in_buffer(
                    &buffer,
                    &event_id_tag,
                    &sender_tag,
                    &event,
                )
            })
        });
    }

    fn replace_edit_in_buffer(
        &self,
        buffer: &Buffer,
        event_id_tag: &Cow<str>,
        sender_tag: &Cow<str>,
        event: &RenderedEvent,
    ) -> bool {
        let lines: Vec<BufferLine> = buffer
            .lines()
            .filter(|l| l.tags().contains(event_id_tag))
            .collect();

        if lines
            .get(0)
            .map(|l| l.tags().contains(sender_tag))
            .unwrap_or(false)
        {
            self.replace_event_helper(buffer, lines, event);
            true
        } else {
            false
        }
    }

    fn replace_event_helper(
        &self,
        buffer: &Buffer,
        lines: Vec<BufferLine<'_>>,
        event: &RenderedEvent,
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
        let Some(room) = maybe_active_room(&self.room) else {
            return;
        };

        if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.set_title(&room.topic().unwrap_or_default());
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
        let Some(room) = maybe_active_room(&self.room) else {
            return;
        };

        let spaces = self.runtime.block_on(parent_spaces(room));

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
        maybe_active_room(&self.room).and_then(|room| room.canonical_alias())
    }

    pub fn calculate_buffer_name(&self) -> String {
        let Some(room) = maybe_active_room(&self.room) else {
            return self.short_name();
        };
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

    pub fn calculate_thread_buffer_name(
        &self,
        thread_root: &EventId,
    ) -> String {
        format!(
            "{}.thread.{}",
            self.calculate_buffer_name(),
            Self::thread_buffer_suffix(thread_root)
        )
    }

    pub fn thread_buffer_suffix(thread_root: &EventId) -> String {
        sanitize_thread_id(thread_root)
    }
}

fn buffer_contains_tag(buffer: &Buffer, tag: &Cow<str>) -> bool {
    buffer
        .lines()
        .rfind(|line| line.tags().contains(tag))
        .is_some()
}

fn sanitize_thread_id(thread_root: &EventId) -> String {
    const THREAD_ID_DISPLAY_CHARS: usize = 12;

    let mut sanitized: String = thread_root
        .as_str()
        .trim_start_matches('$')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(THREAD_ID_DISPLAY_CHARS)
        .collect();

    if sanitized.is_empty() {
        sanitized.push_str("unknown");
    }

    sanitized
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

            self.replace_event_helper(&buffer, lines, &event);
        }
    }

    pub fn print_rendered_event(&self, rendered: RenderedEvent) {
        let buffer = self.buffer_handle();

        if let Ok(buffer) = buffer.upgrade() {
            print_rendered_event_to_buffer(&buffer, rendered);
        }
    }
}

pub fn print_rendered_event_to_buffer(
    buffer: &Buffer,
    rendered: RenderedEvent,
) {
    for line in rendered.content.lines {
        let message = format!("{}{}", &rendered.prefix, &line.message);
        let tags: Vec<&str> = line.tags.iter().map(|t| t.as_str()).collect();
        buffer.print_date_tags(rendered.message_timestamp, &tags, &message)
    }
}

fn replace_local_echo_in_buffer(
    buffer: &Buffer,
    uuid_tag: &Cow<str>,
    rendered: &RenderedEvent,
) -> bool {
    let line_contains_uuid = |line: &BufferLine| line.tags().contains(uuid_tag);
    let mut lines = buffer.lines();
    let mut current_line = lines.rfind(line_contains_uuid);
    let mut replaced = false;

    // We go from bottom to top since local echoes are expected to be fresh.
    let mut line_num = rendered.content.lines.len();

    while let Some(line) = &current_line {
        line_num -= 1;
        let rendered_line = &rendered.content.lines[line_num];
        let tags: Vec<&str> =
            rendered_line.tags.iter().map(String::as_str).collect();

        line.set_message(&rendered_line.message);
        line.set_tags(&tags);
        replaced = true;
        current_line = lines.next_back().filter(line_contains_uuid);
    }

    replaced
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{event_id, user_id};

    use super::{
        format_buffer_name, reply_sender_id_from_tags, sanitize_thread_id,
        RoomBuffer,
    };

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

    #[test]
    fn sanitizes_thread_event_id_for_buffer_name() {
        assert_eq!(
            sanitize_thread_id(event_id!("$abc/def:example.org")),
            "abc_def_exam"
        );
    }

    #[test]
    fn thread_buffer_suffix_uses_short_human_id() {
        assert_eq!(
            RoomBuffer::thread_buffer_suffix(event_id!(
                "$abcdef0123456789:example.org"
            )),
            "abcdef012345"
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
