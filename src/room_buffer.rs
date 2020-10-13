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

use matrix_sdk::{
    events::{
        room::{
            member::{
                MemberEventContent,
                MembershipChange::{
                    Banned, InvitationRejected, InvitationRevoked, Invited,
                    Joined, Kicked, KickedAndBanned, Left, ProfileChanged,
                },
                MembershipState,
            },
            message::{MessageEventContent, TextMessageEventContent},
        },
        AnyMessageEventContent, AnyPossiblyRedactedSyncMessageEvent,
        AnyRedactedSyncMessageEvent, AnySyncMessageEvent, AnySyncRoomEvent,
        AnySyncStateEvent, SyncMessageEvent, SyncStateEvent,
    },
    identifiers::{EventId, RoomId, UserId},
    locks::{RwLock, RwLockReadGuard},
    uuid::Uuid,
    Room,
};
use url::Url;

use async_trait::async_trait;
use futures::executor::block_on;
use tracing::{debug, error, trace};

use crate::{
    config::Config,
    connection::{Connection, TYPING_NOTICE_TIMEOUT},
    render::{render_membership, Render, RenderedEvent},
};
use std::{
    borrow::Cow,
    ops::Deref,
    cell::RefCell,
    collections::HashMap,
    convert::TryFrom,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};
use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferHandle, BufferInputCallbackAsync,
        BufferLine, NickSettings,
    },
    Weechat,
};

#[derive(Clone)]
pub struct RoomBuffer {
    inner: MatrixRoom,
    buffer_handle: BufferHandle,
}

impl Deref for RoomBuffer {
    type Target = MatrixRoom;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Clone)]
pub struct MatrixRoom {
    homeserver: Rc<Url>,
    room_id: Rc<RoomId>,
    own_user_id: Rc<UserId>,
    room: Arc<RwLock<Room>>,

    config: Rc<RefCell<Config>>,
    connection: Rc<RefCell<Option<Connection>>>,

    typing_notice_time: Rc<RefCell<Option<Instant>>>,
    typing_in_flight: Rc<Mutex<()>>,

    outgoing_messages: MessageQueue,

    members: Members
}

#[derive(Clone)]
pub struct Members {
    room: Arc<RwLock<Room>>,
    inner: Rc<RefCell<HashMap<UserId, WeechatRoomMember>>>,
    buffer: Rc<RefCell<Option<BufferHandle>>>,
}

const BUFFER_CLOSED_ERROR: &str = "Buffer got closed but Room is still lingering around";

impl Members {
    pub fn new(room: Arc<RwLock<Room>>) -> Self {
        Self {
            room,
            inner: Rc::new(RefCell::new(HashMap::new())),
            buffer: Rc::new(RefCell::new(None)),
        }
    }

    pub fn buffer(&self) -> BufferHandle {
        self.buffer.borrow().as_ref().expect("Members struct wasn't initialized properly").clone()
    }

    /// Add a new Weechat room member.
    pub fn add(&self, member: WeechatRoomMember) {
        {
            let buffer = self.buffer();
            let buffer = buffer.upgrade().expect(BUFFER_CLOSED_ERROR);
            let nick = member.nick.borrow();
            let nick_settings = NickSettings::new(&nick);

            buffer.add_nick(nick_settings).unwrap_or_else(|_| {
                panic!("Error adding nick for {:#?}, already added?", member)
            });
        }

        self.inner
            .borrow_mut()
            .insert((&*member.user_id).clone(), member);
    }

    /// Remove a Weechat room member by user ID.
    ///
    /// Returns either the removed Weechat room member, or an error if the
    /// member does not exist.
    pub fn remove(
        &self,
        user_id: &UserId,
    ) -> Result<WeechatRoomMember, RoomError> {
        let buffer = self.buffer();
        let buffer = buffer.upgrade().expect(BUFFER_CLOSED_ERROR);

        if let Some(member) = self.inner.borrow_mut().remove(user_id) {
            buffer.remove_nick(&member.nick.borrow());
            Ok(member)
        } else {
            error!(
                "{}: Tried removing a non-existent Weechat room member: {}",
                buffer.name(),
                user_id
            );

            Err(RoomError::NonExistentMember(user_id.clone()))
        }
    }

    /// Retrieve a reference to a Weechat room member by user ID.
    pub fn get(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        self.inner.borrow().get(user_id).cloned()
    }

    /// Change nick of member.
    ///
    /// Returns either the old nick of the member, or an error if the member
    /// does not exist.
    pub fn rename_member(
        &self,
        user_id: &UserId,
        new_nick: String,
    ) -> Result<String, RoomError> {
        let buffer = self.buffer();
        let buffer = buffer.upgrade().expect(BUFFER_CLOSED_ERROR);

        if let Some(member) = self.inner.borrow().get(user_id) {
            trace!(
                "Renaming member from {} to {}",
                &member.nick.borrow(),
                &new_nick
            );

            buffer.remove_nick(&member.nick.borrow());

            let nick_settings = NickSettings::new(&new_nick);
            buffer
                .add_nick(nick_settings)
                .expect("Can't add nick to nicklist");

            let old_nick = member.nick.replace(new_nick);

            Ok(old_nick)
        } else {
            Err(RoomError::NonExistentMember(user_id.clone()))
        }
    }

    pub fn room(&self) -> RwLockReadGuard<'_, Room> {
        block_on(self.room.read())
    }

    pub fn calculate_buffer_name(&self) -> String {
        let room = self.room();
        let room_name = room.display_name();

        if room_name == "#" {
            "##".to_owned()
        } else if room_name.starts_with('#') {
            room_name
        } else {
            // TODO: only do this for non-direct chats
            format!("#{}", room_name)
        }
    }

    pub fn update_buffer_name(&self) {
        let name = self.calculate_buffer_name();
        let buffer = self.buffer();
        let buffer = buffer.upgrade().expect(BUFFER_CLOSED_ERROR);
        buffer.set_name(&name)
    }

    /// Helper method to calculate the display name of a room member from their
    /// UserId.
    ///
    /// If no member with that ID is in the room, the string representation of
    /// the ID will be returned.
    ///
    /// # Panics
    ///
    /// This panics if no member with the given user id can be found.
    fn calculate_user_name(&self, user_id: &UserId) -> String {
        self.room()
            .get_member(user_id)
            .unwrap_or_else(|| panic!("No such member {}", user_id))
            .disambiguated_name()
    }

    /// Process disambiguations received from the SDK.
    ///
    /// Disambiguations are a hashmap of user ID -> bool indicating that this
    /// user is either newly ambiguous (true) or no longer ambiguous (false).
    #[allow(dead_code)]
    fn process_disambiguations(
        &mut self,
        disambiguations: &HashMap<UserId, bool>,
    ) {
        for (affected_member, is_ambiguous) in disambiguations.iter() {
            if *is_ambiguous {
                let new_nick = self
                    .room()
                    .get_member(affected_member)
                    .unwrap()
                    .unique_name();

                match self.rename_member(affected_member, new_nick.clone()) {
                    Ok(old_nick) => debug!(
                        "{}: Disambiguating nick: {} -> {}",
                        self.calculate_buffer_name(),
                        old_nick,
                        &new_nick
                    ),
                    Err(RoomError::NonExistentMember(user_id)) => error!(
                        "{}: Tried disambiguating {} but they are not a member",
                        self.calculate_buffer_name(),
                        user_id
                    ),
                }
            } else {
                let new_nick =
                    self.room().get_member(affected_member).unwrap().name();

                match self.rename_member(affected_member, new_nick.clone()) {
                        Ok(old_nick) => debug!(
                            "{}: No longer disambiguating: {} -> {}",
                            self.calculate_buffer_name(),
                            old_nick,
                            &new_nick),
                        Err(RoomError::NonExistentMember(user_id)) => error!(
                            "{}: Tried removing disambiguation for {} but they are not a member",
                            self.calculate_buffer_name(),
                            user_id),
                    }
            }
        }
    }

    pub fn handle_membership_event(
        &self,
        event: &SyncStateEvent<MemberEventContent>,
        state_event: bool,
    ) {
        let buffer = self.buffer();
        let buffer = buffer.upgrade().expect(BUFFER_CLOSED_ERROR);

        let sender_id = event.sender.clone();
        let target_id;

        if let Ok(t) = UserId::try_from(event.state_key.clone()) {
            target_id = t;
        } else {
            error!(
                "Invalid state key given by the server: {}",
                event.state_key
            );
            return;
        }

        let new_nick = self.calculate_user_name(&target_id);

        if state_event {
            use MembershipState::*;

            // FIXME: Handle gaps (e.g. long disconnects) properly.
            //
            // For joins and invites, first we need to check whether a member
            // with some MXID exists. If he does, we have to update *that*
            // member with the new state. Only if they do not exist yet do we
            // create a new one.
            //
            // For leaves and bans we just need to remove the member.
            match event.content.membership {
                Invite | Join => {
                    // TODO remove this unwrap.
                    let display_name = self
                        .room()
                        .get_member(&target_id)
                        .unwrap()
                        .display_name
                        .clone();

                    self.add(WeechatRoomMember::new(
                        &target_id,
                        new_nick,
                        display_name,
                    ));
                }
                Leave | Ban => {
                    let _ = self.remove(&target_id);
                }
                _ => (),
            }

            // TODO enable this again once we receive the event via an event
            // emitter.
            // self.process_disambiguations(&disambiguations);

            // Names of rooms without display names can get affected by the
            // member list so we need to update them.
            self.update_buffer_name();
        } else {
            let change_op = event.membership_change();
            let sender;
            let target;

            match change_op {
                Joined | Invited => {
                    debug!(
                        "{}: User {} joining, adding nick {}",
                        self.calculate_buffer_name(),
                        target_id,
                        new_nick
                    );

                    // TODO remove this unwrap
                    let display_name = self
                        .room()
                        .get_member(&target_id)
                        .unwrap()
                        .display_name
                        .clone();

                    let member = WeechatRoomMember::new(
                        &target_id,
                        new_nick,
                        display_name,
                    );
                    self.add(member.clone());

                    sender = self.get(&sender_id);
                    target = Some(member);
                }

                Left | Banned | Kicked | KickedAndBanned
                | InvitationRejected | InvitationRevoked => {
                    sender = self.get(&sender_id);
                    target = self.get(&target_id);

                    match self.remove(&target_id) {
                        Ok(removed_member) => {
                            debug!(
                                "{}: User {} leaving, removing nick {}",
                                self.calculate_buffer_name(),
                                target_id,
                                removed_member.nick.borrow(),
                            );
                        }

                        Err(RoomError::NonExistentMember(user_id)) => {
                            error!(
                                "{}: User {} leaving, but he's not a member",
                                self.calculate_buffer_name(),
                                user_id
                            );
                        }
                    }
                }

                ProfileChanged {
                    displayname_changed,
                    avatar_url_changed,
                } => {
                    sender = self.get(&sender_id);
                    target = self.get(&target_id);

                    if displayname_changed {
                        match self.rename_member(&target_id, new_nick.clone()) {
                            Ok(old_nick) => debug!(
                                "{}: Profile changed for {}, renaming {} -> {}",
                                self.calculate_buffer_name(),
                                &target_id,
                                old_nick,
                                &new_nick
                            ),

                            Err(RoomError::NonExistentMember(user_id)) => error!(
                                "{}: Profile changed for {} but they are not a member",
                                self.calculate_buffer_name(),
                                user_id
                            ),
                        }

                        // TODO remove this unwrap
                        self.get(&target_id)
                            .unwrap()
                            .display_name
                            .replace(event.content.displayname.clone());
                    }

                    if avatar_url_changed {
                        debug!(
                            "{}: Avatar changed for {}, new avatar {:#?}",
                            self.calculate_buffer_name(),
                            &target_id,
                            event.content.avatar_url
                        );
                    }
                }
                _ => {
                    sender = self.get(&sender_id);
                    target = self.get(&target_id);
                }
            };

            // TODO enable this again once we receive the event via an event
            // emitter.
            // self.process_disambiguations(&disambiguations);

            // Names of rooms without display names can get affected by the member list so we need to
            // update them.
            self.update_buffer_name();

            // Display the event message
            let message = match (&sender, &target) {
                (Some(sender), Some(target)) => {
                    render_membership(event, sender, target)
                }

                _ => {
                    if sender.is_none() {
                        error!(
                            "Cannot render event since event sender {} is not a room member",
                            sender_id);
                    }

                    if target.is_none() {
                        error!(
                            "Cannot render event since event target {} is not a room member",
                            target_id);
                    }

                    "ERROR: cannot render event since sender or target are not a room member".into()
                }
            };

            let timestamp: u64 = event
                .origin_server_ts
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            buffer.print_date_tags(
                timestamp as i64,
                &[],
                &message,
            );
        }
    }
}

pub enum RoomError {
    NonExistentMember(UserId),
}

#[derive(Clone, Debug)]
pub struct WeechatRoomMember {
    pub user_id: Rc<UserId>,
    pub nick: Rc<RefCell<String>>,
    pub display_name: Rc<RefCell<Option<String>>>,
    pub prefix: Rc<RefCell<Option<String>>>,
    #[allow(clippy::rc_buffer)]
    pub color: Rc<String>,
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

impl WeechatRoomMember {
    pub fn new(
        user_id: &UserId,
        nick: String,
        display_name: Option<String>,
    ) -> Self {
        let color = Weechat::info_get("nick_color_name", user_id.as_str())
            .expect("Couldn't get the nick color name");

        WeechatRoomMember {
            user_id: Rc::new(user_id.clone()),
            nick: Rc::new(RefCell::new(nick)),
            display_name: Rc::new(RefCell::new(display_name)),
            prefix: Rc::new(RefCell::new(None)),
            color: Rc::new(color),
        }
    }
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, buffer_handle: BufferHandle, input: String) {
        // TODO parse the input here and produce a formatted body.
        let content = MessageEventContent::Text(TextMessageEventContent {
            body: input,
            formatted: None,
            relates_to: None,
        });

        self.send_message(buffer_handle, content).await;
    }
}

impl MatrixRoom {
    pub async fn send_message(
        &self,
        buffer: BufferHandle,
        content: MessageEventContent,
    ) {
        let uuid = Uuid::new_v4();

        if let Some(c) = &*self.connection.borrow() {
            self.queue_outgoing_message(buffer.clone(), uuid, &content);
            match c
                .send_message(
                    &self.room_id,
                    AnyMessageEventContent::RoomMessage(content),
                    Some(uuid),
                )
                .await
            {
                Ok(r) => {
                    self.handle_outgoing_message(buffer, uuid, &r.event_id);
                }
                Err(_e) => {
                    // TODO print out an error, remember to modify the local
                    // echo line if there is one.
                    self.outgoing_messages.remove(uuid);
                }
            }
        } else {
            let buffer = buffer
                .upgrade()
                .expect("Trying to send a message while the buffer is closed");

            buffer.print("Error not connected");
        }
    }

    // Add the content of the message to our outgoing messag queue and print out
    // a local echo line if local echo is enabled.
    pub fn queue_outgoing_message(
        &self,
        buffer_handle: BufferHandle,
        uuid: Uuid,
        content: &MessageEventContent,
    ) {
        if self.config.borrow().look().local_echo() {
            if let MessageEventContent::Text(c) = content {
                let sender =
                    self.members.get(&self.own_user_id).unwrap_or_else(|| {
                        panic!("No own member {}", self.own_user_id)
                    });

                if let Ok(b) = buffer_handle.upgrade() {
                    let local_echo =
                        c.render_with_prefix_for_echo(&sender, uuid, &());
                    self.print_rendered_event(&b, local_echo)
                }

                self.outgoing_messages.add_with_echo(uuid, content.clone());
            } else {
                self.outgoing_messages.add(uuid, content.clone());
            }
        } else {
            self.outgoing_messages.add(uuid, content.clone());
        }
    }

    pub fn print_rendered_event(
        &self,
        buffer: &Buffer,
        rendered: RenderedEvent,
    ) {
        for line in rendered.content.lines {
            let message = format!("{}\t{}", &rendered.prefix, &line.message);
            let tags: Vec<&str> =
                line.tags.iter().map(|t| t.as_str()).collect();
            buffer.print_date_tags(0, &tags, &message)
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

    fn handle_outgoing_message(
        &self,
        buffer_handle: BufferHandle,
        uuid: Uuid,
        event_id: &EventId,
    ) {
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
                .render_message_event(&event)
                .expect("Sent out an event that we don't know how to render");

            if let Ok(buffer) = buffer_handle.upgrade() {
                if echo {
                    self.replace_local_echo(uuid, &buffer, rendered);
                } else {
                    self.print_rendered_event(&buffer, rendered);
                }
            }
        }
    }

    pub fn room(&self) -> RwLockReadGuard<'_, Room> {
        block_on(self.room.read())
    }

    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    fn render_message_event(
        &self,
        event: &AnySyncMessageEvent,
    ) -> Option<RenderedEvent> {
        use AnyMessageEventContent::*;
        use MessageEventContent::*;

        // TODO remove this expect.
        let sender = self
            .members
            .get(event.sender())
            .expect("Rendering a message but the sender isn't in the nicklist");

        let send_time = event.origin_server_ts();

        let rendered = match event.content() {
            RoomEncrypted(c) => c.render_with_prefix(send_time, &sender, &()),
            RoomMessage(c) => match c {
                Text(c) => c.render_with_prefix(send_time, &sender, &()),
                Emote(c) => c.render_with_prefix(send_time, &sender, &sender),
                Notice(c) => c.render_with_prefix(send_time, &sender, &sender),
                ServerNotice(c) => {
                    c.render_with_prefix(send_time, &sender, &sender)
                }
                Location(c) => c.render_with_prefix(send_time, &sender, &sender),
                Audio(c) => {
                    c.render_with_prefix(send_time, &sender, &self.homeserver)
                }
                Video(c) => {
                    c.render_with_prefix(send_time, &sender, &self.homeserver)
                }
                File(c) => {
                    c.render_with_prefix(send_time, &sender, &self.homeserver)
                }
                Image(c) => {
                    c.render_with_prefix(send_time, &sender, &self.homeserver)
                }
            },
            _ => return None,
        };

        Some(rendered)
    }
}

impl RoomBuffer {
    pub fn new(
        connection: &Rc<RefCell<Option<Connection>>>,
        config: Rc<RefCell<Config>>,
        room: Arc<RwLock<Room>>,
        homeserver: &Url,
        room_id: RoomId,
        own_user_id: &UserId,
    ) -> Self {
        let members = Members::new(room.clone());
        let room = MatrixRoom {
            homeserver: Rc::new(homeserver.clone()),
            room_id: Rc::new(room_id.clone()),
            connection: connection.clone(),
            typing_notice_time: Rc::new(RefCell::new(None)),
            typing_in_flight: Rc::new(Mutex::new(())),
            config,
            room,
            own_user_id: Rc::new(own_user_id.to_owned()),
            members: members.clone(),
            outgoing_messages: MessageQueue::new(),
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

        room.members.buffer.borrow_mut().replace(buffer_handle.clone());

        RoomBuffer {
            inner: room,
            buffer_handle,
        }
    }

    pub fn restore(
        room: Arc<RwLock<Room>>,
        connection: &Rc<RefCell<Option<Connection>>>,
        config: Rc<RefCell<Config>>,
        homeserver: &Url,
    ) -> Self {
        let room_clone = room.clone();
        let room_lock = block_on(room.read());
        let room_id = room_lock.room_id.to_owned();
        let own_user_id = &room_lock.own_user_id;

        let room_buffer = RoomBuffer::new(
            connection,
            config,
            room_clone,
            homeserver,
            room_id,
            own_user_id,
        );

        debug!("Restoring room {}", room_lock.room_id);

        let matrix_members = room_lock
            .joined_members
            .values()
            .chain(room_lock.invited_members.values());

        for member in matrix_members {
            let display_name = room_lock
                .get_member(&member.user_id)
                .unwrap()
                .display_name
                .clone();

            trace!("Restoring member {}", member.user_id);
            let member = WeechatRoomMember::new(
                &member.user_id,
                member.disambiguated_name(),
                display_name,
            );

            room_buffer.members.add(member);
        }

        room_buffer.update_buffer_name();
        room_buffer.restore_messages();

        room_buffer
    }

    pub fn restore_messages(&self) {
        use AnyPossiblyRedactedSyncMessageEvent::*;

        let room = self.room();
        let buffer = self.weechat_buffer();

        if buffer.num_lines() == 0 {
            for event in room.messages.iter() {
                match event {
                    Regular(e) => self.handle_room_message(e),
                    Redacted(e) => self.handle_redacted_events(e),
                }
            }
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
        let buffer = self.buffer_handle.clone();
        self.inner.send_message(buffer, content).await
    }

    pub fn weechat_buffer(&self) -> Buffer {
        self.buffer_handle
            .upgrade()
            .expect(BUFFER_CLOSED_ERROR)
    }

    pub fn update_buffer_name(&self) {
        let name = self.members.calculate_buffer_name();
        self.weechat_buffer().set_name(&name)
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
        let typing_in_flight = self.inner.typing_in_flight.clone();
        let buffer = self.weechat_buffer();
        let input = buffer.input();

        if input.starts_with('/') && !input.starts_with("//") {
            // Don't send typing notices for commands.
            return;
        }

        let connection = self.inner.connection.clone();
        let room_id = self.inner.room_id.clone();
        let typing_notice_time = self.inner.typing_notice_time.clone();

        let send = async move |typing: bool| {
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

        let typing_time = self.inner.typing_notice_time.borrow_mut();

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


    pub fn handle_room_message(&self, event: &AnySyncMessageEvent) {
        // If the event has a transaction id it's an event that we sent out
        // ourselves, the content will be in the outgoing message queue and it
        // may have been printed out as a local echo.
        if let Some(id) = &event.unsigned().transaction_id {
            if let Ok(id) = Uuid::parse_str(id) {
                self.handle_outgoing_message(
                    self.buffer_handle.clone(),
                    id,
                    event.event_id(),
                );
                return;
            }
        }

        if let Some(rendered) = self.render_message_event(event) {
            self.print_rendered_event(rendered);
        }
    }

    fn print_rendered_event(&self, rendered: RenderedEvent) {
        let buffer = self.weechat_buffer();
        self.inner.print_rendered_event(&buffer, rendered);
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
            let rendered =
                e.render_with_prefix(&e.origin_server_ts, &sender, &redacter);

            self.print_rendered_event(rendered);
        }
    }

    pub fn handle_sync_room_event(&mut self, event: AnySyncRoomEvent) {
        match &event {
            AnySyncRoomEvent::Message(message) => {
                self.handle_room_message(message)
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

    pub fn handle_sync_state_event(&mut self, event: AnySyncStateEvent) {
        match &event {
            AnySyncStateEvent::RoomMember(e) => {
                self.members.handle_membership_event(e, true)
            }
            AnySyncStateEvent::RoomName(_) => self.update_buffer_name(),
            _ => (),
        }
    }
}
