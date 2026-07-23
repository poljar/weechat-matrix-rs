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

mod buffer;
mod members;
mod verification;

use buffer::{print_rendered_event_to_buffer, RoomBuffer};
use members::Members;
pub use members::WeechatRoomMember;
use tokio::runtime::Handle;
use tracing::{debug, trace};
use verification::Verification;

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Deref,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
};

use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use matrix_sdk::{
    async_trait,
    attachment::{AttachmentConfig, AttachmentInfo, BaseFileInfo},
    deserialized_responses::AmbiguityChange,
    room::Room,
    ruma::{
        events::{
            relation::Thread,
            room::{
                member::RoomMemberEventContent,
                message::{
                    MessageType, Relation, RoomMessageEventContent,
                    TextMessageEventContent,
                },
                redaction::SyncRoomRedactionEvent,
            },
            AnyMessageLikeEventContent, AnySyncMessageLikeEvent,
            AnySyncStateEvent, AnySyncTimelineEvent, AnyTimelineEvent,
            OriginalSyncMessageLikeEvent, SyncMessageLikeEvent, SyncStateEvent,
        },
        EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomAliasId,
        OwnedTransactionId, OwnedUserId, RoomId, TransactionId, UInt, UserId,
    },
    StoreError,
};

use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferHandle, BufferInputCallbackAsync,
        BufferLine,
    },
    Prefix, Weechat,
};

use crate::{
    config::{Config, RedactionStyle},
    connection::Connection,
    render::{Render, RenderedEvent},
    utils::{Edit, VerificationEvent},
    PLUGIN_NAME,
};

#[derive(Clone)]
pub struct RoomHandle {
    inner: MatrixRoom,
}

pub(super) type SharedRoom = Rc<RefCell<Option<Room>>>;

pub(super) fn maybe_active_room(room: &SharedRoom) -> Option<Room> {
    room.borrow().as_ref().cloned()
}

pub(super) fn active_room(room: &SharedRoom) -> Room {
    maybe_active_room(room).expect("Matrix room was already shut down")
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PrevBatch {
    Forward(String),
    Backwards(String),
}

fn restored_prev_batch(prev_batch: Option<String>) -> Option<PrevBatch> {
    prev_batch.map(PrevBatch::Backwards)
}

fn should_render_event(already_rendered: bool) -> bool {
    !already_rendered
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

impl Drop for IntMutexGuard<'_> {
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
    room: SharedRoom,
    buffer: RoomBuffer,

    config: Rc<RefCell<Config>>,
    connection: Rc<RefCell<Option<Connection>>>,

    messages_in_flight: IntMutex,
    prev_batch: Rc<RefCell<Option<PrevBatch>>>,
    latest_event_id: Rc<RefCell<Option<OwnedEventId>>>,
    latest_read_event_id: Rc<RefCell<Option<OwnedEventId>>>,
    latest_thread_event_ids: Rc<RefCell<HashMap<OwnedEventId, OwnedEventId>>>,

    outgoing_messages: MessageQueue,

    members: Members,
    verification: Verification,
}

#[derive(Debug, Clone, Default)]
pub struct MessageQueue {
    queue: Rc<
        RefCell<HashMap<OwnedTransactionId, (bool, RoomMessageEventContent)>>,
    >,
    rendering: Rc<RefCell<HashSet<OwnedEventId>>>,
}

impl MessageQueue {
    fn new() -> Self {
        Self {
            queue: Rc::new(RefCell::new(HashMap::new())),
            rendering: Rc::new(RefCell::new(HashSet::new())),
        }
    }

    fn add(&self, uuid: OwnedTransactionId, content: RoomMessageEventContent) {
        self.queue.borrow_mut().insert(uuid, (false, content));
    }

    fn add_with_echo(
        &self,
        uuid: OwnedTransactionId,
        content: RoomMessageEventContent,
    ) {
        self.queue.borrow_mut().insert(uuid, (true, content));
    }

    fn remove(
        &self,
        uuid: &TransactionId,
    ) -> Option<(bool, RoomMessageEventContent)> {
        self.queue.borrow_mut().remove(uuid)
    }

    fn start_response(
        &self,
        uuid: &TransactionId,
        event_id: &EventId,
    ) -> Option<(bool, RoomMessageEventContent)> {
        let message = self.remove(uuid)?;
        self.rendering.borrow_mut().insert(event_id.to_owned());
        Some(message)
    }

    fn finish_response(&self, event_id: &EventId) {
        self.rendering.borrow_mut().remove(event_id);
    }

    fn response_in_progress(&self, event_id: &EventId) -> bool {
        self.rendering.borrow().contains(event_id)
    }
}

enum TransactionEventHandling {
    RenderNormally,
    LocalEcho {
        echo: bool,
        content: RoomMessageEventContent,
    },
}

fn take_transaction_event(
    queue: &MessageQueue,
    transaction_id: Option<&TransactionId>,
) -> TransactionEventHandling {
    match transaction_id.and_then(|id| queue.remove(id)) {
        Some((echo, content)) => {
            TransactionEventHandling::LocalEcho { echo, content }
        }
        None => TransactionEventHandling::RenderNormally,
    }
}

fn make_text_message_content(
    input: String,
    markdown: bool,
    thread_root: Option<OwnedEventId>,
    latest_thread_event: Option<OwnedEventId>,
) -> RoomMessageEventContent {
    let text = if markdown {
        TextMessageEventContent::markdown(input)
    } else {
        TextMessageEventContent::plain(input)
    };

    let mut content = RoomMessageEventContent::new(MessageType::Text(text));

    if let Some(thread_root) = thread_root {
        let latest_thread_event =
            latest_thread_event.unwrap_or_else(|| thread_root.clone());

        content.relates_to = Some(Relation::Thread(Thread::plain(
            thread_root,
            latest_thread_event,
        )));
    }

    content
}

fn thread_root_from_buffer(buffer: &Buffer) -> Option<OwnedEventId> {
    buffer
        .get_localvar("thread_root")
        .and_then(|thread_root| EventId::parse(thread_root.as_ref()).ok())
}

fn thread_root_from_content(
    content: &RoomMessageEventContent,
) -> Option<&EventId> {
    match content.relates_to.as_ref() {
        Some(Relation::Thread(thread)) => Some(&thread.event_id),
        _ => None,
    }
}

fn thread_root_from_event(
    event: &AnySyncMessageLikeEvent,
) -> Option<OwnedEventId> {
    event.original_content().and_then(|content| match content {
        AnyMessageLikeEventContent::RoomMessage(content) => {
            thread_root_from_content(&content).map(ToOwned::to_owned)
        }
        _ => None,
    })
}

impl RoomHandle {
    pub fn new(
        server_name: &str,
        runtime: Handle,
        connection: &Rc<RefCell<Option<Connection>>>,
        config: Rc<RefCell<Config>>,
        room: Room,
        homeserver: Url,
        room_id: &RoomId,
        own_user_id: &UserId,
    ) -> Self {
        let room = Rc::new(RefCell::new(Some(room)));
        let sdk_room = active_room(&room);
        let buffer = RoomBuffer::new(room.clone(), runtime.clone());
        let members = Members::new(
            room.clone(),
            runtime.clone(),
            config.clone(),
            buffer.clone(),
        );

        let own_nick = runtime
            .block_on(sdk_room.get_member_no_sync(own_user_id))
            .ok()
            .flatten()
            .map(|m| m.name().to_owned())
            .unwrap_or_else(|| own_user_id.localpart().to_owned());

        let verification = Verification::new(
            own_user_id.into(),
            connection.clone(),
            members.clone(),
            buffer.clone(),
        );

        let room = MatrixRoom {
            homeserver: Rc::new(homeserver),
            room_id: room_id.into(),
            connection: connection.clone(),
            config,
            prev_batch: Rc::new(RefCell::new(
                sdk_room.last_prev_batch().map(PrevBatch::Backwards),
            )),
            latest_event_id: Rc::new(RefCell::new(None)),
            latest_read_event_id: Rc::new(RefCell::new(None)),
            latest_thread_event_ids: Rc::new(RefCell::new(HashMap::new())),
            own_user_id: own_user_id.into(),
            members,
            buffer,
            verification,
            outgoing_messages: MessageQueue::new(),
            messages_in_flight: IntMutex::new(),
            room,
        };

        let buffer_name = format!("{}.{}", server_name, room_id);

        let buffer_handle = BufferBuilderAsync::new(&buffer_name)
            .input_callback(room.clone())
            .close_callback(|_weechat: &Weechat, _buffer: &Buffer| {
                // TODO: remove the roombuffer from the server here.
                // TODO: leave the room if the plugin isn't unloading.
                Ok(())
            })
            .build()
            .expect("Can't create new room buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created buffer");

        buffer
            .add_nicklist_group(
                "000|o",
                "weechat.color.nicklist_group",
                true,
                None,
            )
            .expect("Can't create nicklist group");
        buffer
            .add_nicklist_group(
                "001|h",
                "weechat.color.nicklist_group",
                true,
                None,
            )
            .expect("Can't create nicklist group");
        buffer
            .add_nicklist_group(
                "002|v",
                "weechat.color.nicklist_group",
                true,
                None,
            )
            .expect("Can't create nicklist group");
        buffer
            .add_nicklist_group(
                "999|...",
                "weechat.color.nicklist_group",
                true,
                None,
            )
            .expect("Can't create nicklist group");

        buffer.enable_nicklist();
        buffer.disable_nicklist_groups();
        buffer.enable_multiline();

        buffer.set_localvar("server", server_name);
        buffer.set_localvar("nick", &own_nick);
        buffer
            .run_command("/buffer set highlight_words $nick")
            .expect("Can't set room buffer highlight words");
        buffer.set_localvar(
            "domain",
            room.room_id()
                .server_name()
                .map(|name| name.as_str())
                .unwrap_or_default(),
        );
        buffer.set_localvar("room_id", room.room_id().as_str());
        if room.is_direct() {
            buffer.set_localvar("type", "private")
        } else {
            buffer.set_localvar("type", "channel")
        }

        if let Some(alias) = room.alias() {
            buffer.set_localvar("alias", alias.as_str());
        }

        *room.buffer.inner.borrow_mut() = Some(buffer_handle.clone());
        room.buffer.update_parent_spaces();

        Self { inner: room }
    }

    pub async fn restore(
        server_name: &str,
        runtime: Handle,
        room: Room,
        connection: &Rc<RefCell<Option<Connection>>>,
        config: Rc<RefCell<Config>>,
        homeserver: Url,
    ) -> Result<Self, StoreError> {
        let room_clone = room.clone();
        let room_id = room.room_id();
        let own_user_id = room.own_user_id();
        let prev_batch = room.last_prev_batch();

        let room_buffer = Self::new(
            server_name,
            runtime.clone(),
            connection,
            config,
            room_clone,
            homeserver,
            room_id,
            own_user_id,
        );

        debug!("Restoring room {}", room.room_id());

        // Sync callbacks can run while member restoration awaits the SDK. Put
        // the saved history cursor in place first so a sync event cannot set a
        // newer cursor which is then overwritten with the saved one.
        *room_buffer.prev_batch.borrow_mut() = restored_prev_batch(prev_batch);

        let matrix_members = runtime
            .spawn(async move { room.joined_user_ids().await })
            .await
            .expect("Couldn't get the joined user ids")?;

        for user_id in matrix_members {
            trace!("Restoring member {}", &user_id);
            room_buffer.members.restore_member(user_id).await;
        }

        room_buffer.buffer.update_buffer_name();
        room_buffer.buffer.set_topic();
        room_buffer.buffer.update_parent_spaces();

        Ok(room_buffer)
    }
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, buffer: BufferHandle, input: String) {
        let thread_root = buffer
            .upgrade()
            .ok()
            .and_then(|buffer| thread_root_from_buffer(&buffer));
        let latest_thread_event = thread_root.as_ref().and_then(|root| {
            self.latest_thread_event_ids.borrow().get(root).cloned()
        });
        let content = make_text_message_content(
            input,
            self.config.borrow().input().markdown_input(),
            thread_root,
            latest_thread_event,
        );

        self.send_message(content).await;
    }
}

impl MatrixRoom {
    pub fn owns_buffer(&self, buffer: &Buffer) -> bool {
        self.buffer.owns_buffer(buffer)
    }

    pub fn release_sdk_state(&self) {
        self.verification.release_sdk_state();
        self.room.borrow_mut().take();
    }

    pub fn is_encrypted(&self) -> bool {
        let Some(room) = maybe_active_room(&self.room) else {
            return false;
        };

        self.members
            .runtime
            .block_on(room.latest_encryption_state())
            .map(|s| s.is_encrypted())
            .unwrap_or_default()
    }

    pub fn contains_only_verified_devices(&self) -> bool {
        let Some(room) = maybe_active_room(&self.room) else {
            return false;
        };

        self.members
            .runtime
            .block_on(room.contains_only_verified_devices())
            .unwrap_or_default()
    }

    pub fn is_public(&self) -> bool {
        maybe_active_room(&self.room)
            .and_then(|room| room.is_public())
            .unwrap_or_default()
    }

    pub fn is_direct(&self) -> bool {
        let Some(room) = maybe_active_room(&self.room) else {
            return false;
        };

        self.members
            .runtime
            .block_on(room.is_direct())
            .unwrap_or_default()
    }

    pub fn alias(&self) -> Option<OwnedRoomAliasId> {
        maybe_active_room(&self.room).and_then(|room| room.canonical_alias())
    }

    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    pub fn names(&self) -> Vec<String> {
        self.members.names()
    }

    pub fn buffer_handle(&self) -> BufferHandle {
        self.buffer.buffer_handle()
    }

    pub fn update_parent_spaces(&self) {
        self.buffer.update_parent_spaces();
    }

    pub fn accept_verification(&self) {
        let verification = self.verification.clone();
        Weechat::spawn(async move { verification.accept().await }).detach();
    }

    pub fn confirm_verification(&self) {
        let verification = self.verification.clone();
        Weechat::spawn(async move { verification.confirm().await }).detach();
    }

    pub fn cancel_verification(&self) {
        let verification = self.verification.clone();
        Weechat::spawn(async move { verification.cancel().await }).detach();
    }

    async fn redact_event(&self, event: &SyncRoomRedactionEvent) {
        let event = if let SyncRoomRedactionEvent::Original(e) = event {
            e
        } else {
            // Redacted redaction events don't contain enough data to be applied, so there's
            // nothing to do here.
            return;
        };

        let buffer_handle = self.buffer_handle();

        let buffer = if let Ok(b) = buffer_handle.upgrade() {
            b
        } else {
            return;
        };

        // TODO: remove this unwrap.
        let redacter = self.members.get(&event.sender).await.unwrap();

        // TODO: handle unwrapping redacts Option<EventId> properly for rooms versions 11+
        let event_id_tag = Cow::from(format!(
            "{}_id_{}",
            PLUGIN_NAME,
            event.redacts.clone().unwrap()
        ));
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
            redacter.nick(),
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
        }

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
        send_time: MilliSecondsSinceUnixEpoch,
        sender: &WeechatRoomMember,
        content: &AnyMessageLikeEventContent,
    ) -> Option<RenderedEvent> {
        use AnyMessageLikeEventContent::{RoomEncrypted, RoomMessage};
        use MessageType::*;

        self.members.mark_active(sender.user_id(), send_time);

        let rendered = match content {
            RoomEncrypted(c) => {
                c.render_with_prefix(send_time, event_id, sender, &())
            }
            RoomMessage(c) => {
                let reply_to = match c.relates_to.as_ref() {
                    Some(Relation::Reply { in_reply_to }) => {
                        let sender = match self
                            .buffer
                            .reply_sender_id(&in_reply_to.event_id)
                        {
                            Some(sender_id) => self
                                .members
                                .get(&sender_id)
                                .await
                                .map(|member| member.nick()),
                            None => None,
                        };

                        Some((&in_reply_to.event_id, sender))
                    }
                    _ => None,
                };

                let rendered = match &c.msgtype {
                    Text(c) => {
                        c.render_with_prefix(send_time, event_id, sender, &())
                    }
                    Emote(c) => c.render_with_prefix(
                        send_time, event_id, sender, sender,
                    ),
                    Notice(c) => c.render_with_prefix(
                        send_time, event_id, sender, sender,
                    ),
                    ServerNotice(c) => c.render_with_prefix(
                        send_time, event_id, sender, sender,
                    ),
                    Location(c) => c.render_with_prefix(
                        send_time, event_id, sender, sender,
                    ),
                    Audio(c) => c.render_with_prefix(
                        send_time,
                        event_id,
                        sender,
                        &self.homeserver,
                    ),
                    Video(c) => c.render_with_prefix(
                        send_time,
                        event_id,
                        sender,
                        &self.homeserver,
                    ),
                    File(c) => c.render_with_prefix(
                        send_time,
                        event_id,
                        sender,
                        &self.homeserver,
                    ),
                    Image(c) => c.render_with_prefix(
                        send_time,
                        event_id,
                        sender,
                        &self.homeserver,
                    ),
                    _ => return None,
                };

                if let Some((event_id, sender)) = reply_to {
                    rendered.add_reply_context(event_id, sender.as_deref())
                } else {
                    rendered
                }
            }
            _ => return None,
        };

        Some(rendered)
    }

    fn get_or_create_thread_buffer(
        &self,
        thread_root: &EventId,
    ) -> Option<BufferHandle> {
        if let Some(handle) = self.buffer.thread_buffer(thread_root) {
            if handle.upgrade().is_ok() {
                return Some(handle);
            }

            self.buffer.remove_thread_buffer(thread_root);
        }

        let room_buffer_handle = self.buffer_handle();
        let room_buffer = room_buffer_handle.upgrade().ok()?;
        let thread_short_name =
            self.buffer.calculate_thread_buffer_name(thread_root);
        let buffer_name = format!(
            "{}.thread.{}",
            room_buffer.name(),
            RoomBuffer::thread_buffer_suffix(thread_root)
        );
        let server = room_buffer
            .get_localvar("server")
            .map(|value| value.to_string());
        let nick = room_buffer
            .get_localvar("nick")
            .map(|value| value.to_string());
        let domain = room_buffer
            .get_localvar("domain")
            .map(|value| value.to_string());
        let room_short_name = self.buffer.short_name();

        let buffer_handle = BufferBuilderAsync::new(&buffer_name)
            .input_callback(self.clone())
            .close_callback(|_weechat: &Weechat, _buffer: &Buffer| Ok(()))
            .build()
            .ok()?;
        let buffer = buffer_handle.upgrade().ok()?;

        buffer.set_short_name(&thread_short_name);
        buffer.enable_multiline();
        buffer.disable_nicklist();
        buffer.disable_nicklist_groups();

        if let Some(server) = server {
            buffer.set_localvar("server", &server);
        }
        if let Some(nick) = nick {
            buffer.set_localvar("nick", &nick);
        }
        if let Some(domain) = domain {
            buffer.set_localvar("domain", &domain);
        }

        buffer.set_localvar("room_id", self.room_id.as_str());
        buffer.set_localvar("thread_root", thread_root.as_str());
        buffer.set_title(&format!(
            "Thread {} in {}",
            thread_root, room_short_name
        ));

        self.buffer
            .set_thread_buffer(thread_root.to_owned(), buffer_handle.clone());

        Some(buffer_handle)
    }

    fn print_rendered_event_for_relation(
        &self,
        thread_root: Option<&EventId>,
        rendered: RenderedEvent,
    ) {
        if let Some(handle) =
            thread_root.and_then(|root| self.get_or_create_thread_buffer(root))
        {
            if let Ok(buffer) = handle.upgrade() {
                print_rendered_event_to_buffer(&buffer, rendered);
            } else {
                self.buffer.print_rendered_event(rendered);
            }
        } else {
            self.buffer.print_rendered_event(rendered);
        }
    }

    async fn render_sync_message(
        &self,
        event: &AnySyncMessageLikeEvent,
    ) -> Option<RenderedEvent> {
        // TODO: remove this expect.
        let sender =
            self.members.get(event.sender()).await.expect(
                "Rendering a message but the sender isn't in the nicklist",
            );

        if let Some(content) = event.original_content() {
            let send_time = event.origin_server_ts();
            self.render_message_content(
                event.event_id(),
                send_time,
                &sender,
                &content,
            )
            .await
            .map(|r| {
                // TODO: the tags are different if the room is a DM.
                if sender.user_id() == &*self.own_user_id {
                    r.add_self_tags()
                } else {
                    r.add_msg_tags()
                }
            })
        } else {
            self.render_redacted_event(event).await
        }
    }

    // Add the content of the message to our outgoing message queue and print out
    // a local echo line if local echo is enabled.
    async fn queue_outgoing_message(
        &self,
        transaction_id: &TransactionId,
        content: &RoomMessageEventContent,
    ) {
        let thread_root =
            thread_root_from_content(content).map(ToOwned::to_owned);

        if self.config.borrow().look().local_echo() {
            if let MessageType::Text(c) = &content.msgtype {
                let sender =
                    self.members.get(&self.own_user_id).await.unwrap_or_else(
                        || panic!("No own member {}", self.own_user_id),
                    );

                let local_echo = c
                    .render_with_prefix_for_echo(&sender, transaction_id, &())
                    .add_self_tags();
                self.print_rendered_event_for_relation(
                    thread_root.as_deref(),
                    local_echo,
                );

                self.outgoing_messages
                    .add_with_echo(transaction_id.to_owned(), content.clone());
            } else {
                self.outgoing_messages
                    .add(transaction_id.to_owned(), content.clone());
            }
        } else {
            self.outgoing_messages
                .add(transaction_id.to_owned(), content.clone());
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
    pub async fn send_message(&self, content: RoomMessageEventContent) {
        let transaction_id = TransactionId::new();

        let connection = self.connection.borrow().clone();

        if let Some(c) = connection {
            self.queue_outgoing_message(&transaction_id, &content).await;
            match c
                .send_message(
                    self.room(),
                    AnyMessageLikeEventContent::RoomMessage(content),
                    Some(transaction_id.clone()),
                )
                .await
            {
                Ok(r) => {
                    if let Some((echo, content)) = self
                        .outgoing_messages
                        .start_response(&transaction_id, &r.event_id)
                    {
                        self.handle_outgoing_message(
                            &transaction_id,
                            &r.event_id,
                            echo,
                            content,
                        )
                        .await;
                        self.outgoing_messages.finish_response(&r.event_id);
                    }
                }
                Err(_e) => {
                    // TODO: print out an error, remember to modify the local
                    // echo line if there is one.
                    self.outgoing_messages.remove(&transaction_id);
                }
            }
        } else if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.print("Error not connected");
        }
    }

    fn print_network(&self, message: &str) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.print(&format!(
                "{}{}",
                Weechat::prefix(Prefix::Network),
                message
            ));
        }
    }

    fn print_error(&self, message: &str) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            buffer.print(&format!(
                "{}{}",
                Weechat::prefix(Prefix::Error),
                message
            ));
        }
    }

    pub async fn invite_user(&self, user_id: OwnedUserId) {
        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        let invited_user = user_id.clone();
        let room = self.room().clone();

        match connection
            .spawn(async move { room.invite_user_by_id(&user_id).await })
            .await
        {
            Ok(()) => self.print_network(&format!(
                "Invited {} to the room.",
                invited_user
            )),
            Err(error) => self.print_error(&format!(
                "Failed to invite {}: {}",
                invited_user, error
            )),
        }
    }

    pub async fn send_redaction(
        &self,
        event_id: OwnedEventId,
        reason: Option<String>,
    ) {
        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        let room = self.room().clone();
        let error_event_id = event_id.clone();

        match connection
            .spawn(async move {
                room.redact(&event_id, reason.as_deref(), None).await
            })
            .await
        {
            Ok(_) => (),
            Err(error) => self.print_error(&format!(
                "Failed to redact {}: {}",
                error_event_id, error
            )),
        }
    }

    pub async fn ban_user(&self, user_id: OwnedUserId, reason: Option<String>) {
        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        let room = self.room().clone();
        let error_user_id = user_id.clone();

        match connection
            .spawn(
                async move { room.ban_user(&user_id, reason.as_deref()).await },
            )
            .await
        {
            Ok(_) => (),
            Err(error) => self.print_error(&format!(
                "Failed to ban {}: {}",
                error_user_id, error
            )),
        }
    }

    pub async fn kick_user(
        &self,
        user_id: OwnedUserId,
        reason: Option<String>,
    ) {
        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        let room = self.room().clone();
        let error_user_id = user_id.clone();

        match connection
            .spawn(async move { room.kick_user(&user_id, reason.as_deref()).await })
            .await
        {
            Ok(_) => (),
            Err(error) => self.print_error(&format!(
                "Failed to kick {}: {}",
                error_user_id, error
            )),
        }
    }

    pub async fn unban_user(
        &self,
        user_id: OwnedUserId,
        reason: Option<String>,
    ) {
        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        let room = self.room().clone();
        let error_user_id = user_id.clone();

        match connection
            .spawn(async move { room.unban_user(&user_id, reason.as_deref()).await })
            .await
        {
            Ok(_) => (),
            Err(error) => self.print_error(&format!(
                "Failed to unban {}: {}",
                error_user_id, error
            )),
        }
    }

    pub async fn leave(&self) {
        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        let room = self.room().clone();

        match connection.spawn(async move { room.leave().await }).await {
            Ok(()) => {
                if let Ok(buffer) = self.buffer_handle().upgrade() {
                    let display_name = buffer
                        .get_localvar("nick")
                        .map(|nick| nick.into_owned())
                        .unwrap_or_else(|| self.room_id().to_string());

                    buffer.print(&format!(
                        "{}{} has left the room",
                        Weechat::prefix(Prefix::Quit),
                        display_name,
                    ));
                }
            }
            Err(error) => {
                self.print_error(&format!("Failed to leave room: {}", error))
            }
        }
    }

    pub async fn send_attachment(&self, path: PathBuf) {
        let Some(filename) = path
            .file_name()
            .and_then(|f| f.to_str())
            .map(ToOwned::to_owned)
        else {
            self.print_error("Invalid file name");
            return;
        };

        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) => {
                self.print_error(&format!(
                    "Failed to read attachment {}: {}",
                    path.display(),
                    e
                ));
                return;
            }
        };

        let content_type = mime_guess::from_path(&path).first_or_octet_stream();
        let size = UInt::new(data.len() as u64);
        let config = AttachmentConfig::new()
            .info(AttachmentInfo::File(BaseFileInfo { size }));

        let Some(connection) = self.connection.borrow().clone() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        match connection
            .spawn({
                let room = self.room();
                async move {
                    room.send_attachment(filename, &content_type, data, config)
                        .await
                }
            })
            .await
        {
            Ok(_) => (),
            Err(e) => {
                self.print_error(&format!(
                    "Failed to upload attachment {}: {}",
                    path.display(),
                    e
                ));
            }
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
        let room = self.room();

        let send = |typing: bool| async move {
            let connection = connection.borrow().clone();

            if let Some(connection) = connection {
                let _ = connection.send_typing_notice(room, typing).await;
            };
        };

        if input.len() < 4 {
            // If we have an active typing notice and our input is short, e.g.
            // we removed the input set the typing notice to false.
            Weechat::spawn(send(false)).detach();
        } else if input.len() >= 4 {
            // If we have some valid input and no active typing notice, send
            // one out.
            Weechat::spawn(send(true)).detach();
        }
    }

    pub fn is_busy(&self) -> bool {
        self.messages_in_flight.locked()
    }

    pub fn reset_prev_batch(&self) {
        // TODO: we'll want to be able to scroll up again after we clear the
        // buffer.
        *self.prev_batch.borrow_mut() = None;
    }

    fn mark_event_as_read(&self, event_id: OwnedEventId, verbose: bool) {
        if self.latest_read_event_id.borrow().as_ref() == Some(&event_id) {
            return;
        }

        let connection = self.connection.borrow().clone();
        let room = self.room().clone();
        let public_receipt =
            self.config.borrow().network().send_read_receipts();
        let latest_read_event_id = self.latest_read_event_id.clone();

        let mark_read = async move {
            if let Some(connection) = connection {
                match connection
                    .mark_room_as_read(room, event_id.clone(), public_receipt)
                    .await
                {
                    Ok(()) => {
                        *latest_read_event_id.borrow_mut() = Some(event_id);
                    }
                    Err(e) => {
                        Weechat::print(&format!(
                            "{}: Failed to mark room as read: {}",
                            PLUGIN_NAME, e
                        ));
                    }
                }
            } else if verbose {
                Weechat::print(&format!(
                    "{}: Room is not connected.",
                    PLUGIN_NAME
                ));
            }
        };

        Weechat::spawn(mark_read).detach();
    }

    fn mark_latest_event_as_read(&self, verbose: bool) {
        let event_id =
            if let Some(event_id) = self.latest_event_id.borrow().clone() {
                event_id
            } else if verbose {
                Weechat::print(&format!(
                    "{}: No event has been received for this room yet.",
                    PLUGIN_NAME
                ));
                return;
            } else {
                return;
            };

        self.mark_event_as_read(event_id, verbose);
    }

    pub fn mark_as_read(&self) {
        self.mark_latest_event_as_read(true);
    }

    pub fn mark_as_read_silent(&self) {
        self.mark_latest_event_as_read(false);
    }

    pub async fn get_messages(&self) {
        let messages_lock = self.messages_in_flight.clone();

        let connection = self.connection.borrow().as_ref().cloned();

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

        Weechat::bar_item_update("buffer_modes");
        Weechat::bar_item_update("matrix_modes");

        if let Some(connection) = connection {
            let room = self.room();
            let room_id = room.room_id().to_owned();

            if let Ok(r) = connection.room_messages(room, prev_batch).await {
                for event in
                    r.chunk.iter().filter_map(|e| e.raw().deserialize().ok())
                {
                    let event = event.into_full_event(room_id.clone());
                    if self.latest_event_id.borrow().is_none() {
                        let event_id = match &event {
                            AnyTimelineEvent::MessageLike(event) => {
                                event.event_id().to_owned()
                            }
                            AnyTimelineEvent::State(event) => {
                                event.event_id().to_owned()
                            }
                        };
                        *self.latest_event_id.borrow_mut() = Some(event_id);
                    }
                    self.handle_room_event(&event).await;
                }

                let mut prev_batch = self.prev_batch.borrow_mut();

                if let Some(PrevBatch::Forward(t)) = prev_batch.as_ref() {
                    *prev_batch = Some(PrevBatch::Backwards(t.to_owned()));
                    self.buffer.sort_messages();
                } else if r.chunk.is_empty() {
                    *prev_batch = None;
                } else {
                    *prev_batch = r.end.map(PrevBatch::Backwards);
                    self.buffer.sort_messages();
                }
            }
        }

        drop(guard);

        Weechat::bar_item_update("buffer_modes");
        Weechat::bar_item_update("matrix_modes");
    }

    pub async fn get_messages_if_empty(&self) {
        let buffer_handle = self.buffer_handle();
        let Ok(buffer) = buffer_handle.upgrade() else {
            return;
        };

        if buffer.num_lines() == 0 {
            self.get_messages().await;
        }
    }

    async fn handle_outgoing_message(
        &self,
        transaction_id: &TransactionId,
        event_id: &EventId,
        echo: bool,
        content: RoomMessageEventContent,
    ) {
        let thread_root =
            thread_root_from_content(&content).map(ToOwned::to_owned);

        let event = OriginalSyncMessageLikeEvent {
            sender: (*self.own_user_id).to_owned(),
            origin_server_ts: MilliSecondsSinceUnixEpoch::now(),
            event_id: event_id.to_owned(),
            content,
            unsigned: Default::default(),
        };

        let event = AnySyncMessageLikeEvent::RoomMessage(
            SyncMessageLikeEvent::Original(event),
        );

        let rendered = self
            .render_sync_message(&event)
            .await
            .expect("Sent out an event that we don't know how to render");

        if echo {
            self.buffer.replace_local_echo(transaction_id, rendered);
        } else {
            self.print_rendered_event_for_relation(
                thread_root.as_deref(),
                rendered,
            );
        }

        if let Some(thread_root) = thread_root {
            self.latest_thread_event_ids
                .borrow_mut()
                .insert(thread_root, event_id.to_owned());
        }
        self.mark_event_as_read(event_id.to_owned(), false);
    }

    async fn handle_edits(&self, event: &AnySyncMessageLikeEvent) {
        // TODO: remove this expect.
        let sender =
            self.members.get(event.sender()).await.expect(
                "Rendering a message but the sender isn't in the nicklist",
            );

        if let Some((event_id, content)) = event.get_edit() {
            let send_time = event.origin_server_ts();

            if let Some(rendered) = self
                .render_message_content(
                    event_id,
                    send_time,
                    &sender,
                    &AnyMessageLikeEventContent::RoomMessage(
                        content.clone().with_relation(None),
                    ),
                )
                .await
                .map(|r| {
                    // TODO: the tags are different if the room is a DM.
                    if sender.user_id() == &*self.own_user_id {
                        r.add_self_tags()
                    } else {
                        r.add_msg_tags()
                    }
                })
            {
                self.buffer.replace_edit(event_id, event.sender(), rendered);
            }
        }
    }

    async fn handle_room_message(&self, event: &AnySyncMessageLikeEvent) {
        // If the event has a transaction id it's an event that we sent out
        // ourselves, the content will be in the outgoing message queue and it
        // may have been printed out as a local echo.
        self.members
            .mark_active(event.sender(), event.origin_server_ts());

        if let Some(id) = event.transaction_id() {
            // The send response may have rendered this event before /sync
            // arrived.
            if !event.is_edit()
                && (self
                    .outgoing_messages
                    .response_in_progress(event.event_id())
                    || self.buffer.contains_event(event.event_id()))
            {
                return;
            }

            if let TransactionEventHandling::LocalEcho { echo, content } =
                take_transaction_event(&self.outgoing_messages, Some(id))
            {
                self.handle_outgoing_message(
                    id,
                    event.event_id(),
                    echo,
                    content,
                )
                .await;
                return;
            }
        }

        if let AnySyncMessageLikeEvent::RoomRedaction(r) = event {
            self.redact_event(r).await;
        } else if event.is_verification() {
            self.verification.handle_room_verification(event).await;
        } else if event.is_edit() {
            self.handle_edits(event).await;
        } else if !should_render_event(
            self.buffer.contains_event(event.event_id()),
        ) {
            return;
        } else if let Some(rendered) = self.render_sync_message(event).await {
            let thread_root = thread_root_from_event(event);

            if let Some(thread_root) = &thread_root {
                self.latest_thread_event_ids
                    .borrow_mut()
                    .insert(thread_root.clone(), event.event_id().to_owned());
            }

            self.print_rendered_event_for_relation(
                thread_root.as_deref(),
                rendered,
            );
        }
    }

    async fn render_redacted_event(
        &self,
        event: &AnySyncMessageLikeEvent,
    ) -> Option<RenderedEvent> {
        if let AnySyncMessageLikeEvent::RoomMessage(
            SyncMessageLikeEvent::Redacted(e),
        ) = event
        {
            let redacter = e
                .unsigned
                .redacted_because
                .get_field::<OwnedUserId>("sender")
                .ok()
                .flatten()?;
            let redacter = self.members.get(redacter.as_ref()).await?;
            let sender = self.members.get(&e.sender).await?;

            Some(e.render_with_prefix(
                e.origin_server_ts,
                event.event_id(),
                &sender,
                &redacter,
            ))
        } else {
            None
        }
    }

    pub async fn handle_membership_event(
        &self,
        event: &SyncStateEvent<RoomMemberEventContent>,
        state_event: bool,
        ambiguity_change: Option<&AmbiguityChange>,
    ) {
        let smart_filter_delay_ms =
            self.config.borrow().look().smart_filter_delay();

        self.members
            .handle_membership_event(
                event,
                state_event,
                ambiguity_change,
                smart_filter_delay_ms,
            )
            .await;

        Weechat::bar_item_update("buffer_modes");
    }

    fn set_prev_batch(&self) {
        if let Ok(buffer) = self.buffer_handle().upgrade() {
            if buffer.num_lines() == 0 {
                *self.prev_batch.borrow_mut() =
                    self.room().last_prev_batch().map(PrevBatch::Backwards);
            }
        }
    }

    pub async fn handle_sync_room_event(&self, event: AnySyncTimelineEvent) {
        self.set_prev_batch();

        *self.latest_event_id.borrow_mut() = Some(match &event {
            AnySyncTimelineEvent::MessageLike(event) => {
                event.event_id().to_owned()
            }
            AnySyncTimelineEvent::State(event) => event.event_id().to_owned(),
        });

        match &event {
            AnySyncTimelineEvent::MessageLike(message) => {
                self.handle_room_message(message).await
            }
            AnySyncTimelineEvent::State(event) => {
                self.handle_sync_state_event(event, false).await
            }
        }

        let is_current = self
            .buffer_handle()
            .upgrade()
            .map(|buffer| {
                // Sync callbacks run on WeeChat's main thread, so the global
                // context is valid for this immediate buffer comparison.
                buffer == unsafe { Weechat::weechat() }.current_buffer()
            })
            .unwrap_or(false);
        if is_current {
            self.mark_as_read_silent();
        }
    }

    pub async fn handle_room_event(&self, event: &AnyTimelineEvent) {
        match &event {
            AnyTimelineEvent::MessageLike(event) => {
                // TODO: Only print out historical events if they aren't edits of
                // other events.
                if !event.is_edit()
                    && should_render_event(
                        self.buffer.contains_event(event.event_id()),
                    )
                {
                    let sender = self.members.get(event.sender()).await.expect(
                    "Rendering a message but the sender isn't in the nicklist",
                );

                    let content = if let Some(content) =
                        event.original_content()
                    {
                        content
                    } else {
                        tracing::error!("Unhandled redacted event: {event:?}");
                        return;
                    };

                    let send_time = event.origin_server_ts();

                    if let Some(rendered) = self
                        .render_message_content(
                            event.event_id(),
                            send_time,
                            &sender,
                            &content,
                        )
                        .await
                    {
                        self.buffer.print_rendered_event(rendered);
                    }
                }
            }
            // TODO: print out state events.
            AnyTimelineEvent::State(_) => (),
        }
    }

    pub fn room(&self) -> Room {
        active_room(&self.room)
    }

    pub async fn handle_sync_state_event(
        &self,
        event: &AnySyncStateEvent,
        _state_event: bool,
    ) {
        self.members
            .mark_active(event.sender(), event.origin_server_ts());

        match event {
            AnySyncStateEvent::RoomName(_) => self.buffer.update_buffer_name(),
            AnySyncStateEvent::RoomTopic(_) => self.buffer.set_topic(),
            AnySyncStateEvent::RoomCanonicalAlias(_) => {
                self.buffer.set_alias();
                self.buffer.update_buffer_name();
            }
            AnySyncStateEvent::SpaceParent(_) => {
                self.buffer.update_parent_spaces()
            }
            AnySyncStateEvent::RoomEncryption(_)
            | AnySyncStateEvent::RoomJoinRules(_) => {
                Weechat::bar_item_update("buffer_modes");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_content(body: &str) -> RoomMessageEventContent {
        RoomMessageEventContent::text_plain(body)
    }

    #[test]
    fn restored_rooms_fetch_history_backwards_from_prev_batch() {
        assert_eq!(
            restored_prev_batch(Some("token".to_owned())),
            Some(PrevBatch::Backwards("token".to_owned()))
        );
    }

    #[test]
    fn restored_rooms_without_prev_batch_have_no_history_request() {
        assert_eq!(restored_prev_batch(None), None);
    }

    #[test]
    fn already_rendered_events_are_not_printed_again() {
        assert!(should_render_event(false));
        assert!(!should_render_event(true));
    }

    #[test]
    fn thread_buffer_input_sends_thread_relation() {
        let content = make_text_message_content(
            "thread body".to_owned(),
            false,
            Some(EventId::parse("$thread-root:example.org").unwrap()),
            Some(EventId::parse("$latest-thread-event:example.org").unwrap()),
        );

        assert!(matches!(content.msgtype, MessageType::Text(_)));

        let Some(Relation::Thread(thread)) = content.relates_to else {
            panic!("thread buffer input must send an m.thread relation");
        };

        assert_eq!(
            thread.event_id,
            EventId::parse("$thread-root:example.org").unwrap()
        );
        assert_eq!(
            thread.in_reply_to.expect("fallback target").event_id,
            EventId::parse("$latest-thread-event:example.org").unwrap()
        );
        assert!(thread.is_falling_back);
    }

    #[test]
    fn transaction_event_without_id_renders_normally() {
        let queue = MessageQueue::new();

        assert!(matches!(
            take_transaction_event(&queue, None),
            TransactionEventHandling::RenderNormally
        ));
    }

    #[test]
    fn unmatched_transaction_event_renders_normally() {
        let queue = MessageQueue::new();
        let queued_id = TransactionId::new();
        let other_id = TransactionId::new();

        queue.add(queued_id.clone(), text_content("queued message"));

        assert!(matches!(
            take_transaction_event(&queue, Some(&other_id)),
            TransactionEventHandling::RenderNormally
        ));
        assert!(matches!(
            take_transaction_event(&queue, Some(&queued_id)),
            TransactionEventHandling::LocalEcho { .. }
        ));
    }

    #[test]
    fn matched_transaction_event_is_consumed_once() {
        let queue = MessageQueue::new();
        let transaction_id = TransactionId::new();

        queue.add_with_echo(transaction_id.clone(), text_content("local echo"));

        match take_transaction_event(&queue, Some(&transaction_id)) {
            TransactionEventHandling::LocalEcho { echo, .. } => {
                assert!(echo);
            }
            TransactionEventHandling::RenderNormally => {
                panic!("expected the queued local echo to be consumed");
            }
        }

        assert!(matches!(
            take_transaction_event(&queue, Some(&transaction_id)),
            TransactionEventHandling::RenderNormally
        ));
    }

    #[test]
    fn send_response_reserves_event_until_render_finishes() {
        let queue = MessageQueue::new();
        let transaction_id = TransactionId::new();
        let event_id = EventId::parse("$event:example.org").unwrap();

        queue.add(transaction_id.clone(), text_content("sent message"));
        assert!(queue.start_response(&transaction_id, &event_id).is_some());
        assert!(queue.response_in_progress(&event_id));
        assert!(matches!(
            take_transaction_event(&queue, Some(&transaction_id)),
            TransactionEventHandling::RenderNormally
        ));

        queue.finish_response(&event_id);
        assert!(!queue.response_in_progress(&event_id));
    }
}
