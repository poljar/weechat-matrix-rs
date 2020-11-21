//! Room buffer module.
//!
//! This module implements creates buffers that processes and prints out all the
//! user visible events
//!
//! Care should be taken when handling events. Events can be state events or
//! timeline events and they can come from a sync response or from a room
//! messages response.
//!
//! Events coming from a sync response and are part of the timeline need to be
//! printed out and they need to change the buffer state (e.g. when someone
//! joins, they need to be added to the nicklist).
//!
//! Events coming from a sync response and are part of the room state only need
//! to change the buffer state.
//!
//! Events coming from a room messages response, meaning they are old events,
//! should never change the room state. They only should be printed out.
//!
//! Care should be taken to model this in a way that event formatting methods
//! are pure functions so they can be reused e.g. if we print messages that
//! we're sending ourselves before we receive them in a sync response, or if we
//! decrypt a previously undecryptable event.

mod members;

use members::Members;
pub use members::WeechatRoomMember;

pub const BUFFER_CLOSED_ERROR: &str =
    "Buffer got closed but Room is still lingering around";

use std::{
    borrow::Cow,
    cell::RefCell,
    cmp::Reverse,
    collections::HashMap,
    ops::Deref,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
    time::{Instant, SystemTime},
};

use async_trait::async_trait;
use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use matrix_sdk::{
    events::{
        room::{
            message::{MessageEventContent, TextMessageEventContent},
            redaction::SyncRedactionEvent,
        },
        AnyMessageEventContent, AnyPossiblyRedactedSyncMessageEvent,
        AnyRedactedSyncMessageEvent, AnyRoomEvent, AnySyncMessageEvent,
        AnySyncRoomEvent, AnySyncStateEvent, SyncMessageEvent,
    },
    identifiers::{EventId, RoomId, UserId},
    uuid::Uuid,
    Room,
};

use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferHandle, BufferInputCallbackAsync,
        BufferLine, LineData,
    },
    Weechat,
};

use crate::{
    config::{Config, RedactionStyle},
    connection::{Connection, TYPING_NOTICE_TIMEOUT},
    render::{Render, RenderedEvent},
    PLUGIN_NAME,
};

#[derive(Clone)]
pub struct RoomHandle {
    inner: MatrixRoom,
    buffer_handle: BufferHandle,
}

impl Deref for RoomHandle {
    type Target = MatrixRoom;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Clone, Debug)]
struct IntMutex {
    inner: Rc<Mutex<Rc<AtomicBool>>>,
    locked: Rc<AtomicBool>,
}

struct IntMutexGuard<'a> {
    inner: MutexGuard<'a, Rc<AtomicBool>>,
}

impl<'a> Drop for IntMutexGuard<'a> {
    fn drop(&mut self) {
        self.inner.store(false, Ordering::SeqCst)
    }
}

impl IntMutex {
    fn new() -> Self {
        let locked = Rc::new(AtomicBool::from(false));
        let inner = Rc::new(Mutex::new(locked.clone()));

        Self { inner, locked }
    }

    fn locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    fn try_lock(&self) -> Result<IntMutexGuard<'_>, ()> {
        match self.inner.try_lock() {
            Ok(guard) => {
                guard.store(true, Ordering::SeqCst);

                Ok(IntMutexGuard { inner: guard })
            }
            Err(_) => Err(()),
        }
    }
}

#[derive(Clone)]
pub struct MatrixRoom {
    homeserver: Rc<Url>,
    room_id: Rc<RoomId>,
    own_user_id: Rc<UserId>,
    room: Room,
    buffer: Rc<Option<BufferHandle>>,

    config: Rc<RefCell<Config>>,
    connection: Rc<RefCell<Option<Connection>>>,

    typing_notice_time: Rc<RefCell<Option<Instant>>>,
    typing_in_flight: Rc<Mutex<()>>,

    messages_in_flight: IntMutex,
    prev_batch: Rc<RefCell<Option<String>>>,

    outgoing_messages: MessageQueue,

    members: Members,
}

#[derive(Debug, Clone, Default)]
pub struct MessageQueue {
    queue: Rc<RefCell<HashMap<Uuid, (bool, MessageEventContent)>>>,
}

impl MessageQueue {
    fn new() -> Self {
        Self {
            queue: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn add(&self, uuid: Uuid, content: MessageEventContent) {
        self.queue.borrow_mut().insert(uuid, (false, content));
    }

    fn add_with_echo(&self, uuid: Uuid, content: MessageEventContent) {
        self.queue.borrow_mut().insert(uuid, (true, content));
    }

    fn remove(&self, uuid: Uuid) -> Option<(bool, MessageEventContent)> {
        self.queue.borrow_mut().remove(&uuid)
    }
}

impl RoomHandle {
    pub fn new(
        connection: &Rc<RefCell<Option<Connection>>>,
        config: Rc<RefCell<Config>>,
        room: Room,
        homeserver: &Url,
        room_id: RoomId,
        own_user_id: &UserId,
    ) -> Self {
        let members = Members::new(room.clone());

        let mut room = MatrixRoom {
            homeserver: Rc::new(homeserver.clone()),
            room_id: Rc::new(room_id.clone()),
            connection: connection.clone(),
            typing_notice_time: Rc::new(RefCell::new(None)),
            typing_in_flight: Rc::new(Mutex::new(())),
            config,
            prev_batch: Rc::new(RefCell::new(room.last_prev_batch())),
            own_user_id: Rc::new(own_user_id.to_owned()),
            members: members.clone(),
            buffer: members.buffer,
            outgoing_messages: MessageQueue::new(),
            messages_in_flight: IntMutex::new(),
            room,
        };

        let buffer_handle = BufferBuilderAsync::new(&room_id.to_string())
            .input_callback(room.clone())
            .close_callback(|_weechat: &Weechat, _buffer: &Buffer| {
                // TODO remove the roombuffer from the server here.
                // TODO leave the room if the plugin isn't unloading.
                Ok(())
            })
            .build()
            .expect("Can't create new room buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created buffer");

        buffer.enable_nicklist();

        // This is fine since we're only given the room to the buffer input and
        // the callback can only run once we yield controll back to Weechat.
        unsafe {
            *Rc::get_mut_unchecked(&mut room.members.buffer) =
                Some(buffer_handle.clone());
        }

        Self {
            inner: room,
            buffer_handle,
        }
    }

    pub async fn restore(
        room: Room,
        connection: &Rc<RefCell<Option<Connection>>>,
        config: Rc<RefCell<Config>>,
        homeserver: &Url,
    ) -> Self {
        todo!();
        // let room_clone = room.clone();
        // let room_id = room_lock.room_id.to_owned();
        // let own_user_id = &room_lock.own_user_id;

        // let room_buffer = Self::new(
        //     connection,
        //     config,
        //     room_clone,
        //     homeserver,
        //     room_id,
        //     own_user_id,
        // );

        // debug!("Restoring room {}", room_lock.room_id);

        // let matrix_members = room_lock
        //     .joined_members
        //     .values()
        //     .chain(room_lock.invited_members.values());

        // for member in matrix_members {
        //     let display_name = room_lock
        //         .get_member(&member.user_id)
        //         .unwrap()
        //         .display_name
        //         .clone();

        //     trace!("Restoring member {}", member.user_id);
        //     let member = WeechatRoomMember::new(
        //         &member.user_id,
        //         member.disambiguated_name(),
        //         display_name,
        //     );

        //     room_buffer.members.add(member);
        // }

        // room_buffer.update_buffer_name();

        // room_buffer
    }
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, _: BufferHandle, input: String) {
        // TODO parse the input here and produce a formatted body.
        let content = MessageEventContent::Text(TextMessageEventContent {
            body: input,
            formatted: None,
            relates_to: None,
        });

        self.send_message(content).await;
    }
}

impl MatrixRoom {
    fn room(&self) -> &Room {
        &self.room
    }

    pub fn is_encrypted(&self) -> bool {
        self.room.is_encrypted()
    }

    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    pub fn buffer_handle(&self) -> BufferHandle {
        (&*self.buffer)
            .as_ref()
            .expect("Room struct wasn't initialized properly")
            .clone()
    }

    fn print_rendered_event(&self, rendered: RenderedEvent) {
        let buffer = self.buffer_handle();

        if let Ok(buffer) = buffer.upgrade() {
            for line in rendered.content.lines {
                let message =
                    format!("{}\t{}", &rendered.prefix, &line.message);
                let tags: Vec<&str> =
                    line.tags.iter().map(|t| t.as_str()).collect();
                buffer.print_date_tags(0, &tags, &message)
            }
        }
    }

    fn redact_event(&self, event: &SyncRedactionEvent) {
        let buffer_handle = self.buffer_handle();

        let buffer = if let Ok(b) = buffer_handle.upgrade() {
            b
        } else {
            return;
        };

        // TODO remove this unwrap.
        let redacter = self.members.get(&event.sender).unwrap();

        let event_id_tag =
            Cow::from(format!("{}_id_{}", PLUGIN_NAME, event.redacts));
        let tag = Cow::from("matrix_redacted");

        let reason = if let Some(r) = &event.content.reason {
            format!(", reason: {}", r)
        } else {
            "".to_owned()
        };
        let redaction_message = format!(
            "{}<{}Message redacted by: {}{}{}>{}",
            Weechat::color("chat_delimiters"),
            Weechat::color("logger.color.backlog_line"),
            redacter.nick.borrow(),
            reason,
            Weechat::color("chat_delimiters"),
            Weechat::color("reset"),
        );

        let redaction_style = self.config.borrow().look().redaction_style();

        let predicate = |l: &BufferLine| {
            let tags = l.tags();
            tags.contains(&event_id_tag)
                && !tags.contains(&Cow::from("matrix_redacted"))
        };

        let strike_through = |string: Cow<str>| {
            Weechat::remove_color(&string)
                .graphemes(true)
                .map(|g| format!("{}\u{0336}", g))
                .collect::<Vec<String>>()
                .join("")
        };

        let redact_first_line = |message: Cow<str>| match redaction_style {
            RedactionStyle::Delete => redaction_message.clone(),
            RedactionStyle::Notice => {
                format!("{} {}", message, redaction_message)
            }
            RedactionStyle::StrikeThrough => {
                format!("{} {}", strike_through(message), redaction_message)
            }
        };

        let redact_string = |message: Cow<str>| match redaction_style {
            RedactionStyle::Delete => redaction_message.clone(),
            RedactionStyle::Notice => {
                format!("{} {}", message, redaction_message)
            }
            RedactionStyle::StrikeThrough => strike_through(message),
        };

        fn modify_line<F>(line: BufferLine, tag: Cow<str>, redaction_func: F)
        where
            F: Fn(Cow<str>) -> String,
        {
            let message = line.message();
            let new_message = redaction_func(message);

            let mut tags = line.tags();
            tags.push(tag);
            let tags: Vec<&str> = tags.iter().map(|t| t.as_ref()).collect();

            line.set_message(&new_message);
            line.set_tags(&tags);
        };

        let mut lines = buffer.lines();
        let first_line = lines.rfind(predicate);

        if let Some(line) = first_line {
            modify_line(line, tag.clone(), redact_first_line);
        } else {
            return;
        }

        while let Some(line) = lines.next_back().filter(predicate) {
            modify_line(line, tag.clone(), redact_string);
        }
    }

    async fn render_message_content(
        &self,
        event_id: &EventId,
        send_time: &SystemTime,
        sender: &WeechatRoomMember,
        content: &AnyMessageEventContent,
    ) -> Option<RenderedEvent> {
        use AnyMessageEventContent::*;
        use MessageEventContent::*;

        let rendered = match content {
            RoomEncrypted(c) => {
                c.render_with_prefix(send_time, event_id, sender, &())
            }
            RoomMessage(c) => match c {
                Text(c) => {
                    c.render_with_prefix(send_time, event_id, sender, &())
                }
                Emote(c) => {
                    c.render_with_prefix(send_time, event_id, &sender, &sender)
                }
                Notice(c) => {
                    c.render_with_prefix(send_time, event_id, &sender, &sender)
                }
                ServerNotice(c) => {
                    c.render_with_prefix(send_time, event_id, &sender, &sender)
                }
                Location(c) => {
                    c.render_with_prefix(send_time, event_id, &sender, &sender)
                }
                Audio(c) => c.render_with_prefix(
                    send_time,
                    event_id,
                    &sender,
                    &self.homeserver,
                ),
                Video(c) => c.render_with_prefix(
                    send_time,
                    event_id,
                    &sender,
                    &self.homeserver,
                ),
                File(c) => c.render_with_prefix(
                    send_time,
                    event_id,
                    &sender,
                    &self.homeserver,
                ),
                Image(c) => c.render_with_prefix(
                    send_time,
                    event_id,
                    &sender,
                    &self.homeserver,
                ),
            },
            _ => return None,
        };

        Some(rendered)
    }

    async fn render_sync_message(
        &self,
        event: &AnySyncMessageEvent,
    ) -> Option<RenderedEvent> {
        // TODO remove this expect.
        let sender = self
            .members
            .get(event.sender())
            .expect("Rendering a message but the sender isn't in the nicklist");

        let send_time = event.origin_server_ts();
        self.render_message_content(
            event.event_id(),
            send_time,
            &sender,
            &event.content(),
        )
        .await
    }

    // Add the content of the message to our outgoing messag queue and print out
    // a local echo line if local echo is enabled.
    fn queue_outgoing_message(
        &self,
        uuid: Uuid,
        content: &MessageEventContent,
    ) {
        if self.config.borrow().look().local_echo() {
            if let MessageEventContent::Text(c) = content {
                let sender =
                    self.members.get(&self.own_user_id).unwrap_or_else(|| {
                        panic!("No own member {}", self.own_user_id)
                    });

                let local_echo =
                    c.render_with_prefix_for_echo(&sender, uuid, &());
                self.print_rendered_event(local_echo);

                self.outgoing_messages.add_with_echo(uuid, content.clone());
            } else {
                self.outgoing_messages.add(uuid, content.clone());
            }
        } else {
            self.outgoing_messages.add(uuid, content.clone());
        }
    }

    /// Send the given content to the server.
    ///
    /// # Arguments
    ///
    /// * `content` - The content that should be sent to the server.
    ///
    /// # Examples
    ///
    /// ```
    /// let content = MessageEventContent::Text(TextMessageEventContent {
    ///     body: "Hello world".to_owned(),
    ///     formatted: None,
    ///     relates_to: None,
    /// });
    /// let content = AnyMessageEventContent::RoomMessage(content);
    ///
    /// buffer.send_message(content).await
    /// ```
    pub async fn send_message(&self, content: MessageEventContent) {
        let uuid = Uuid::new_v4();

        if let Some(c) = &*self.connection.borrow() {
            self.queue_outgoing_message(uuid, &content);
            match c
                .send_message(
                    &self.room_id,
                    AnyMessageEventContent::RoomMessage(content),
                    Some(uuid),
                )
                .await
            {
                Ok(r) => {
                    self.handle_outgoing_message(uuid, &r.event_id).await;
                }
                Err(_e) => {
                    // TODO print out an error, remember to modify the local
                    // echo line if there is one.
                    self.outgoing_messages.remove(uuid);
                }
            }
        } else if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.print("Error not connected");
        }
    }

    /// Send out a typing notice.
    ///
    /// This will send out a typing notice or reset the one in progress, if
    /// needed. It will make sure that only one typing notice request is in
    /// flight at a time.
    ///
    /// Typing notices are sent out only if we have more than 4 letters in the
    /// input and the input isn't a command.
    ///
    /// If the input is empty the typing notice is disabled.
    pub fn update_typing_notice(&self) {
        let typing_in_flight = self.typing_in_flight.clone();
        let buffer_handle = self.buffer_handle();

        let buffer = if let Ok(b) = buffer_handle.upgrade() {
            b
        } else {
            return;
        };

        let input = buffer.input();

        if input.starts_with('/') && !input.starts_with("//") {
            // Don't send typing notices for commands.
            return;
        }

        let connection = self.connection.clone();
        let room_id = self.room_id.clone();
        let typing_notice_time = self.typing_notice_time.clone();

        let send = |typing: bool| async move {
            let typing_time = typing_notice_time;

            // We're in the process of sending out a typing notice, so don't
            // make the same request twice.
            let guard = match typing_in_flight.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    return;
                }
            };

            if let Some(connection) = &*connection.borrow() {
                let response =
                    connection.send_typing_notice(&*room_id, typing).await;

                // We need to record the time when the last typing notice was
                // sent to ensure we don't send out new ones while a previous
                // one is active. If we cancelled the last notice (`!typing`),
                // we record that there is currently no active notice.
                //
                // An unsuccessful response indicates the send operation failed.
                // In this case we need to retry, so we don't update anything.

                if response.is_ok() {
                    if typing {
                        *typing_time.borrow_mut() = Some(Instant::now());
                    } else {
                        *typing_time.borrow_mut() = None;
                    }
                }
            };

            drop(guard)
        };

        let typing_time = self.typing_notice_time.borrow_mut();

        if input.len() < 4 && typing_time.is_some() {
            // If we have an active typing notice and our input is short, e.g.
            // we removed the input set the typing notice to false.
            Weechat::spawn(send(false)).detach();
        } else if input.len() >= 4 {
            if let Some(typing_time) = &*typing_time {
                // If we have some valid input, check if the typing notice
                // expired and send one out if it indeed expired.
                if typing_time.elapsed() > TYPING_NOTICE_TIMEOUT {
                    Weechat::spawn(send(true)).detach();
                }
            } else {
                // If we have some valid input and no active typing notice, send
                // one out.
                Weechat::spawn(send(true)).detach();
            }
        }
    }

    pub fn is_busy(&self) -> bool {
        self.messages_in_flight.locked()
    }

    pub fn reset_prev_batch(&self) {
        // TODO we'll want to be able to scroll up again after we clear the
        // buffer.
        *self.prev_batch.borrow_mut() = None;
    }

    pub async fn get_messages(&self) {
        let messages_lock = self.messages_in_flight.clone();

        let connection = self.connection.borrow().as_ref().cloned();
        let room_id = self.room_id().clone();

        let prev_batch =
            if let Some(p) = self.prev_batch.borrow().as_ref().cloned() {
                p
            } else {
                return;
            };

        let guard = if let Ok(l) = messages_lock.try_lock() {
            l
        } else {
            return;
        };

        Weechat::bar_item_update("matrix_modes");

        if let Some(connection) = connection {
            match connection.room_messages(room_id, prev_batch).await {
                Ok(r) => {
                    for event in
                        r.chunk.iter().filter_map(|e| e.deserialize().ok())
                    {
                        self.handle_room_event(&event).await;
                    }

                    if r.chunk.is_empty() {
                        *self.prev_batch.borrow_mut() = None;
                    } else {
                        *self.prev_batch.borrow_mut() = r.end;
                        self.sort_messages();
                    }
                }
                // TODO print out the error.
                Err(_) => (),
            }
        }

        drop(guard);

        Weechat::bar_item_update("matrix_modes");
    }

    fn sort_messages(&self) {
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
            lines.sort_by_key(|l| Reverse(l.date));

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

    /// Replace the local echo of an event with a fully rendered one.
    fn replace_local_echo(
        &self,
        uuid: Uuid,
        buffer: &Buffer,
        rendered: RenderedEvent,
    ) {
        let uuid_tag = Cow::from(format!("matrix_echo_{}", uuid.to_string()));
        let line_contains_uuid = |l: &BufferLine| l.tags().contains(&uuid_tag);

        let mut lines = buffer.lines();
        let mut first_line = lines.rfind(line_contains_uuid);
        let mut line_num = 0;

        while let Some(line) = &first_line {
            let rendered_line = &rendered.content.lines[line_num];

            line.set_message(&rendered_line.message);

            line_num += 1;
            first_line = lines.next_back().filter(line_contains_uuid);
        }
    }

    async fn handle_outgoing_message(&self, uuid: Uuid, event_id: &EventId) {
        if let Some((echo, content)) = self.outgoing_messages.remove(uuid) {
            let event = SyncMessageEvent {
                sender: (&*self.own_user_id).clone(),
                origin_server_ts: std::time::SystemTime::now(),
                event_id: event_id.clone(),
                content,
                unsigned: Default::default(),
            };

            let event = AnySyncMessageEvent::RoomMessage(event);

            let rendered = self
                .render_sync_message(&event)
                .await
                .expect("Sent out an event that we don't know how to render");

            if let Ok(buffer) = self.buffer_handle().upgrade() {
                if echo {
                    self.replace_local_echo(uuid, &buffer, rendered);
                } else {
                    self.print_rendered_event(rendered);
                }
            }
        }
    }

    fn update_buffer_name(&self) {
        let name = self.members.calculate_buffer_name();

        if let Ok(b) = self.buffer_handle().upgrade() {
            b.set_short_name(&name)
        }
    }

    async fn handle_room_message(&self, event: &AnySyncMessageEvent) {
        // If the event has a transaction id it's an event that we sent out
        // ourselves, the content will be in the outgoing message queue and it
        // may have been printed out as a local echo.
        if let Some(id) = &event.unsigned().transaction_id {
            if let Ok(id) = Uuid::parse_str(id) {
                self.handle_outgoing_message(id, event.event_id()).await;
                return;
            }
        }

        if let AnySyncMessageEvent::RoomRedaction(r) = event {
            self.redact_event(r);
        } else if let Some(rendered) = self.render_sync_message(event).await {
            self.print_rendered_event(rendered);
        }
    }

    fn handle_redacted_events(&self, event: &AnyRedactedSyncMessageEvent) {
        use AnyRedactedSyncMessageEvent::*;

        if let RoomMessage(e) = event {
            // TODO remove those expects and unwraps.
            let redacter =
                &e.unsigned.redacted_because.as_ref().unwrap().sender;
            let redacter = self.members.get(redacter).expect(
                "Rendering a message but the sender isn't in the nicklist",
            );
            let sender = self.members.get(&e.sender).expect(
                "Rendering a message but the sender isn't in the nicklist",
            );
            let rendered = e.render_with_prefix(
                &e.origin_server_ts,
                event.event_id(),
                &sender,
                &redacter,
            );

            self.print_rendered_event(rendered);
        }
    }

    pub async fn handle_sync_room_event(&self, event: AnySyncRoomEvent) {
        match &event {
            AnySyncRoomEvent::Message(message) => {
                self.handle_room_message(message).await
            }

            AnySyncRoomEvent::RedactedMessage(e) => {
                self.handle_redacted_events(e)
            }
            // We don't print out redacted state event for now.
            AnySyncRoomEvent::RedactedState(_) => (),

            AnySyncRoomEvent::State(event) => match event {
                AnySyncStateEvent::RoomMember(e) => {
                    self.members.handle_membership_event(e, false)
                }
                AnySyncStateEvent::RoomName(_) => self.update_buffer_name(),
                _ => (),
            },
        }
    }

    pub async fn handle_room_event(&self, event: &AnyRoomEvent) {
        match &event {
            AnyRoomEvent::Message(event) => {
                let sender = self.members.get(event.sender()).expect(
                    "Rendering a message but the sender isn't in the nicklist",
                );

                let send_time = event.origin_server_ts();
                if let Some(rendered) = self
                    .render_message_content(
                        event.event_id(),
                        send_time,
                        &sender,
                        &event.content(),
                    )
                    .await
                {
                    self.print_rendered_event(rendered);
                }
            }
            // TODO print out redacted messages.
            AnyRoomEvent::RedactedMessage(_) => (),
            AnyRoomEvent::RedactedState(_) => (),
            AnyRoomEvent::State(_) => (),
        }
    }

    pub fn handle_sync_state_event(&self, event: AnySyncStateEvent) {
        match &event {
            AnySyncStateEvent::RoomMember(e) => {
                self.members.handle_membership_event(e, true)
            }
            AnySyncStateEvent::RoomName(_) => self.update_buffer_name(),
            _ => (),
        }
    }
}
