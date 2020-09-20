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
            name::NameEventContent,
        },
        AnyMessageEventContent, AnyPossiblyRedactedSyncMessageEvent,
        AnySyncMessageEvent, AnySyncRoomEvent, AnySyncStateEvent,
        SyncStateEvent,
    },
    identifiers::{RoomId, UserId},
    PossiblyRedactedExt, Room,
};
use url::Url;

use async_trait::async_trait;
use tracing::{debug, error, trace};

use crate::{
    connection::{Connection, TYPING_NOTICE_TIMEOUT},
    render::{render_membership, render_message},
};
use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    convert::TryFrom,
    rc::Rc,
    sync::Mutex,
    time::Instant,
};
use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferHandle, BufferInputCallbackAsync,
        NickSettings,
    },
    Weechat,
};

#[derive(Clone)]
pub struct RoomBuffer {
    inner: MatrixRoom,
    buffer_handle: BufferHandle,
}

#[derive(Clone, Debug)]
pub struct WeechatRoomMember {
    pub user_id: Rc<UserId>,
    pub nick: Rc<RefCell<String>>,
    pub display_name: Rc<RefCell<Option<String>>>,
    pub prefix: Rc<RefCell<Option<String>>>,
    pub color: Rc<String>,
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

pub enum RoomError {
    NonExistentMember(UserId),
}

#[derive(Clone)]
struct MatrixRoom {
    homeserver: Rc<Url>,
    room_id: Rc<RoomId>,
    own_user_id: Rc<UserId>,

    connection: Rc<RefCell<Option<Connection>>>,

    typing_notice_time: Rc<RefCell<Option<Instant>>>,
    typing_in_flight: Rc<Mutex<()>>,

    room: Rc<RefCell<Room>>,
    members: Rc<RefCell<HashMap<UserId, WeechatRoomMember>>>,
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, buffer: BufferHandle, input: String) {
        // TODO parse the input here and produce a formatted body.
        let content = MessageEventContent::Text(TextMessageEventContent {
            body: input,
            formatted: None,
            relates_to: None,
        });
        let content = AnyMessageEventContent::RoomMessage(content);

        self.send_message(buffer, content).await;
    }
}

impl MatrixRoom {
    pub async fn send_message(
        &self,
        buffer: BufferHandle,
        content: AnyMessageEventContent,
    ) {
        if let Some(c) = &*self.connection.borrow() {
            // TODO check for errors and print them out.
            match c.send_message(&self.room_id, content, None).await {
                Ok(_r) => (),
                Err(_e) => (),
            }
        } else {
            let buffer = buffer
                .upgrade()
                .expect("Trying to send a message while the buffer is closed");

            buffer.print("Error not connected");
        }
    }
}

impl RoomBuffer {
    pub fn new(
        connection: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
        room_id: RoomId,
        own_user_id: &UserId,
    ) -> Self {
        let room = MatrixRoom {
            homeserver: Rc::new(homeserver.clone()),
            room_id: Rc::new(room_id.clone()),
            connection: connection.clone(),
            typing_notice_time: Rc::new(RefCell::new(None)),
            typing_in_flight: Rc::new(Mutex::new(())),
            room: Rc::new(RefCell::new(Room::new(&room_id, &own_user_id))),
            own_user_id: Rc::new(own_user_id.to_owned()),
            members: Rc::new(RefCell::new(HashMap::new())),
            local_echo_queue: Rc::new(RefCell::new(HashMap::new()))
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

        RoomBuffer {
            inner: room,
            buffer_handle,
        }
    }

    pub fn restore(
        room: Room,
        connection: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
    ) -> Self {
        let mut room_buffer = RoomBuffer::new(
            connection,
            homeserver,
            room.room_id.clone(),
            &room.own_user_id,
        );

        debug!("Restoring room {}", room.room_id);

        let matrix_members = room
            .joined_members
            .values()
            .chain(room.invited_members.values());

        let mut weechat_members = HashMap::new();

        for member in matrix_members {
            let display_name = room
                .get_member(&member.user_id)
                .unwrap()
                .display_name
                .clone();

            weechat_members.insert(
                member.user_id.clone(),
                WeechatRoomMember::new(
                    &member.user_id,
                    member.disambiguated_name(),
                    display_name,
                ),
            );
        }

        let buffer = room_buffer.weechat_buffer();

        for member in weechat_members.values() {
            trace!("Restoring member {}", member.user_id);
            let nick = member.nick.borrow();
            let settings = NickSettings::new(&nick);
            buffer
                .add_nick(settings)
                .expect("Can't add nick to nicklist");
        }

        room_buffer.inner.room = Rc::new(RefCell::new(room));
        room_buffer.inner.members.replace(weechat_members);
        room_buffer.update_buffer_name();
        room_buffer.restore_messages();

        room_buffer
    }

    pub fn restore_messages(&self) {
        let room = self.room();
        let buffer = self.weechat_buffer();

        if buffer.num_lines() == 0 {
            for message in room.messages.iter() {
                self.handle_room_message(&message)
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
    pub async fn send_message(&self, content: AnyMessageEventContent) {
        let buffer = self.buffer_handle.clone();
        self.inner.send_message(buffer, content).await
    }

    pub fn room_mut(&mut self) -> RefMut<'_, Room> {
        self.inner.room.borrow_mut()
    }

    pub fn room(&self) -> Ref<'_, Room> {
        self.inner.room.borrow()
    }

    /// Retrieve a reference to a Weechat room member by user ID.
    pub fn get_member(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        self.inner.members.borrow().get(user_id).cloned()
    }

    /// Retrieve a mutable reference to a Weechat room member by user ID.
    pub fn get_member_mut(
        &self,
        user_id: &UserId,
    ) -> Option<WeechatRoomMember> {
        self.inner.members.borrow().get(user_id).map(|m| m.clone())
    }

    /// Add a new Weechat room member.
    pub fn add_member(&mut self, member: WeechatRoomMember) {
        let buffer = self.weechat_buffer();
        let nick = member.nick.borrow();
        let nick_settings = NickSettings::new(&nick);

        // FIXME: If we expect this, it fails at least once. Why?
        let _ = buffer.add_nick(nick_settings);
        drop(nick);

        self.inner
            .members
            .borrow_mut()
            .insert((&*member.user_id).clone(), member);
    }

    /// Remove a Weechat room member by user ID.
    ///
    /// Returns either the removed Weechat room member, or an error if the member does not exist.
    pub fn remove_member(
        &mut self,
        user_id: &UserId,
    ) -> Result<WeechatRoomMember, RoomError> {
        let buffer = self.weechat_buffer();

        match self.get_member(user_id) {
            Some(member) => {
                buffer.remove_nick(&member.nick.borrow());
                Ok(self.inner.members.borrow_mut().remove(user_id).unwrap())
            }

            None => {
                error!(
                    "{}: Tried removing a non-existent Weechat room member: {}",
                    self.calculate_buffer_name(),
                    user_id
                );

                Err(RoomError::NonExistentMember(user_id.clone()))
            }
        }
    }

    /// Change nick of member.
    ///
    /// Returns either the old nick of the member, or an error if the member does not exist.
    pub fn rename_member(
        &mut self,
        user_id: &UserId,
        new_nick: String,
    ) -> Result<String, RoomError> {
        if self.get_member(user_id).is_some() {
            {
                let member = self.get_member(user_id).unwrap();
                trace!(
                    "Renaming member from {} to {}",
                    &member.nick.borrow(),
                    &new_nick
                );

                let buffer = self.weechat_buffer();

                buffer.remove_nick(&member.nick.borrow());
                let nick_settings = NickSettings::new(&new_nick);
                buffer
                    .add_nick(nick_settings)
                    .expect("Can't add nick to nicklist");
            }

            let old_nick = {
                let member = self.get_member_mut(user_id).unwrap();
                member.nick.replace(new_nick)
            };

            Ok(old_nick)
        } else {
            Err(RoomError::NonExistentMember(user_id.clone()))
        }
    }

    pub fn weechat_buffer(&self) -> Buffer {
        self.buffer_handle
            .upgrade()
            .expect("Buffer got closed but Room is still lingering around")
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
        self.weechat_buffer().set_name(&name)
    }

    /// Helper method to calculate the display name of a room member from their UserId.
    ///
    /// If no member with that ID is in the room, the string representation of the ID will be
    /// returned.
    fn calculate_user_name(&self, user_id: &UserId) -> String {
        self.room()
            .get_member(user_id)
            .unwrap_or_else(|| panic!("No such member {}", user_id))
            .disambiguated_name()
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

            // We're in the process of sending out a typing notice, so don't make
            // the same request twice.
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
            Weechat::spawn(send(false));
        } else if input.len() >= 4 {
            if let Some(typing_time) = &*typing_time {
                // If we have some valid input, check if the typing notice
                // expired and send one out if it indeed expired.
                if typing_time.elapsed() > TYPING_NOTICE_TIMEOUT {
                    Weechat::spawn(send(true));
                }
            } else {
                // If we have some valid input and no active typing notice, send
                // one out.
                Weechat::spawn(send(true));
            }
        }
    }

    /// Process disambiguations received from the SDK.
    ///
    /// Disambiguations are a hashmap of user ID -> bool indicating that this user is either newly
    /// ambiguous (true) or no longer ambiguous (false).
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
        &mut self,
        event: &SyncStateEvent<MemberEventContent>,
        state_event: bool,
    ) {
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

        // Handle the event in the SDK room model
        let (_, disambiguations) =
            self.room_mut().handle_membership(event, state_event);

        let new_nick = self.calculate_user_name(&target_id);

        if state_event {
            use MembershipState::*;

            // FIXME: Handle gaps (e.g. long disconnects) properly.
            //
            // For joins and invites, first we need to check whether a member with some MXID
            // exists. If he does, we have to update *that* member with the new state. Only if they
            // do not exist yet do we create a new one.
            //
            // For leaves and bans we just need to remove the member.
            match event.content.membership {
                Invite | Join => {
                    let display_name = self
                        .room()
                        .get_member(&target_id)
                        .unwrap()
                        .display_name
                        .clone();

                    self.add_member(WeechatRoomMember::new(
                        &target_id,
                        new_nick,
                        display_name,
                    ));
                }
                Leave | Ban => {
                    let _ = self.remove_member(&target_id);
                }
                _ => (),
            }

            self.process_disambiguations(&disambiguations);

            // Names of rooms without display names can get affected by the member list so we need to
            // update them.
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
                    self.add_member(member.clone());

                    sender = self.get_member(&sender_id).map(|s| s.clone());
                    target = Some(member);
                }

                Left | Banned | Kicked | KickedAndBanned
                | InvitationRejected | InvitationRevoked => {
                    sender = self.get_member(&sender_id).map(|s| s.clone());
                    target = self.get_member(&target_id).map(|s| s.clone());

                    match self.remove_member(&target_id) {
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
                    sender = self.get_member(&sender_id).map(|s| s.clone());
                    target = self.get_member(&target_id).map(|t| t.clone());

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

                        self.get_member_mut(&target_id)
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
                    sender = self.get_member(&sender_id).map(|m| m.clone());
                    target = self.get_member(&target_id).map(|m| m.clone());
                }
            };

            self.process_disambiguations(&disambiguations);

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
            self.weechat_buffer().print_date_tags(
                timestamp as i64,
                &[],
                &message,
            );
        }
    }

    pub fn handle_room_message(
        &self,
        event: &AnyPossiblyRedactedSyncMessageEvent,
    ) {
        let timestamp: u64 = event
            .origin_server_ts()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let message =
            render_message(event, self.calculate_user_name(event.sender()));
        let buffer = self.weechat_buffer();
        buffer.print_date_tags(timestamp as i64, &[], &message);
    }

    pub fn handle_room_name(
        &mut self,
        event: &SyncStateEvent<NameEventContent>,
    ) {
        self.room_mut().handle_room_name(event);
        self.update_buffer_name();
    }

    pub fn handle_room_event(&mut self, event: AnySyncRoomEvent) {
        match &event {
            AnySyncRoomEvent::Message(message) => match message {
                AnySyncMessageEvent::RoomMessage(_)
                | AnySyncMessageEvent::RoomEncrypted(_) => self
                    .handle_room_message(
                        &AnyPossiblyRedactedSyncMessageEvent::Regular(
                            message.to_owned(),
                        ),
                    ),
                _ => (),
            },

            AnySyncRoomEvent::State(event) => match &event {
                AnySyncStateEvent::RoomMember(e) => {
                    self.handle_membership_event(e, false)
                }
                AnySyncStateEvent::RoomName(n) => self.handle_room_name(n),
                _ => (),
            },

            event => {
                let mut room = self.room_mut();
                room.receive_timeline_event(event);
            }
        }
    }

    pub fn handle_state_event(&mut self, event: AnySyncStateEvent) {
        match &event {
            AnySyncStateEvent::RoomMember(e) => {
                self.handle_membership_event(e, true)
            }
            AnySyncStateEvent::RoomName(n) => self.handle_room_name(n),
            _ => {
                let mut room = self.room_mut();
                room.receive_state_event(&event);
            }
        }
    }
}
