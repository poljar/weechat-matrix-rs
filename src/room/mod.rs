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
    time::Duration,
};

use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use matrix_sdk::{
    async_trait,
    attachment::{
        AttachmentConfig, AttachmentInfo, BaseFileInfo, BaseImageInfo,
    },
    deserialized_responses::AmbiguityChange,
    room::{
        reply::{EnforceThread, Reply},
        IncludeRelations, RelationsOptions, Room,
    },
    ruma::{
        api::Direction,
        events::{
            relation::{RelationType, Thread},
            room::{
                encrypted::{
                    EncryptedEventScheme, Relation as EncryptedRelation,
                    RoomEncryptedEventContent,
                },
                member::RoomMemberEventContent,
                message::{
                    MessageType, Relation, ReplyWithinThread,
                    RoomMessageEventContent, TextMessageEventContent,
                },
                redaction::SyncRoomRedactionEvent,
            },
            AnyMessageLikeEventContent, AnySyncMessageLikeEvent,
            AnySyncStateEvent, AnySyncTimelineEvent, AnyTimelineEvent,
            OriginalSyncMessageLikeEvent, SyncMessageLikeEvent, SyncStateEvent,
        },
        serde::Raw,
        EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomAliasId,
        OwnedRoomId, OwnedTransactionId, OwnedUserId, RoomId, TransactionId,
        UInt, UserId,
    },
    StoreError,
};

use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferHandle, BufferInputCallbackAsync,
    },
    Prefix, Weechat,
};

use crate::{
    config::{Config, RedactionStyle},
    connection::Connection,
    render::{Render, RenderedEvent, ReplyContext},
    server::{InnerServer, MatrixServer, ThreadRoute},
    thread_continuation::ThreadKey,
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
    Backwards(Option<String>),
}

#[derive(Debug, Eq, PartialEq)]
pub enum HistoryPageResult {
    Page { added: usize, exhausted: bool },
    Unavailable,
    Busy,
    Failed,
}

const HISTORY_PAGE_TAGS: [&str; 2] =
    ["matrix_history_page", "matrix_smart_filter"];
const RESTORED_HISTORY_BATCH_SIZE: u16 = 25;
const INTERACTIVE_HISTORY_BATCH_SIZE: u16 = 25;
const INTERACTIVE_HISTORY_MAX_PAGES: usize = 50;

// Restore one current page eagerly. Further pages are user-driven so a noisy
// room cannot monopolize its history lock while the GUI is asking for older
// messages in the room the user is actually viewing.
const RESTORED_HISTORY_TARGET_LINES: i32 = 1;
const RESTORED_HISTORY_MAX_PAGES: usize = 10;

fn restored_prev_batch(_prev_batch: Option<String>) -> Option<PrevBatch> {
    // The SDK does not replay the stored sync timeline when a room is restored.
    // Its last_prev_batch token points before that timeline, so using it here
    // skips the newest messages entirely. Start at the current room end; the
    // response's end token will drive older backward pagination afterwards.
    Some(PrevBatch::Backwards(None))
}

fn should_continue_restored_history(
    lines_before: i32,
    lines_after: i32,
    has_older_page: bool,
) -> bool {
    has_older_page
        && lines_after > lines_before
        && lines_after < RESTORED_HISTORY_TARGET_LINES
}

fn has_history_page(prev_batch: &Option<PrevBatch>) -> bool {
    prev_batch.is_some()
}

fn next_history_page_state(
    current: &PrevBatch,
    end: Option<String>,
    _added: usize,
) -> (Option<PrevBatch>, bool) {
    if let PrevBatch::Forward(token) = current {
        return (Some(PrevBatch::Backwards(Some(token.clone()))), false);
    }

    let repeated_cursor = matches!(
        (current, end.as_deref()),
        (PrevBatch::Backwards(Some(current)), Some(next)) if current == next
    );
    if end.is_none() || repeated_cursor {
        (None, true)
    } else {
        (Some(PrevBatch::Backwards(end)), false)
    }
}

fn history_page_marker(result: &HistoryPageResult) -> String {
    match result {
        HistoryPageResult::Page { added, exhausted } => format!(
            "matrix_history_page added={} exhausted={}",
            added,
            u8::from(*exhausted),
        ),
        HistoryPageResult::Unavailable => {
            "matrix_history_page added=0 exhausted=1 state=unavailable"
                .to_owned()
        }
        HistoryPageResult::Busy => {
            "matrix_history_page added=0 exhausted=0 state=busy".to_owned()
        }
        HistoryPageResult::Failed => {
            "matrix_history_page added=0 exhausted=0 state=failed".to_owned()
        }
    }
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
    server: std::rc::Weak<InnerServer>,
    buffer: RoomBuffer,

    config: Rc<RefCell<Config>>,
    connection: Rc<RefCell<Option<Connection>>>,

    messages_in_flight: IntMutex,
    prev_batch: Rc<RefCell<Option<PrevBatch>>>,
    latest_event_id: Rc<RefCell<Option<OwnedEventId>>>,
    latest_read_event_id: Rc<RefCell<Option<OwnedEventId>>>,
    latest_thread_event_ids: Rc<RefCell<HashMap<OwnedEventId, OwnedEventId>>>,
    thread_history_in_flight: Rc<RefCell<HashSet<OwnedEventId>>>,
    thread_history_loaded: Rc<RefCell<HashSet<OwnedEventId>>>,
    pending_encrypted_events:
        Rc<RefCell<HashMap<OwnedEventId, PendingEncryptedEvent>>>,
    pending_encrypted_recoveries:
        Rc<RefCell<HashSet<EncryptedMessageRecovery>>>,

    outgoing_messages: MessageQueue,

    members: Members,
    verification: Verification,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolvedReplyContext {
    sender: String,
    body: Option<String>,
}

fn reply_event_details(
    event: AnySyncTimelineEvent,
) -> Option<(OwnedUserId, Option<String>)> {
    let AnySyncTimelineEvent::MessageLike(event) = event else {
        return None;
    };
    let sender = event.sender().to_owned();
    let body = match event {
        AnySyncMessageLikeEvent::RoomMessage(
            SyncMessageLikeEvent::Original(event),
        ) => Some(event.content.msgtype.body().to_owned()),
        _ => None,
    };

    Some((sender, body))
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct EncryptedMessageRecovery {
    room_id: OwnedRoomId,
    session_id: String,
}

impl EncryptedMessageRecovery {
    fn from_content(
        room_id: &RoomId,
        content: &RoomEncryptedEventContent,
    ) -> Option<Self> {
        match &content.scheme {
            EncryptedEventScheme::MegolmV1AesSha2(content) => Some(Self {
                room_id: room_id.to_owned(),
                session_id: content.session_id.clone(),
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingEncryptedEvent {
    event: Raw<OriginalSyncMessageLikeEvent<RoomEncryptedEventContent>>,
    recovery: Option<EncryptedMessageRecovery>,
}

fn pending_encrypted_event_raw(
    event_id: &EventId,
    sender: &UserId,
    origin_server_ts: MilliSecondsSinceUnixEpoch,
    content: &RoomEncryptedEventContent,
) -> Option<Raw<OriginalSyncMessageLikeEvent<RoomEncryptedEventContent>>> {
    serde_json::to_string(&serde_json::json!({
        "content": content,
        "event_id": event_id,
        "origin_server_ts": origin_server_ts,
        "sender": sender,
        "type": "m.room.encrypted",
        "unsigned": {},
    }))
    .ok()
    .and_then(|json| Raw::from_json_string(json).ok())
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

fn make_emote_message_content(
    body: String,
    thread_root: Option<OwnedEventId>,
    latest_thread_event: Option<OwnedEventId>,
) -> RoomMessageEventContent {
    let mut content = RoomMessageEventContent::emote_plain(body);

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

fn with_mentions(
    mut content: RoomMessageEventContent,
    mentioned_user_ids: Vec<OwnedUserId>,
) -> RoomMessageEventContent {
    content.mentions = Some(matrix_sdk::ruma::events::Mentions::with_user_ids(
        mentioned_user_ids,
    ));
    content
}

pub(crate) fn thread_root_from_buffer(buffer: &Buffer) -> Option<OwnedEventId> {
    buffer
        .get_localvar("thread_root")
        .and_then(|thread_root| EventId::parse(thread_root.as_ref()).ok())
}

fn attachment_config(
    info: AttachmentInfo,
    thread_root: Option<OwnedEventId>,
) -> AttachmentConfig {
    let config = AttachmentConfig::new().info(info);

    if let Some(event_id) = thread_root {
        config.reply(Some(Reply {
            event_id,
            enforce_thread: EnforceThread::Threaded(ReplyWithinThread::No),
        }))
    } else {
        config
    }
}

fn thread_root_from_content(
    content: &RoomMessageEventContent,
) -> Option<&EventId> {
    match content.relates_to.as_ref() {
        Some(Relation::Thread(thread)) => Some(&thread.event_id),
        _ => None,
    }
}

fn thread_root_from_encrypted_content(
    content: &RoomEncryptedEventContent,
) -> Option<&EventId> {
    match content.relates_to.as_ref() {
        Some(EncryptedRelation::Thread(thread)) => Some(&thread.event_id),
        _ => None,
    }
}

fn retarget_thread_content(
    content: &mut RoomMessageEventContent,
    thread_root: OwnedEventId,
) {
    content.relates_to = Some(Relation::Thread(Thread::plain(
        thread_root.clone(),
        thread_root,
    )));
}

fn thread_root_from_event(
    event: &AnySyncMessageLikeEvent,
) -> Option<OwnedEventId> {
    event.original_content().and_then(|content| match content {
        AnyMessageLikeEventContent::RoomMessage(content) => {
            thread_root_from_content(&content).map(ToOwned::to_owned)
        }
        AnyMessageLikeEventContent::RoomEncrypted(content) => {
            thread_root_from_encrypted_content(&content).map(ToOwned::to_owned)
        }
        _ => None,
    })
}

fn rendered_root_to_seed<'a>(
    event_id: Option<&'a EventId>,
    thread_root: Option<&EventId>,
) -> Option<&'a EventId> {
    if thread_root.is_none() {
        event_id
    } else {
        None
    }
}

fn thread_root_from_timeline_event(
    event: &AnyTimelineEvent,
) -> Option<OwnedEventId> {
    match event {
        AnyTimelineEvent::MessageLike(event) => {
            event.original_content().and_then(|content| match content {
                AnyMessageLikeEventContent::RoomMessage(content) => {
                    thread_root_from_content(&content).map(ToOwned::to_owned)
                }
                _ => None,
            })
        }
        AnyTimelineEvent::State(_) => None,
    }
}

fn thread_history_page_is_complete(
    event_count: usize,
    next_batch_token: Option<&str>,
) -> bool {
    event_count == 0 || next_batch_token.is_none()
}

impl RoomHandle {
    pub fn new(
        server_name: &str,
        runtime: Handle,
        server: std::rc::Weak<InnerServer>,
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
            prev_batch: Rc::new(RefCell::new(restored_prev_batch(
                sdk_room.last_prev_batch(),
            ))),
            latest_event_id: Rc::new(RefCell::new(None)),
            latest_read_event_id: Rc::new(RefCell::new(None)),
            latest_thread_event_ids: Rc::new(RefCell::new(HashMap::new())),
            thread_history_in_flight: Rc::new(RefCell::new(HashSet::new())),
            thread_history_loaded: Rc::new(RefCell::new(HashSet::new())),
            pending_encrypted_events: Rc::new(RefCell::new(HashMap::new())),
            pending_encrypted_recoveries: Rc::new(RefCell::new(HashSet::new())),
            own_user_id: own_user_id.into(),
            members,
            buffer,
            verification,
            outgoing_messages: MessageQueue::new(),
            messages_in_flight: IntMutex::new(),
            room,
            server,
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
        buffer.set_localvar("matrix_upload_v1", "1");
        let sdk_room = room.room();
        buffer.set_localvar(
            "matrix_predecessor_room_id",
            sdk_room
                .predecessor_room()
                .as_ref()
                .map(|predecessor| predecessor.room_id.as_str())
                .unwrap_or_default(),
        );
        buffer.set_localvar(
            "matrix_replacement_room_id",
            sdk_room
                .successor_room()
                .as_ref()
                .map(|successor| successor.room_id.as_str())
                .unwrap_or_default(),
        );
        let room_avatar = room.room().avatar_url();
        buffer.set_localvar(
            "matrix_avatar_mxc",
            room_avatar
                .as_ref()
                .map(|uri| uri.as_str())
                .unwrap_or_default(),
        );
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
        server: std::rc::Weak<InnerServer>,
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
            server,
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
        room_buffer.members.update_member_localvars();

        room_buffer.buffer.update_buffer_name();
        room_buffer.buffer.set_topic();
        room_buffer.buffer.update_parent_spaces();

        Ok(room_buffer)
    }
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, buffer: BufferHandle, input: String) {
        let thread_root = buffer.upgrade().ok().and_then(|buffer| {
            buffer
                .get_localvar("matrix_continuation_source_thread_root")
                .and_then(|root| EventId::parse(root.as_ref()).ok())
                .or_else(|| thread_root_from_buffer(&buffer))
        });
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
    fn server(&self) -> Option<MatrixServer> {
        self.server.upgrade().map(|inner| MatrixServer { inner })
    }

    fn adopt_thread_buffer(
        &self,
        source_root: &EventId,
        target: &RoomHandle,
        target_root: &EventId,
    ) {
        let Some(handle) = self.buffer.thread_buffer(source_root) else {
            return;
        };
        self.buffer.remove_thread_buffer(source_root);
        target
            .buffer
            .set_thread_buffer(target_root.to_owned(), handle.clone());
        if let Ok(buffer) = handle.upgrade() {
            buffer.set_localvar(
                "matrix_continuation_source_room_id",
                self.room_id.as_str(),
            );
            buffer.set_localvar(
                "matrix_continuation_source_thread_root",
                source_root.as_str(),
            );
            buffer.set_localvar("room_id", target.room_id().as_str());
            buffer.set_localvar("thread_root", target_root.as_str());
            target.buffer.seed_thread_buffer(target_root, &buffer);
            target.buffer.sort_thread_messages(target_root);
        }
        target.fetch_thread_history(target_root.to_owned());
    }

    pub(crate) fn mention_message_content(
        &self,
        buffer: &Buffer,
        input: String,
        mentioned_user_ids: Vec<OwnedUserId>,
    ) -> RoomMessageEventContent {
        let thread_root = thread_root_from_buffer(buffer);
        let latest_thread_event = thread_root.as_ref().and_then(|root| {
            self.latest_thread_event_ids.borrow().get(root).cloned()
        });
        with_mentions(
            make_text_message_content(
                input,
                self.config.borrow().input().markdown_input(),
                thread_root,
                latest_thread_event,
            ),
            mentioned_user_ids,
        )
    }

    pub fn owns_buffer(&self, buffer: &Buffer) -> bool {
        self.buffer.owns_buffer(buffer)
    }

    pub async fn send_emote(&self, buffer: &Buffer<'_>, body: String) {
        let thread_root = thread_root_from_buffer(buffer);
        let latest_thread_event = thread_root.as_ref().and_then(|root| {
            self.latest_thread_event_ids.borrow().get(root).cloned()
        });
        let content =
            make_emote_message_content(body, thread_root, latest_thread_event);

        self.send_message(content).await;
    }

    pub fn open_thread_buffer(
        &self,
        thread_root: &EventId,
    ) -> Option<BufferHandle> {
        if !self.buffer.contains_event(thread_root) {
            return None;
        }

        let buffer = self.get_or_create_thread_buffer(thread_root);
        if buffer.is_some() {
            self.resume_thread_continuation(thread_root.to_owned());
        }
        buffer
    }

    fn resume_thread_continuation(&self, source_root: OwnedEventId) {
        let source = RoomHandle {
            inner: self.clone(),
        };
        Weechat::spawn(async move {
            let Some(server) = source.server() else {
                return;
            };
            let _guard = server.thread_continuation_send_guard().await;
            let Ok(ThreadRoute::Current { room, thread_root }) =
                server.resolve_thread_route(&source, &source_root).await
            else {
                return;
            };
            if room.room_id() != source.room_id() || thread_root != source_root
            {
                source.adopt_thread_buffer(&source_root, &room, &thread_root);
            }
        })
        .detach();
    }

    pub fn close_thread_buffer(&self, buffer: &Buffer) -> bool {
        if let Some(thread_root) = self.buffer.thread_root_for_buffer(buffer) {
            self.buffer.remove_thread_buffer(&thread_root);
            self.thread_history_in_flight
                .borrow_mut()
                .remove(&thread_root);
            self.thread_history_loaded.borrow_mut().remove(&thread_root);
            self.latest_thread_event_ids
                .borrow_mut()
                .remove(&thread_root);
            buffer.close();
            true
        } else {
            false
        }
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

    pub fn buffer_short_names(&self) -> Vec<String> {
        self.buffer.short_names()
    }

    pub fn buffer_handle_for_short_name(
        &self,
        short_name: &str,
    ) -> Option<BufferHandle> {
        self.buffer.buffer_handle_for_short_name(short_name)
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

        self.buffer.redact_event_lines(
            &event_id_tag,
            &tag,
            redact_first_line,
            redact_string,
        );
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

                        Some((in_reply_to.event_id.clone(), sender))
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

                if let Some((event_id, mut reply_sender)) = reply_to {
                    let threshold = self
                        .config
                        .borrow()
                        .look()
                        .reply_full_quote_threshold();
                    let context = reply_context_for_distance(
                        threshold,
                        self.buffer.reply_line_distance(&event_id),
                    );
                    let needs_remote_context = reply_sender.is_none()
                        || !rendered.has_reply_fallback();
                    let remote_context = if needs_remote_context {
                        self.load_reply_context(&event_id).await
                    } else {
                        None
                    };

                    if reply_sender.is_none() {
                        reply_sender = remote_context
                            .as_ref()
                            .map(|context| context.sender.clone());
                    }

                    let fetched_body = if rendered.has_reply_fallback() {
                        None
                    } else {
                        remote_context
                            .as_ref()
                            .and_then(|context| context.body.as_deref())
                    };

                    rendered.add_reply_context(
                        &event_id,
                        reply_sender.as_deref(),
                        context,
                        fetched_body,
                    )
                } else {
                    rendered
                }
            }
            _ => return None,
        };

        Some(rendered)
    }

    async fn load_reply_context(
        &self,
        event_id: &EventId,
    ) -> Option<ResolvedReplyContext> {
        let connection = self.connection.borrow().as_ref().cloned()?;
        let room = self.room();
        let event_id = event_id.to_owned();
        let timeline_event = connection
            .spawn(
                async move { room.load_or_fetch_event(&event_id, None).await },
            )
            .await
            .ok()?;
        let event = timeline_event.raw().deserialize().ok()?;
        let (sender_id, body) = reply_event_details(event)?;
        let sender = self
            .members
            .get(&sender_id)
            .await
            .map(|member| member.nick())
            .unwrap_or_else(|| sender_id.as_str().to_owned());

        Some(ResolvedReplyContext { sender, body })
    }

    fn get_or_create_thread_buffer(
        &self,
        thread_root: &EventId,
    ) -> Option<BufferHandle> {
        if let Some(handle) = self.buffer.thread_buffer(thread_root) {
            if handle.upgrade().is_ok() {
                self.buffer.seed_open_thread_buffer(thread_root);
                self.buffer.sort_thread_messages(thread_root);
                if !self.thread_history_loaded.borrow().contains(thread_root)
                    && !self
                        .thread_history_in_flight
                        .borrow()
                        .contains(thread_root)
                {
                    self.fetch_thread_history(thread_root.to_owned());
                }
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
        buffer.set_localvar("matrix_upload_v1", "1");
        buffer.set_title(&format!(
            "Thread {} in {}",
            thread_root, room_short_name
        ));

        self.buffer.seed_thread_buffer(thread_root, &buffer);
        self.buffer
            .set_thread_buffer(thread_root.to_owned(), buffer_handle.clone());

        self.fetch_thread_history(thread_root.to_owned());

        Some(buffer_handle)
    }

    fn fetch_thread_history(&self, thread_root: OwnedEventId) {
        let room = self.clone();
        Weechat::spawn(async move {
            room.get_thread_messages(thread_root).await;
        })
        .detach();
    }

    fn print_rendered_event_for_relation(
        &self,
        event_id: Option<&EventId>,
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

            if let Some(thread_root) =
                rendered_root_to_seed(event_id, thread_root)
            {
                self.buffer.seed_open_thread_buffer(thread_root);
            }
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

    fn note_pending_encrypted_event(
        &self,
        event_id: &EventId,
        sender: &UserId,
        origin_server_ts: MilliSecondsSinceUnixEpoch,
        content: &RoomEncryptedEventContent,
    ) -> Option<EncryptedMessageRecovery> {
        let recovery = EncryptedMessageRecovery::from_content(
            self.room_id.as_ref(),
            content,
        );
        let raw = pending_encrypted_event_raw(
            event_id,
            sender,
            origin_server_ts,
            content,
        )?;

        self.pending_encrypted_events.borrow_mut().insert(
            event_id.to_owned(),
            PendingEncryptedEvent {
                event: raw,
                recovery: recovery.clone(),
            },
        );
        recovery
    }

    fn maybe_request_missing_encrypted_event_recovery(
        &self,
        event_id: &EventId,
        sender: &UserId,
        origin_server_ts: MilliSecondsSinceUnixEpoch,
        content: &RoomEncryptedEventContent,
    ) {
        let Some(recovery) = self.note_pending_encrypted_event(
            event_id,
            sender,
            origin_server_ts,
            content,
        ) else {
            return;
        };

        if !self
            .pending_encrypted_recoveries
            .borrow_mut()
            .insert(recovery.clone())
        {
            return;
        }

        let room = self.clone();
        Weechat::spawn(async move {
            room.download_missing_room_key(recovery).await;
        })
        .detach();
    }

    async fn download_missing_room_key(
        &self,
        recovery: EncryptedMessageRecovery,
    ) {
        let Some(connection) = self.connection.borrow().as_ref().cloned()
        else {
            return;
        };
        let client = connection.client().clone();
        let room_id = recovery.room_id.clone();
        let session_id = recovery.session_id.clone();

        match connection
            .spawn(async move {
                client
                    .encryption()
                    .backups()
                    .download_room_key(&room_id, &session_id)
                    .await
            })
            .await
        {
            Ok(true) => {
                self.retry_pending_encrypted_events_for_session(
                    &recovery.room_id,
                    &recovery.session_id,
                )
                .await;
            }
            Ok(false) => {}
            Err(error) => {
                trace!(
                    room_id = %recovery.room_id,
                    session_id = %recovery.session_id,
                    ?error,
                    "Unable to download a missing room key from backup"
                );
            }
        }
    }

    async fn retry_pending_encrypted_event(&self, event_id: &EventId) -> bool {
        let Some(pending) = self
            .pending_encrypted_events
            .borrow()
            .get(event_id)
            .cloned()
        else {
            return false;
        };
        let Some(connection) = self.connection.borrow().as_ref().cloned()
        else {
            return false;
        };
        let room = self.room();
        let raw = pending.event.clone();
        let timeline = match connection
            .spawn(async move { room.decrypt_event(&raw, None).await })
            .await
        {
            Ok(timeline) => timeline,
            Err(error) => {
                trace!(%event_id, ?error, "Unable to retry room event decryption");
                return false;
            }
        };
        let event: AnySyncTimelineEvent = match timeline.raw().deserialize() {
            Ok(event) => event,
            Err(error) => {
                trace!(%event_id, ?error, "Unable to deserialize retried decrypted event");
                return false;
            }
        };
        let AnySyncTimelineEvent::MessageLike(event) = event else {
            return false;
        };
        if matches!(
            event.original_content(),
            Some(AnyMessageLikeEventContent::RoomEncrypted(_))
        ) {
            return false;
        }
        let Some(rendered) = self.render_sync_message(&event).await else {
            return false;
        };
        if !self.buffer.replace_event(event_id, rendered) {
            trace!(%event_id, "Unable to find a pending encrypted event in the room buffers");
            return false;
        }

        self.pending_encrypted_events.borrow_mut().remove(event_id);
        if let Some(recovery) = pending.recovery {
            let still_pending =
                self.pending_encrypted_events.borrow().values().any(
                    |pending| pending.recovery.as_ref() == Some(&recovery),
                );
            if !still_pending {
                self.pending_encrypted_recoveries
                    .borrow_mut()
                    .remove(&recovery);
            }
        }
        true
    }

    pub async fn retry_pending_encrypted_events_for_session(
        &self,
        room_id: &RoomId,
        session_id: &str,
    ) {
        let event_ids: Vec<OwnedEventId> = self
            .pending_encrypted_events
            .borrow()
            .iter()
            .filter(|(_, pending)| {
                pending.recovery.as_ref().is_some_and(|recovery| {
                    recovery.room_id == room_id
                        && recovery.session_id == session_id
                })
            })
            .map(|(event_id, _)| event_id.clone())
            .collect();

        for event_id in event_ids {
            self.retry_pending_encrypted_event(&event_id).await;
        }
    }

    pub async fn retry_all_pending_encrypted_events(&self) {
        let event_ids: Vec<OwnedEventId> = self
            .pending_encrypted_events
            .borrow()
            .keys()
            .cloned()
            .collect();

        for event_id in event_ids {
            self.retry_pending_encrypted_event(&event_id).await;
        }
    }

    /// Re-fetch encrypted events whose placeholder lines survived a WeeChat
    /// restart. The log keeps event IDs and tags, but not the raw ciphertext
    /// required by the Matrix crypto machine for a later retry.
    pub async fn recover_logged_encrypted_events(&self) {
        let event_ids = self.buffer.encrypted_event_ids();
        let Some(connection) = self.connection.borrow().as_ref().cloned()
        else {
            return;
        };

        for event_id in event_ids {
            let room = self.room();
            let requested_event_id = event_id.clone();
            let timeline = match connection
                .spawn(
                    async move { room.event(&requested_event_id, None).await },
                )
                .await
            {
                Ok(timeline) => timeline,
                Err(error) => {
                    trace!(%event_id, ?error, "Unable to restore encrypted event from its logged placeholder");
                    continue;
                }
            };
            let event: AnySyncTimelineEvent = match timeline.raw().deserialize()
            {
                Ok(event) => event,
                Err(error) => {
                    trace!(%event_id, ?error, "Unable to deserialize restored encrypted event");
                    continue;
                }
            };
            let AnySyncTimelineEvent::MessageLike(event) = event else {
                continue;
            };

            if let AnySyncMessageLikeEvent::RoomEncrypted(
                SyncMessageLikeEvent::Original(encrypted),
            ) = &event
            {
                self.maybe_request_missing_encrypted_event_recovery(
                    &encrypted.event_id,
                    &encrypted.sender,
                    encrypted.origin_server_ts,
                    &encrypted.content,
                );
            } else if let Some(rendered) =
                self.render_sync_message(&event).await
            {
                self.buffer.replace_event(&event_id, rendered);
            }
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
                    None,
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
    pub async fn send_message(&self, mut content: RoomMessageEventContent) {
        let Some(source_root) =
            thread_root_from_content(&content).map(ToOwned::to_owned)
        else {
            if let Err(error) = self.send_message_direct(content).await {
                self.print_error(&error);
            }
            return;
        };
        let Some(server) = self.server() else {
            if let Err(error) = self.send_message_direct(content).await {
                self.print_error(&error);
            }
            return;
        };
        let _guard = server.thread_continuation_send_guard().await;
        let route = match server
            .resolve_thread_route(
                &RoomHandle {
                    inner: self.clone(),
                },
                &source_root,
            )
            .await
        {
            Ok(route) => route,
            Err(error) => {
                self.print_error(&format!(
                    "Failed to continue archived Matrix thread: {error}"
                ));
                return;
            }
        };
        match route {
            ThreadRoute::Current { room, thread_root } => {
                if room.room_id() != self.room_id()
                    || thread_root != source_root
                {
                    self.adopt_thread_buffer(&source_root, &room, &thread_root);
                    retarget_thread_content(&mut content, thread_root);
                }
                if let Err(error) = room.send_message_direct(content).await {
                    self.print_error(&error);
                }
            }
            ThreadRoute::CreateRoot {
                room,
                continuation_source,
            } => {
                if let Err(error) =
                    server.verify_thread_continuation_store().await
                {
                    self.print_error(&format!(
                        "Cannot persist Matrix thread continuation: {error}"
                    ));
                    return;
                }
                content.relates_to = None;
                // Matrix assigns the event ID. The SDK store is proven writable
                // before sending, but the homeserver event and local mapping
                // cannot be committed atomically without persisting the full
                // arbitrary user payload as a recovery journal.
                match room.send_message_direct(content).await {
                    Ok(target_root) => {
                        let target_key =
                            ThreadKey::new(room.room_id(), &target_root);
                        if let Err(error) = server
                            .persist_thread_continuation(
                                continuation_source,
                                target_key,
                            )
                            .await
                        {
                            self.print_error(&format!("Sent continuation root but failed to persist it: {error}"));
                            return;
                        }
                        self.adopt_thread_buffer(
                            &source_root,
                            &room,
                            &target_root,
                        );
                    }
                    Err(error) => self.print_error(&error),
                }
            }
        }
    }

    async fn send_message_direct(
        &self,
        content: RoomMessageEventContent,
    ) -> Result<OwnedEventId, String> {
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
                    Ok(r.event_id)
                }
                Err(error) => {
                    // TODO: print out an error, remember to modify the local
                    // echo line if there is one.
                    self.outgoing_messages.remove(&transaction_id);
                    Err(format!("Failed to send Matrix message: {error}"))
                }
            }
        } else {
            Err("Matrix server is not connected".to_owned())
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

    pub async fn send_attachment(
        &self,
        path: PathBuf,
        thread_root: Option<OwnedEventId>,
    ) {
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
        let info = AttachmentInfo::File(BaseFileInfo { size });
        if let Err(error) = self
            .send_attachment_routed(
                filename,
                content_type,
                data,
                info,
                thread_root,
            )
            .await
        {
            self.print_error(&format!(
                "Failed to upload attachment {}: {error}",
                path.display()
            ));
        }
    }

    pub async fn send_attachment_bytes(
        &self,
        filename: String,
        content_type: mime::Mime,
        data: Vec<u8>,
        thread_root: Option<OwnedEventId>,
    ) {
        let size = UInt::new(data.len() as u64);
        let info = if content_type.type_() == mime::IMAGE {
            AttachmentInfo::Image(BaseImageInfo {
                size,
                ..Default::default()
            })
        } else {
            AttachmentInfo::File(BaseFileInfo { size })
        };
        if let Err(error) = self
            .send_attachment_routed(
                filename,
                content_type,
                data,
                info,
                thread_root,
            )
            .await
        {
            self.print_error(&format!(
                "Failed to upload Matrix attachment: {error}"
            ));
        }
    }

    async fn send_attachment_routed(
        &self,
        filename: String,
        content_type: mime::Mime,
        data: Vec<u8>,
        info: AttachmentInfo,
        source_root: Option<OwnedEventId>,
    ) -> Result<(), String> {
        let Some(source_root) = source_root else {
            self.send_attachment_direct(
                filename,
                content_type,
                data,
                info,
                None,
            )
            .await?;
            return Ok(());
        };
        let Some(server) = self.server() else {
            self.send_attachment_direct(
                filename,
                content_type,
                data,
                info,
                Some(source_root),
            )
            .await?;
            return Ok(());
        };
        let _guard = server.thread_continuation_send_guard().await;
        match server
            .resolve_thread_route(
                &RoomHandle {
                    inner: self.clone(),
                },
                &source_root,
            )
            .await?
        {
            ThreadRoute::Current { room, thread_root } => {
                if room.room_id() != self.room_id()
                    || thread_root != source_root
                {
                    self.adopt_thread_buffer(&source_root, &room, &thread_root);
                }
                room.send_attachment_direct(
                    filename,
                    content_type,
                    data,
                    info,
                    Some(thread_root),
                )
                .await?;
            }
            ThreadRoute::CreateRoot {
                room,
                continuation_source,
            } => {
                server.verify_thread_continuation_store().await?;
                let target_root = room
                    .send_attachment_direct(
                        filename,
                        content_type,
                        data,
                        info,
                        None,
                    )
                    .await?;
                server
                    .persist_thread_continuation(
                        continuation_source,
                        ThreadKey::new(room.room_id(), &target_root),
                    )
                    .await?;
                self.adopt_thread_buffer(&source_root, &room, &target_root);
            }
        }
        Ok(())
    }

    async fn send_attachment_direct(
        &self,
        filename: String,
        content_type: mime::Mime,
        data: Vec<u8>,
        info: AttachmentInfo,
        thread_root: Option<OwnedEventId>,
    ) -> Result<OwnedEventId, String> {
        let config = attachment_config(info, thread_root);
        let Some(connection) = self.connection.borrow().clone() else {
            return Err("Matrix server is not connected".to_owned());
        };
        connection
            .spawn({
                let room = self.room();
                async move {
                    room.send_attachment(filename, &content_type, data, config)
                        .await
                }
            })
            .await
            .map(|response| response.event_id)
            .map_err(|error| error.to_string())
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

    pub fn has_history_page(&self) -> bool {
        has_history_page(&self.prev_batch.borrow())
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

    pub async fn get_messages(&self) -> HistoryPageResult {
        self.get_messages_with_limit(RESTORED_HISTORY_BATCH_SIZE)
            .await
    }

    pub async fn get_interactive_history_page(&self) -> HistoryPageResult {
        let mut total_added = 0;

        for _ in 0..INTERACTIVE_HISTORY_MAX_PAGES {
            match self
                .get_messages_with_limit(INTERACTIVE_HISTORY_BATCH_SIZE)
                .await
            {
                HistoryPageResult::Page { added, exhausted } => {
                    total_added += added;
                    if total_added > 0 || exhausted {
                        return HistoryPageResult::Page {
                            added: total_added,
                            exhausted,
                        };
                    }
                }
                result => return result,
            }
        }

        HistoryPageResult::Page {
            added: total_added,
            exhausted: false,
        }
    }

    async fn get_messages_with_limit(&self, limit: u16) -> HistoryPageResult {
        let messages_lock = self.messages_in_flight.clone();

        let connection = self.connection.borrow().as_ref().cloned();

        let prev_batch =
            if let Some(p) = self.prev_batch.borrow().as_ref().cloned() {
                p
            } else {
                return HistoryPageResult::Unavailable;
            };

        let guard = if let Ok(l) = messages_lock.try_lock() {
            l
        } else {
            return HistoryPageResult::Busy;
        };

        Weechat::bar_item_update("buffer_modes");
        Weechat::bar_item_update("matrix_modes");

        let result = if let Some(connection) = connection {
            let room = self.room();
            let room_id = room.room_id().to_owned();

            if let Ok(r) = connection
                .room_messages(room, prev_batch.clone(), limit)
                .await
            {
                let fetched = r.chunk.len();
                let (next_prev_batch, exhausted) = next_history_page_state(
                    &prev_batch,
                    r.end.clone(),
                    fetched,
                );
                let mut added = 0;
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
                    added += self.handle_room_event(&event).await;
                }

                let mut prev_batch = self.prev_batch.borrow_mut();

                if matches!(prev_batch.as_ref(), Some(PrevBatch::Forward(_))) {
                    *prev_batch = next_prev_batch;
                    self.buffer.sort_messages();
                } else {
                    *prev_batch = next_prev_batch;
                    if added > 0 {
                        self.buffer.sort_messages();
                    }
                }

                HistoryPageResult::Page { added, exhausted }
            } else {
                HistoryPageResult::Failed
            }
        } else {
            HistoryPageResult::Failed
        };

        drop(guard);

        Weechat::bar_item_update("buffer_modes");
        Weechat::bar_item_update("matrix_modes");

        result
    }

    pub fn print_history_page_result(&self, result: HistoryPageResult) {
        if let Ok(buffer) = self.buffer.buffer_handle().upgrade() {
            buffer.print_date_tags(
                0,
                &HISTORY_PAGE_TAGS,
                &history_page_marker(&result),
            );
        }
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

    pub async fn preload_restored_messages(&self) {
        for _ in 0..RESTORED_HISTORY_MAX_PAGES {
            let buffer_handle = self.buffer_handle();
            let Ok(buffer) = buffer_handle.upgrade() else {
                return;
            };
            let lines_before = buffer.num_lines();
            drop(buffer);

            self.get_messages().await;

            let buffer_handle = self.buffer_handle();
            let Ok(buffer) = buffer_handle.upgrade() else {
                return;
            };
            let lines_after = buffer.num_lines();
            drop(buffer);

            let has_older_page = self.prev_batch.borrow().is_some();
            if !should_continue_restored_history(
                lines_before,
                lines_after,
                has_older_page,
            ) {
                break;
            }
        }
    }

    pub async fn get_thread_messages(&self, thread_root: OwnedEventId) {
        if self.thread_history_loaded.borrow().contains(&thread_root)
            || !self
                .thread_history_in_flight
                .borrow_mut()
                .insert(thread_root.clone())
        {
            return;
        }

        let mut from = None;
        let mut seen_tokens = HashSet::new();
        let mut seen_event_ids = HashSet::new();
        let mut newest_event_id = None;
        let mut completed = false;
        let mut request_attempts = 0u8;
        let Some(connection) = self.connection.borrow().as_ref().cloned()
        else {
            self.thread_history_in_flight
                .borrow_mut()
                .remove(&thread_root);
            return;
        };

        loop {
            let options = RelationsOptions {
                from: from.clone(),
                dir: Direction::Backward,
                limit: Some(UInt::from(100u8)),
                include_relations: IncludeRelations::RelationsOfType(
                    RelationType::Thread,
                ),
                recurse: false,
            };

            let room = self.room();
            let relation_root = thread_root.clone();
            let relations = match connection
                .spawn(
                    async move { room.relations(relation_root, options).await },
                )
                .await
            {
                Ok(relations) => relations,
                Err(error) => {
                    request_attempts += 1;
                    if request_attempts < 3 {
                        let delay = Duration::from_millis(
                            250 * u64::from(request_attempts),
                        );
                        connection
                            .spawn(
                                async move { tokio::time::sleep(delay).await },
                            )
                            .await;
                        continue;
                    }
                    Weechat::print(&format!(
                            "{}: Error fetching thread history for {} after {} attempts: {}",
                            Weechat::prefix(Prefix::Error),
                            thread_root,
                            request_attempts,
                            error,
                        ));
                    break;
                }
            };
            request_attempts = 0;

            let room_id = self.room_id.as_ref().to_owned();
            let mut new_event_count = 0;
            for event in relations
                .chunk
                .iter()
                .filter_map(|event| event.raw().deserialize().ok())
            {
                let event = event.into_full_event(room_id.clone());
                if !seen_event_ids.insert(event.event_id().to_owned()) {
                    continue;
                }
                new_event_count += 1;
                if newest_event_id.is_none() {
                    newest_event_id = Some(event.event_id().to_owned());
                }
                self.handle_thread_history_event(&thread_root, &event).await;
            }

            if thread_history_page_is_complete(
                new_event_count,
                relations.next_batch_token.as_deref(),
            ) {
                completed = true;
                break;
            }

            let Some(next) = relations.next_batch_token else {
                completed = true;
                break;
            };
            if !seen_tokens.insert(next.clone()) {
                Weechat::print(&format!(
                    "{}: Thread history pagination repeated a token for {}",
                    Weechat::prefix(Prefix::Error),
                    thread_root,
                ));
                break;
            }
            from = Some(next);
        }

        self.thread_history_in_flight
            .borrow_mut()
            .remove(&thread_root);

        if completed {
            self.thread_history_loaded
                .borrow_mut()
                .insert(thread_root.clone());
            if let Some(event_id) = newest_event_id {
                self.latest_thread_event_ids
                    .borrow_mut()
                    .entry(thread_root.clone())
                    .or_insert(event_id);
            }
            self.buffer.seed_open_thread_buffer(&thread_root);
            self.buffer.sort_thread_messages(&thread_root);
        }
    }

    async fn handle_thread_history_event(
        &self,
        thread_root: &EventId,
        event: &AnyTimelineEvent,
    ) {
        if thread_root_from_timeline_event(event).as_deref()
            != Some(thread_root)
        {
            return;
        }

        let AnyTimelineEvent::MessageLike(event) = event else {
            return;
        };

        if event.is_edit()
            || !should_render_event(
                self.buffer
                    .thread_contains_event(thread_root, event.event_id()),
            )
        {
            return;
        }

        let sender =
            self.members.get(event.sender()).await.expect(
                "Rendering a message but the sender isn't in the nicklist",
            );
        let Some(content) = event.original_content() else {
            tracing::error!("Unhandled redacted event: {event:?}");
            return;
        };

        if let Some(rendered) = self
            .render_message_content(
                event.event_id(),
                event.origin_server_ts(),
                &sender,
                &content,
            )
            .await
        {
            self.print_rendered_event_for_relation(
                Some(event.event_id()),
                Some(thread_root),
                rendered.add_backlog_tags(),
            );
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

            if let Some(thread_root) =
                rendered_root_to_seed(Some(event_id), thread_root.as_deref())
            {
                self.buffer.seed_open_thread_buffer(thread_root);
            }
        } else {
            self.print_rendered_event_for_relation(
                Some(event_id),
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

        // Keep encrypted events available for a later decryption retry even
        // when WeeChat restored their placeholder lines from its log already.
        // The normal render de-duplication below must not discard this state.
        if let AnySyncMessageLikeEvent::RoomEncrypted(
            SyncMessageLikeEvent::Original(encrypted),
        ) = event
        {
            self.maybe_request_missing_encrypted_event_recovery(
                &encrypted.event_id,
                &encrypted.sender,
                encrypted.origin_server_ts,
                &encrypted.content,
            );
        }

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
        } else {
            if let Some(rendered) = self.render_sync_message(event).await {
                let thread_root = thread_root_from_event(event);

                if let Some(thread_root) = &thread_root {
                    self.latest_thread_event_ids.borrow_mut().insert(
                        thread_root.clone(),
                        event.event_id().to_owned(),
                    );
                }

                self.print_rendered_event_for_relation(
                    Some(event.event_id()),
                    thread_root.as_deref(),
                    rendered,
                );
            }
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
                    restored_prev_batch(self.room().last_prev_batch());
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

    pub async fn handle_room_event(&self, event: &AnyTimelineEvent) -> usize {
        let thread_root = thread_root_from_timeline_event(event);

        match &event {
            AnyTimelineEvent::MessageLike(event) => {
                let already_rendered = thread_root.as_ref().map_or_else(
                    || self.buffer.contains_event(event.event_id()),
                    |thread_root| {
                        self.buffer.thread_contains_event(
                            thread_root,
                            event.event_id(),
                        )
                    },
                );
                let content = if let Some(content) = event.original_content() {
                    content
                } else {
                    tracing::error!("Unhandled redacted event: {event:?}");
                    return 0;
                };
                let send_time = event.origin_server_ts();

                // History can contain an event whose placeholder was restored
                // from a WeeChat log. Record it before render de-duplication so
                // a later room key can still replace the existing line.
                if let AnyMessageLikeEventContent::RoomEncrypted(encrypted) =
                    &content
                {
                    self.maybe_request_missing_encrypted_event_recovery(
                        event.event_id(),
                        event.sender(),
                        send_time,
                        encrypted,
                    );
                }

                // TODO: Only print out historical events if they aren't edits of
                // other events.
                if !event.is_edit() && should_render_event(already_rendered) {
                    let sender = self.members.get(event.sender()).await.expect(
                    "Rendering a message but the sender isn't in the nicklist",
                );

                    if let Some(rendered) = self
                        .render_message_content(
                            event.event_id(),
                            send_time,
                            &sender,
                            &content,
                        )
                        .await
                    {
                        let line_count = rendered.content.lines.len();
                        self.print_rendered_event_for_relation(
                            Some(event.event_id()),
                            thread_root.as_deref(),
                            rendered.add_backlog_tags(),
                        );
                        return line_count;
                    }
                }
            }
            // TODO: print out state events.
            AnyTimelineEvent::State(_) => (),
        }

        0
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
            AnySyncStateEvent::RoomAvatar(_) => {
                self.buffer.update_avatar();
                self.members.update_member_localvars();
            }
            AnySyncStateEvent::RoomName(_) => self.buffer.update_buffer_name(),
            AnySyncStateEvent::RoomTopic(_) => self.buffer.set_topic(),
            AnySyncStateEvent::RoomCanonicalAlias(_) => {
                self.buffer.set_alias();
                self.buffer.update_buffer_name();
            }
            AnySyncStateEvent::RoomTombstone(_) => {
                if let Ok(buffer) = self.buffer.buffer_handle().upgrade() {
                    buffer.set_localvar(
                        "matrix_replacement_room_id",
                        self.room()
                            .successor_room()
                            .as_ref()
                            .map(|successor| successor.room_id.as_str())
                            .unwrap_or_default(),
                    );
                }
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

fn reply_context_for_distance(
    threshold: i64,
    distance: Option<usize>,
) -> ReplyContext {
    distance
        .filter(|distance| threshold > 0 && *distance <= threshold as usize)
        .map(|_| ReplyContext::Inline)
        .unwrap_or(ReplyContext::Full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_content(body: &str) -> RoomMessageEventContent {
        RoomMessageEventContent::text_plain(body)
    }

    #[test]
    fn restored_rooms_fetch_newest_history_before_older_pages() {
        assert_eq!(
            restored_prev_batch(Some("token".to_owned())),
            Some(PrevBatch::Backwards(None))
        );
    }

    #[test]
    fn accepted_mentions_become_matrix_mentions() {
        let user_id = UserId::parse("@ada:example.org").unwrap();
        let content =
            with_mentions(text_content("hello @Ada"), vec![user_id.clone()]);

        let mentions = content.mentions.expect("m.mentions");
        assert!(mentions.user_ids.contains(&user_id));
        assert!(!mentions.room);

        let json = serde_json::to_value(with_mentions(
            text_content("hello @Ada"),
            vec![user_id],
        ))
        .unwrap();
        assert_eq!(json["m.mentions"]["user_ids"][0], "@ada:example.org");
    }

    #[test]
    fn restored_rooms_without_prev_batch_fetch_history_from_end() {
        assert_eq!(restored_prev_batch(None), Some(PrevBatch::Backwards(None)));
    }

    #[test]
    fn restored_history_releases_the_lock_after_one_useful_page() {
        assert!(!should_continue_restored_history(0, 13, true));
        assert!(!should_continue_restored_history(87, 99, true));
        assert!(!should_continue_restored_history(13, 13, true));
        assert!(!should_continue_restored_history(0, 13, false));
    }

    #[test]
    fn history_paging_requires_an_available_cursor() {
        assert!(!has_history_page(&None));
        assert!(has_history_page(&Some(PrevBatch::Backwards(None))));
        assert!(has_history_page(&Some(PrevBatch::Backwards(Some(
            "token".to_owned()
        )))));
        assert!(has_history_page(&Some(PrevBatch::Forward(
            "token".to_owned()
        ))));
    }

    #[test]
    fn empty_history_page_keeps_a_new_cursor() {
        assert_eq!(
            next_history_page_state(
                &PrevBatch::Backwards(Some("cursor-1".to_owned())),
                Some("cursor-2".to_owned()),
                0,
            ),
            (
                Some(PrevBatch::Backwards(Some("cursor-2".to_owned()))),
                false,
            ),
        );
    }

    #[test]
    fn empty_history_page_stops_on_a_repeated_cursor() {
        assert_eq!(
            next_history_page_state(
                &PrevBatch::Backwards(Some("cursor-1".to_owned())),
                Some("cursor-1".to_owned()),
                0,
            ),
            (None, true),
        );
    }

    #[test]
    fn non_empty_history_page_also_stops_on_a_repeated_cursor() {
        assert_eq!(
            next_history_page_state(
                &PrevBatch::Backwards(Some("cursor-1".to_owned())),
                Some("cursor-1".to_owned()),
                25,
            ),
            (None, true),
        );
    }

    #[test]
    fn interactive_history_pages_are_bounded() {
        assert_eq!(INTERACTIVE_HISTORY_BATCH_SIZE, 25);
        assert_eq!(RESTORED_HISTORY_BATCH_SIZE, 25);
    }

    #[test]
    fn history_page_markers_are_machine_readable_and_hideable() {
        assert_eq!(
            HISTORY_PAGE_TAGS,
            ["matrix_history_page", "matrix_smart_filter"]
        );
        assert_eq!(
            history_page_marker(&HistoryPageResult::Page {
                added: 25,
                exhausted: false,
            }),
            "matrix_history_page added=25 exhausted=0"
        );
        assert_eq!(
            history_page_marker(&HistoryPageResult::Unavailable),
            "matrix_history_page added=0 exhausted=1 state=unavailable"
        );
    }

    #[test]
    fn already_rendered_events_are_not_printed_again() {
        assert!(should_render_event(false));
        assert!(!should_render_event(true));
    }

    #[test]
    fn reply_event_details_extract_sender_and_body() {
        let event: AnySyncTimelineEvent =
            serde_json::from_value(serde_json::json!({
                "type": "m.room.message",
                "event_id": "$original:example.org",
                "sender": "@alice:example.org",
                "origin_server_ts": 1,
                "content": {
                    "msgtype": "m.text",
                    "body": "original message"
                },
                "unsigned": {}
            }))
            .expect("valid Matrix event");

        assert_eq!(
            reply_event_details(event),
            Some((
                UserId::parse("@alice:example.org").unwrap(),
                Some("original message".to_owned())
            ))
        );
    }

    #[test]
    fn room_image_attachment_has_native_image_metadata_without_reply() {
        let config = attachment_config(
            AttachmentInfo::Image(BaseImageInfo {
                size: UInt::new(42),
                ..Default::default()
            }),
            None,
        );

        assert!(matches!(config.info, Some(AttachmentInfo::Image(_))));
        assert!(config.reply.is_none());
    }

    #[test]
    fn thread_image_attachment_targets_the_thread_root() {
        let thread_root = EventId::parse("$thread-root:example.org").unwrap();
        let config = attachment_config(
            AttachmentInfo::Image(BaseImageInfo {
                size: UInt::new(42),
                ..Default::default()
            }),
            Some(thread_root.to_owned()),
        );

        let reply = config.reply.expect("thread image must carry a reply");
        assert_eq!(reply.event_id, thread_root);
        assert_eq!(
            reply.enforce_thread,
            EnforceThread::Threaded(ReplyWithinThread::No)
        );
    }

    #[test]
    fn zero_reply_quote_threshold_keeps_full_reply_context() {
        assert_eq!(ReplyContext::Full, reply_context_for_distance(0, Some(0)));
        assert_eq!(ReplyContext::Full, reply_context_for_distance(0, Some(1)));
    }

    #[test]
    fn positive_reply_quote_threshold_allows_inline_recent_replies() {
        assert_eq!(
            ReplyContext::Inline,
            reply_context_for_distance(2, Some(2))
        );
        assert_eq!(ReplyContext::Full, reply_context_for_distance(2, Some(3)));
        assert_eq!(ReplyContext::Full, reply_context_for_distance(2, None));
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
    fn thread_buffer_emote_sends_thread_relation() {
        let content = make_emote_message_content(
            "waves".to_owned(),
            Some(EventId::parse("$thread-root:example.org").unwrap()),
            Some(EventId::parse("$latest-thread-event:example.org").unwrap()),
        );

        assert!(matches!(content.msgtype, MessageType::Emote(_)));

        let Some(Relation::Thread(thread)) = content.relates_to else {
            panic!("thread buffer emote must send an m.thread relation");
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
    fn plain_event_can_seed_an_already_open_thread_buffer() {
        let event_id = EventId::parse("$thread-root:example.org").unwrap();

        assert_eq!(
            rendered_root_to_seed(Some(&event_id), None),
            Some(event_id.as_ref())
        );
    }

    #[test]
    fn continued_thread_text_targets_only_the_successor_root() {
        let old_root = EventId::parse("$old-root:example.org").unwrap();
        let new_root = EventId::parse("$new-root:example.org").unwrap();
        let mut content = make_text_message_content(
            "continued body".to_owned(),
            false,
            Some(old_root),
            None,
        );

        retarget_thread_content(&mut content, new_root.clone());

        let Some(Relation::Thread(thread)) = content.relates_to else {
            panic!("continued text must remain a proper Matrix thread reply");
        };
        assert_eq!(thread.event_id, new_root);
        assert_eq!(
            thread.in_reply_to.expect("fallback target").event_id,
            new_root
        );
    }

    #[test]
    fn historical_thread_event_keeps_its_root() {
        let event: AnyTimelineEvent =
            serde_json::from_value(serde_json::json!({
                "type": "m.room.message",
                "room_id": "!room:example.org",
                "sender": "@alice:example.org",
                "origin_server_ts": 1,
                "event_id": "$reply:example.org",
                "content": {
                    "msgtype": "m.text",
                    "body": "thread reply",
                    "m.relates_to": {
                        "rel_type": "m.thread",
                        "event_id": "$root:example.org",
                        "is_falling_back": true,
                        "m.in_reply_to": {
                            "event_id": "$root:example.org"
                        }
                    }
                },
                "unsigned": {}
            }))
            .unwrap();

        assert_eq!(
            thread_root_from_timeline_event(&event).as_deref(),
            Some(EventId::parse("$root:example.org").unwrap().as_ref())
        );
    }

    #[test]
    fn thread_reply_is_not_treated_as_another_thread_root() {
        let event_id = EventId::parse("$thread-reply:example.org").unwrap();
        let thread_root = EventId::parse("$thread-root:example.org").unwrap();

        assert_eq!(
            rendered_root_to_seed(Some(&event_id), Some(&thread_root)),
            None
        );
        assert_eq!(rendered_root_to_seed(None, Some(&thread_root)), None);
    }

    #[test]
    fn empty_thread_history_page_finishes_pagination() {
        assert!(thread_history_page_is_complete(0, Some("next")));
        assert!(thread_history_page_is_complete(0, None));
        assert!(thread_history_page_is_complete(8, None));
        assert!(!thread_history_page_is_complete(8, Some("next")));
    }

    #[test]
    fn encrypted_sync_thread_event_keeps_its_root() {
        let event: AnySyncTimelineEvent =
            serde_json::from_value(serde_json::json!({
                "type": "m.room.encrypted",
                "room_id": "!room:example.org",
                "sender": "@alice:example.org",
                "origin_server_ts": 1,
                "event_id": "$encrypted:example.org",
                "content": {
                    "algorithm": "m.megolm.v1.aes-sha2",
                    "ciphertext": "AwgAEoAB...",
                    "sender_key": "sender_key",
                    "device_id": "DEVICE",
                    "session_id": "session_id",
                    "m.relates_to": {
                        "rel_type": "m.thread",
                        "event_id": "$root:example.org",
                        "is_falling_back": true,
                        "m.in_reply_to": {
                            "event_id": "$root:example.org"
                        }
                    }
                },
                "unsigned": {}
            }))
            .unwrap();

        let AnySyncTimelineEvent::MessageLike(event) = event else {
            panic!("expected an encrypted message-like event");
        };

        assert_eq!(
            thread_root_from_event(&event).as_deref(),
            Some(EventId::parse("$root:example.org").unwrap().as_ref())
        );
    }

    #[test]
    fn megolm_encrypted_event_exposes_recovery_session() {
        let content: RoomEncryptedEventContent =
            serde_json::from_value(serde_json::json!({
                "algorithm": "m.megolm.v1.aes-sha2",
                "ciphertext": "AwgAEoAB...",
                "sender_key": "sender_key",
                "device_id": "DEVICE",
                "session_id": "session_id"
            }))
            .unwrap();

        let recovery = EncryptedMessageRecovery::from_content(
            RoomId::parse("!room:example.org").unwrap().as_ref(),
            &content,
        )
        .expect("megolm event should expose a recovery session");

        assert_eq!(recovery.room_id.as_str(), "!room:example.org");
        assert_eq!(recovery.session_id, "session_id");
    }

    #[test]
    fn pending_encrypted_event_keeps_decryptable_raw_shape() {
        let content: RoomEncryptedEventContent =
            serde_json::from_value(serde_json::json!({
                "algorithm": "m.megolm.v1.aes-sha2",
                "ciphertext": "AwgAEoAB...",
                "sender_key": "sender_key",
                "device_id": "DEVICE",
                "session_id": "session_id"
            }))
            .unwrap();
        let event_id = EventId::parse("$encrypted:example.org").unwrap();
        let sender = UserId::parse("@alice:example.org").unwrap();

        let raw = pending_encrypted_event_raw(
            &event_id,
            &sender,
            MilliSecondsSinceUnixEpoch(UInt::from(1_u32)),
            &content,
        )
        .expect("encrypted event should retain a raw decryption input");
        let event = raw.deserialize().expect("raw event should deserialize");

        assert_eq!(event.event_id, event_id);
        assert_eq!(event.sender, sender);
        assert!(matches!(
            event.content.scheme,
            EncryptedEventScheme::MegolmV1AesSha2(ref megolm)
                if megolm.session_id == "session_id"
        ));
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
