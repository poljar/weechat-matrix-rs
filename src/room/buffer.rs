use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc};

use futures_util::StreamExt;
use matrix_sdk::{
    deserialized_responses::SyncOrStrippedState,
    room::ParentSpace,
    ruma::{
        events::{space::child::SpaceChildEventContent, SyncStateEvent},
        EventId, OwnedEventId, OwnedRoomAliasId, OwnedRoomId, OwnedServerName,
        OwnedUserId, TransactionId, UserId,
    },
    Error, Room, RoomState,
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
    space_children: Rc<RefCell<Vec<SpaceChildInfo>>>,
}

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

impl RoomBuffer {
    pub fn new(room: SharedRoom, runtime: Handle) -> Self {
        Self {
            room,
            runtime,
            inner: Rc::new(RefCell::new(None)),
            thread_buffers: Rc::new(RefCell::new(HashMap::new())),
            space_children: Rc::new(RefCell::new(Vec::new())),
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

    pub fn short_names(&self) -> Vec<String> {
        let mut handles =
            self.inner.borrow().iter().cloned().collect::<Vec<_>>();
        handles.extend(self.thread_buffers.borrow().values().cloned());

        handles
            .into_iter()
            .filter_map(|handle| {
                let buffer = handle.upgrade().ok()?;
                Some(buffer.short_name().into_owned())
            })
            .collect()
    }

    pub fn buffer_handle_for_short_name(
        &self,
        short_name: &str,
    ) -> Option<BufferHandle> {
        if let Some(handle) = self.inner.borrow().as_ref() {
            if handle
                .upgrade()
                .is_ok_and(|buffer| buffer.short_name() == short_name)
            {
                return Some(handle.clone());
            }
        }

        self.thread_buffers
            .borrow()
            .values()
            .find(|handle| {
                handle
                    .upgrade()
                    .is_ok_and(|buffer| buffer.short_name() == short_name)
            })
            .cloned()
    }

    pub fn owns_buffer(&self, buffer: &Buffer) -> bool {
        if self.is_main_buffer(buffer) {
            return true;
        }

        self.thread_buffers
            .borrow()
            .values()
            .any(|handle| handle.upgrade().is_ok_and(|b| &b == buffer))
    }

    pub fn is_main_buffer(&self, buffer: &Buffer) -> bool {
        self.inner
            .borrow()
            .as_ref()
            .and_then(|b| b.upgrade().ok())
            .is_some_and(|b| &b == buffer)
    }

    pub fn thread_root_for_buffer(
        &self,
        buffer: &Buffer,
    ) -> Option<OwnedEventId> {
        self.thread_buffers
            .borrow()
            .iter()
            .find_map(|(thread_root, handle)| {
                handle
                    .upgrade()
                    .is_ok_and(|b| &b == buffer)
                    .then(|| thread_root.clone())
            })
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

    pub fn remove_thread_buffer_for_buffer(&self, buffer: &Buffer) -> bool {
        let thread_root = self.thread_root_for_buffer(buffer);

        if let Some(thread_root) = thread_root {
            self.remove_thread_buffer(&thread_root);
            true
        } else {
            false
        }
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

    /// Count printed lines after an event that is still visible in a buffer.
    pub fn reply_line_distance(&self, event_id: &EventId) -> Option<usize> {
        let event_id_tag = Cow::from(event_id.to_tag());

        self.buffer_handle()
            .upgrade()
            .ok()
            .and_then(|buffer| buffer_line_distance(&buffer, &event_id_tag))
            .or_else(|| {
                self.thread_buffers.borrow().values().find_map(|handle| {
                    handle.upgrade().ok().and_then(|buffer| {
                        buffer_line_distance(&buffer, &event_id_tag)
                    })
                })
            })
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

    /// Return encrypted event IDs represented by placeholder lines restored
    /// from WeeChat logs or currently displayed in a room/thread buffer.
    pub fn encrypted_event_ids(&self) -> Vec<OwnedEventId> {
        const ENCRYPTED_TAG: &str = "matrix_encrypted";
        const EVENT_TAG_PREFIX: &str = "matrix_id_";

        let mut event_ids = Vec::new();
        let mut handles =
            self.inner.borrow().iter().cloned().collect::<Vec<_>>();
        handles.extend(self.thread_buffers.borrow().values().cloned());

        for handle in handles {
            let Ok(buffer) = handle.upgrade() else {
                continue;
            };

            for line in buffer.lines() {
                let tags = line.tags();
                if !tags.iter().any(|tag| tag.as_ref() == ENCRYPTED_TAG) {
                    continue;
                }

                let Some(event_id) = tags.iter().find_map(|tag| {
                    tag.as_ref()
                        .strip_prefix(EVENT_TAG_PREFIX)
                        .and_then(|event_id| EventId::parse(event_id).ok())
                }) else {
                    continue;
                };

                if !event_ids.contains(&event_id) {
                    event_ids.push(event_id);
                }
            }
        }

        event_ids
    }

    /// Copy the root event line into a newly created thread buffer.
    pub fn seed_thread_buffer(
        &self,
        thread_root: &EventId,
        thread_buffer: &Buffer,
    ) -> bool {
        let event_id_tag = Cow::from(thread_root.to_tag());

        if buffer_contains_tag(thread_buffer, &event_id_tag) {
            return false;
        }

        self.buffer_handle().upgrade().is_ok_and(|buffer| {
            copy_buffer_line_by_tag(&buffer, thread_buffer, &event_id_tag)
        })
    }

    /// Copy a newly rendered root event into its already-open thread buffer.
    pub fn seed_open_thread_buffer(&self, thread_root: &EventId) -> bool {
        let Some(handle) = self.thread_buffer(thread_root) else {
            return false;
        };

        if let Ok(buffer) = handle.upgrade() {
            self.seed_thread_buffer(thread_root, &buffer)
        } else {
            self.remove_thread_buffer(thread_root);
            false
        }
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

        let _ = self.buffer_handle().upgrade().is_ok_and(|buffer| {
            self.replace_edit_in_buffer(
                &buffer,
                &event_id_tag,
                &sender_tag,
                &event,
            )
        });

        for handle in self.thread_buffers.borrow().values() {
            if let Ok(buffer) = handle.upgrade() {
                let replaced = self.replace_edit_in_buffer(
                    &buffer,
                    &event_id_tag,
                    &sender_tag,
                    &event,
                );

                if replaced
                    && self
                        .thread_root_for_buffer(&buffer)
                        .is_some_and(|thread_root| thread_root == event_id)
                {
                    sort_buffer_lines(&buffer, Some(event_id_tag.as_ref()));
                }
            }
        }
    }

    pub fn redact_event_lines<F, G>(
        &self,
        event_id_tag: &Cow<str>,
        redacted_tag: &Cow<str>,
        redact_first_line: F,
        redact_rest_line: G,
    ) -> bool
    where
        F: Fn(Cow<str>) -> String,
        G: Fn(Cow<str>) -> String,
    {
        let mut redacted = self.buffer_handle().upgrade().is_ok_and(|buffer| {
            redact_event_lines_in_buffer(
                &buffer,
                event_id_tag,
                redacted_tag,
                &redact_first_line,
                &redact_rest_line,
            )
        });

        for handle in self.thread_buffers.borrow().values() {
            redacted |= handle.upgrade().is_ok_and(|buffer| {
                redact_event_lines_in_buffer(
                    &buffer,
                    event_id_tag,
                    redacted_tag,
                    &redact_first_line,
                    &redact_rest_line,
                )
            });
        }

        redacted
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
            let tags: Vec<&str> = new.tags.iter().map(String::as_str).collect();
            line.set_tags(&tags);
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

                sort_buffer_lines(buffer, None)
            }
            Ordering::Equal => (),
        }
    }

    pub fn sort_messages(&self) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            sort_buffer_lines(&buffer, None);
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

    pub fn update_avatar(&self) {
        let Some(room) = maybe_active_room(&self.room) else {
            return;
        };

        if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.set_localvar(
                "matrix_avatar_mxc",
                room.avatar_url()
                    .as_ref()
                    .map(|uri| uri.as_str())
                    .unwrap_or_default(),
            );
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

    pub fn refresh_space_children(&self) {
        let Some(room) = maybe_active_room(&self.room) else {
            return;
        };

        if !room.is_space() {
            return;
        }

        match self.runtime.block_on(space_children(room)) {
            Ok(children) => {
                *self.space_children.borrow_mut() = children;
                self.render_space_children();
            }
            Err(error) => {
                Weechat::print(&format!(
                    "{}: Error fetching space children from the store: {}",
                    Weechat::prefix(Prefix::Error),
                    error,
                ));
            }
        }
    }

    /// Reuse the room buffer input as a live filter, like WeeChat's `/fset`.
    ///
    /// The stable numbers refer to the unfiltered hierarchy so narrowing and
    /// selecting never changes the target underneath the user.
    pub fn render_space_children(&self) {
        let buffer_handle = self.buffer_handle();
        let Ok(buffer) = buffer_handle.upgrade() else {
            return;
        };

        let input = buffer.input();
        let children = self.space_children.borrow();
        let visible = filter_space_children(&children, &input);

        buffer.clear();
        buffer.set_title(&format!(
            "Space rooms: showing {} of {}. Type to filter; press Enter on a \
             number or unique name to open/join.",
            visible.len(),
            children.len(),
        ));

        if visible.is_empty() {
            buffer.print("No rooms match this filter.");
            return;
        }

        for (index, child) in visible {
            let action = if child.joined { "open" } else { "join" };
            let kind = if child.is_space { "space" } else { "room" };

            buffer.print(&format!(
                "{:>3}  [{action} {kind}] {}  {}",
                index + 1,
                child.name,
                child.room_id,
            ));
        }
    }

    pub fn space_child_command(
        &self,
        input: &str,
        server_name: &str,
    ) -> SpaceChildSelection {
        select_space_child(&self.space_children.borrow(), input)
            .map_or(SpaceChildSelection::NoMatch, |child| {
                space_child_selection(server_name, child)
            })
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
        let is_space = room.is_space();

        let room_name = room
            .name()
            .as_deref()
            .and_then(non_empty_room_name)
            .or_else(|| {
                room.canonical_alias()
                    .and_then(|alias| non_empty_room_name(alias.alias()))
            })
            .or_else(|| {
                self.runtime
                    .block_on(room.display_name())
                    .ok()
                    .and_then(|name| non_empty_room_name(&name.to_string()))
            })
            .unwrap_or_else(|| room.room_id().to_string());

        format_buffer_name(&room_name, is_direct, is_space)
    }

    pub fn calculate_thread_buffer_name(
        &self,
        thread_root: &EventId,
    ) -> String {
        format_thread_buffer_name(thread_root)
    }

    pub fn thread_buffer_suffix(thread_root: &EventId) -> String {
        sanitize_thread_id(thread_root)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpaceChildInfo {
    room_id: OwnedRoomId,
    name: String,
    joined: bool,
    is_space: bool,
    order: Option<String>,
    via: Vec<OwnedServerName>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SpaceChildSelection {
    Open(String),
    Join {
        room_id: OwnedRoomId,
        via: Vec<OwnedServerName>,
    },
    NoMatch,
}

fn filter_space_children<'a>(
    children: &'a [SpaceChildInfo],
    input: &str,
) -> Vec<(usize, &'a SpaceChildInfo)> {
    let query = input.trim().to_lowercase();

    children
        .iter()
        .enumerate()
        .filter(|(_, child)| {
            query.is_empty()
                || child.name.to_lowercase().contains(&query)
                || child.room_id.as_str().to_lowercase().contains(&query)
        })
        .collect()
}

fn select_space_child<'a>(
    children: &'a [SpaceChildInfo],
    input: &str,
) -> Option<&'a SpaceChildInfo> {
    let input = input.trim();

    if input.is_empty() {
        return None;
    }

    if let Ok(index) = input.parse::<usize>() {
        return index.checked_sub(1).and_then(|index| children.get(index));
    }

    let matches = filter_space_children(children, input);

    if matches.len() == 1 {
        matches.first().map(|(_, child)| *child)
    } else {
        None
    }
}

fn space_child_selection(
    server_name: &str,
    child: &SpaceChildInfo,
) -> SpaceChildSelection {
    if child.joined {
        SpaceChildSelection::Open(format!(
            "/buffer {}.{}",
            server_name, child.room_id
        ))
    } else {
        SpaceChildSelection::Join {
            room_id: child.room_id.clone(),
            via: child.via.clone(),
        }
    }
}

fn buffer_contains_tag(buffer: &Buffer, tag: &Cow<str>) -> bool {
    buffer
        .lines()
        .rfind(|line| line.tags().contains(tag))
        .is_some()
}

fn copy_buffer_line_by_tag(
    source: &Buffer,
    target: &Buffer,
    tag: &Cow<str>,
) -> bool {
    let lines: Vec<LineCopy> = source
        .lines()
        .filter(|line| line.tags().contains(tag))
        .map(Into::into)
        .collect();

    if lines.is_empty() {
        return false;
    }

    for line in lines {
        let message = if line.prefix.is_empty() {
            line.message
        } else {
            format!("{}\t{}", line.prefix, line.message)
        };
        let tags: Vec<&str> = line.tags.iter().map(String::as_str).collect();

        target.print_date_tags(line.date, &tags, &message);
    }

    // A thread buffer may already contain the reply that caused its creation.
    // Keep every line of the triggering event first even when Matrix timestamps
    // have only second-level precision or the root arrived through a later sync.
    sort_buffer_lines(target, Some(tag.as_ref()));

    true
}

fn line_sort_key(
    date: isize,
    tags: &[String],
    priority_tag: Option<&str>,
) -> (bool, isize) {
    let is_priority =
        priority_tag.is_some_and(|tag| tags.iter().any(|item| item == tag));

    (!is_priority, date)
}

fn sort_buffer_lines(buffer: &Buffer, priority_tag: Option<&str>) {
    let mut lines: Vec<LineCopy> = buffer.lines().map(Into::into).collect();
    lines
        .sort_by_key(|line| line_sort_key(line.date, &line.tags, priority_tag));

    // TODO update the highlight once Weechat starts supporting it.
    for (line, new) in buffer.lines().zip(lines.drain(..)) {
        let tags = new.tags.iter().map(String::as_str).collect::<Vec<&str>>();
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

fn redact_event_lines_in_buffer<F, G>(
    buffer: &Buffer,
    event_id_tag: &Cow<str>,
    redacted_tag: &Cow<str>,
    redact_first_line: &F,
    redact_rest_line: &G,
) -> bool
where
    F: Fn(Cow<str>) -> String,
    G: Fn(Cow<str>) -> String,
{
    let predicate = |line: &BufferLine| {
        let tags = line.tags();
        tags.contains(event_id_tag) && !tags.contains(redacted_tag)
    };

    let mut lines = buffer.lines();
    let first_line = lines.rfind(predicate);

    if let Some(line) = first_line {
        modify_line(line, redacted_tag.clone(), redact_first_line);
    } else {
        return false;
    }

    while let Some(line) = lines.next_back().filter(predicate) {
        modify_line(line, redacted_tag.clone(), redact_rest_line);
    }

    true
}

fn modify_line<F>(line: BufferLine, tag: Cow<str>, redaction_func: F)
where
    F: Fn(Cow<str>) -> String,
{
    let message = line.message();
    let new_message = redaction_func(message);

    let mut tags = line.tags();
    tags.push(tag);
    let tags: Vec<&str> = tags.iter().map(|tag| tag.as_ref()).collect();

    line.set_message(&new_message);
    line.set_tags(&tags);
}

fn buffer_line_distance(buffer: &Buffer, tag: &Cow<str>) -> Option<usize> {
    let mut lines_after = 0;

    for line in buffer.lines().rev() {
        if line.tags().contains(tag) {
            return Some(lines_after);
        }

        lines_after += 1;
    }

    None
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

fn format_buffer_name(
    room_name: &str,
    is_direct: bool,
    is_space: bool,
) -> String {
    if is_direct || room_name.starts_with('!') {
        return room_name.to_owned();
    }

    if is_space {
        if room_name == "+" {
            return "++".to_owned();
        }

        return room_name
            .strip_prefix('#')
            .map(|name| format!("+{}", name))
            .unwrap_or_else(|| {
                if room_name.starts_with('+') {
                    room_name.to_owned()
                } else {
                    format!("+{}", room_name)
                }
            });
    }

    let room_name = if room_name == "#" {
        "##".to_owned()
    } else if room_name.starts_with('#') {
        room_name.to_owned()
    } else {
        format!("#{}", room_name)
    };

    room_name
}

fn format_thread_buffer_name(thread_root: &EventId) -> String {
    format!("thread.{}", RoomBuffer::thread_buffer_suffix(thread_root))
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
        self.replace_event(event_id, event);
    }

    pub fn replace_event(
        &self,
        event_id: &EventId,
        event: RenderedEvent,
    ) -> bool {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            let event_id_tag = Cow::from(event_id.to_tag());

            let lines: Vec<BufferLine> = buffer
                .lines()
                .filter(|l| l.tags().contains(&event_id_tag))
                .collect();

            if !lines.is_empty() {
                self.replace_event_helper(&buffer, lines, &event);
                return true;
            }
        }

        self.thread_buffers.borrow().values().any(|handle| {
            handle.upgrade().is_ok_and(|buffer| {
                let event_id_tag = Cow::from(event_id.to_tag());
                let lines: Vec<BufferLine> = buffer
                    .lines()
                    .filter(|line| line.tags().contains(&event_id_tag))
                    .collect();

                if lines.is_empty() {
                    false
                } else {
                    self.replace_event_helper(&buffer, lines, &event);
                    true
                }
            })
        })
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
    use matrix_sdk::ruma::{event_id, room_id, user_id};

    use super::{
        filter_space_children, format_buffer_name, format_thread_buffer_name,
        line_sort_key, reply_sender_id_from_tags, sanitize_thread_id,
        select_space_child, space_child_selection, RoomBuffer, SpaceChildInfo,
        SpaceChildSelection,
    };

    fn space_child(
        room_id: &matrix_sdk::ruma::RoomId,
        name: &str,
        joined: bool,
    ) -> SpaceChildInfo {
        SpaceChildInfo {
            room_id: room_id.to_owned(),
            name: name.to_owned(),
            joined,
            is_space: false,
            order: None,
            via: vec!["example.org".parse().unwrap()],
        }
    }

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
    fn thread_root_lines_sort_before_existing_replies() {
        let root_tag = "matrix_id_$root:example.org";
        let root_tags = vec![root_tag.to_owned(), "matrix_text".to_owned()];
        let reply_tags = vec!["matrix_text".to_owned()];

        assert!(
            line_sort_key(200, &root_tags, Some(root_tag))
                < line_sort_key(100, &reply_tags, Some(root_tag))
        );
    }

    #[test]
    fn thread_root_wins_same_timestamp_ties() {
        let root_tag = "matrix_id_$root:example.org";
        let root_tags = vec![root_tag.to_owned()];
        let reply_tags = vec!["matrix_text".to_owned()];

        assert!(
            line_sort_key(100, &root_tags, Some(root_tag))
                < line_sort_key(100, &reply_tags, Some(root_tag))
        );
    }

    #[test]
    fn ordinary_lines_remain_chronological() {
        let tags = vec!["matrix_text".to_owned()];

        assert!(
            line_sort_key(100, &tags, None) < line_sort_key(200, &tags, None)
        );
    }

    #[test]
    fn preserves_named_channel_prefix() {
        assert_eq!(format_buffer_name("OSGeo", false, false), "#OSGeo");
    }

    #[test]
    fn preserves_direct_room_names() {
        assert_eq!(format_buffer_name("Alice", true, false), "Alice");
    }

    #[test]
    fn preserves_alias_like_names() {
        assert_eq!(format_buffer_name("#lounge", false, false), "#lounge");
        assert_eq!(format_buffer_name("#", false, false), "##");
    }

    #[test]
    fn preserves_room_id_fallbacks() {
        assert_eq!(
            format_buffer_name("!roomid:matrix.osgeo.org", false, false),
            "!roomid:matrix.osgeo.org"
        );
    }

    #[test]
    fn marks_space_buffers_with_plus_prefix() {
        assert_eq!(format_buffer_name("OSGeo", false, true), "+OSGeo");
        assert_eq!(format_buffer_name("#OSGeo", false, true), "+OSGeo");
        assert_eq!(format_buffer_name("+OSGeo", false, true), "+OSGeo");
        assert_eq!(format_buffer_name("+", false, true), "++");
    }

    #[test]
    fn filters_space_children_by_name_or_room_id() {
        let children = [
            space_child(room_id!("!alpha:example.org"), "Alpha", true),
            space_child(room_id!("!beta:example.org"), "Release room", false),
        ];

        let by_name = filter_space_children(&children, "RELEASE");
        let by_id = filter_space_children(&children, "!ALPHA");

        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].1.room_id, room_id!("!beta:example.org"));
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].1.room_id, room_id!("!alpha:example.org"));
    }

    #[test]
    fn selects_space_child_by_number_or_unique_filter() {
        let children = [
            space_child(room_id!("!alpha:example.org"), "Alpha", true),
            space_child(room_id!("!alpine:example.org"), "Alpine", false),
            space_child(room_id!("!beta:example.org"), "Beta", false),
        ];

        assert_eq!(
            select_space_child(&children, "2")
                .map(|child| child.room_id.as_ref()),
            Some(room_id!("!alpine:example.org")),
        );
        assert_eq!(
            select_space_child(&children, "bet")
                .map(|child| child.room_id.as_ref()),
            Some(room_id!("!beta:example.org")),
        );
        assert!(select_space_child(&children, "alp").is_none());
    }

    #[test]
    fn opens_joined_space_children_and_joins_other_children() {
        let joined =
            space_child(room_id!("!joined:example.org"), "Joined", true);
        let left = space_child(room_id!("!left:example.org"), "Left", false);

        assert_eq!(
            space_child_selection("matrix", &joined),
            SpaceChildSelection::Open(
                "/buffer matrix.!joined:example.org".to_owned()
            ),
        );
        assert_eq!(
            space_child_selection("matrix", &left),
            SpaceChildSelection::Join {
                room_id: room_id!("!left:example.org").to_owned(),
                via: vec!["example.org".parse().unwrap()],
            },
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

    #[test]
    fn thread_buffer_name_uses_thread_prefix_and_short_suffix() {
        assert_eq!(
            format_thread_buffer_name(event_id!(
                "$abcdef0123456789:example.org"
            )),
            "thread.abcdef012345"
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

async fn space_children(room: Room) -> Result<Vec<SpaceChildInfo>, Error> {
    let client = room.client();
    let mut children = Vec::new();

    for raw_event in room
        .get_state_events_static::<SpaceChildEventContent>()
        .await?
    {
        let event =
            match raw_event.deserialize() {
                Ok(SyncOrStrippedState::Sync(SyncStateEvent::Original(
                    event,
                ))) if !event.content.via.is_empty() => event,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(
                        room_id = ?room.room_id(),
                        "Could not deserialize m.space.child: {error}"
                    );
                    continue;
                }
            };

        let room_id = event.state_key;
        let order = event.content.order.map(|order| order.to_string());

        let (name, joined, is_space) =
            if let Some(child_room) = client.get_room(&room_id) {
                let joined = child_room.state() == RoomState::Joined;
                let is_space = child_room.is_space();
                let name = child_room
                    .display_name()
                    .await
                    .map(|name| name.to_string())
                    .unwrap_or_else(|_| room_id.to_string());

                (name, joined, is_space)
            } else {
                // The local state store knows the hierarchy relation but not a
                // summary for rooms we have never joined. The room ID remains
                // a useful, selectable fallback.
                (room_id.to_string(), false, false)
            };

        children.push(SpaceChildInfo {
            room_id,
            name,
            joined,
            is_space,
            order,
            via: event.content.via,
        });
    }

    children.sort_by(|left, right| {
        left.order
            .is_none()
            .cmp(&right.order.is_none())
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| {
                left.name.to_lowercase().cmp(&right.name.to_lowercase())
            })
            .then_with(|| left.room_id.cmp(&right.room_id))
    });

    Ok(children)
}
