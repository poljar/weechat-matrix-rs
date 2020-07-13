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

use matrix_sdk::events::collections::all::{RoomEvent, StateEvent};
use matrix_sdk::events::room::encrypted::EncryptedEvent;
use matrix_sdk::events::room::member::{MemberEvent, MembershipState};
use matrix_sdk::events::room::message::{
    MessageEvent, MessageEventContent, TextMessageEventContent,
};
use matrix_sdk::events::room::name::NameEvent;
use matrix_sdk::identifiers::{RoomId, UserId};
use matrix_sdk::Room;
use url::Url;

use async_trait::async_trait;

use crate::server::{Connection, TYPING_NOTICE_TIMEOUT};
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Instant;
use weechat::buffer::{
    Buffer, BufferHandle, BufferInputCallbackAsync, BufferSettingsAsync,
    NickSettings,
};
use weechat::Weechat;

pub struct RoomBuffer {
    own_user_id: Rc<UserId>,
    room_id: Rc<RoomId>,
    typing_in_flight: Mutex<()>,
    inner: MatrixRoom,
    buffer_handle: BufferHandle,
}

#[derive(Clone)]
pub struct MatrixRoom {
    server_name: Rc<String>,
    homeserver: Rc<Url>,
    room_id: Rc<RoomId>,
    connection: Rc<RefCell<Option<Connection>>>,
    prev_batch: Rc<RefCell<Option<String>>>,
    typing_notice_time: Rc<RefCell<Option<Instant>>>,
    room: Rc<RefCell<Room>>,
    printed_before_ack_queue: Rc<RefCell<Vec<String>>>,
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, buffer: BufferHandle, input: String) {
        let connection = &self.connection;

        let buffer = buffer
            .upgrade()
            .expect("Running input cb but buffer is closed");

        if let Some(c) = &*connection.borrow() {
            // TODO check for errors and print them out.
            match c.send_message(&self.room_id, input).await {
                Ok(_r) => (),
                Err(_e) => (),
            }
        } else {
            buffer.print("Error not connected");
            return;
        }
    }
}

impl RoomBuffer {
    pub fn new(
        server_name: &str,
        connection: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
        room_id: RoomId,
        own_user_id: &UserId,
    ) -> Self {
        let room = MatrixRoom {
            server_name: Rc::new(server_name.to_owned()),
            homeserver: Rc::new(homeserver.clone()),
            room_id: Rc::new(room_id.clone()),
            connection: connection.clone(),
            prev_batch: Rc::new(RefCell::new(None)),
            typing_notice_time: Rc::new(RefCell::new(None)),
            room: Rc::new(RefCell::new(Room::new(&room_id, &own_user_id))),
            printed_before_ack_queue: Rc::new(RefCell::new(Vec::new())),
        };

        let buffer_settings = BufferSettingsAsync::new(&room_id.to_string())
            .input_callback(room.clone())
            .close_callback(|_weechat: &Weechat, _buffer: &Buffer| {
                // TODO remove the roombuffer from the server here.
                // TODO leave the room if the plugin isn't unloading.
                Ok(())
            });

        let buffer_handle = Weechat::buffer_new_with_async(buffer_settings)
            .expect("Can't create new room buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created buffer");

        buffer.enable_nicklist();

        RoomBuffer {
            room_id: Rc::new(room_id),
            own_user_id: Rc::new(own_user_id.to_owned()),
            typing_in_flight: Mutex::new(()),
            inner: room,
            buffer_handle,
        }
    }

    pub fn restore(
        room: Room,
        server_name: &str,
        connection: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
    ) -> Self {
        let mut room_buffer = RoomBuffer::new(
            server_name,
            connection,
            homeserver,
            room.room_id.clone(),
            &room.own_user_id,
        );

        let buffer = room_buffer.weechat_buffer();

        for member in room.joined_members.values() {
            let user_id = member.user_id.to_string();
            // TODO use display names here
            let settings = NickSettings::new(&user_id);
            buffer
                .add_nick(settings)
                .expect("Can't add nick to nicklist");
        }

        room_buffer.inner.room = Rc::new(RefCell::new(room));
        room_buffer.update_buffer_name();
        room_buffer.restore_messages();

        room_buffer
    }

    pub fn restore_messages(&self) {
        let room = self.room();
        let buffer = self.weechat_buffer();

        if buffer.num_lines() == 0 {
            for message in room.messages.iter() {
                self.handle_room_message(message)
            }
        }
    }

    pub fn room_mut(&mut self) -> RefMut<'_, Room> {
        self.inner.room.borrow_mut()
    }

    pub fn room(&self) -> Ref<'_, Room> {
        self.inner.room.borrow()
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
        } else if room_name.starts_with("#") {
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
        // We're in the process of sending out a typing notice, so don't make
        // the same request twice.
        let guard = match self.typing_in_flight.try_lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        let buffer = self.weechat_buffer();
        let input = buffer.input();

        if input.starts_with('/') && !input.starts_with("//") {
            // Don't send typing notices for commands.
            return;
        }

        let connection = self.inner.connection.clone();
        let room_id = self.room_id.clone();
        let user_id = self.own_user_id.clone();
        let typing_notice_time = self.inner.typing_notice_time.clone();

        let send = async move |typing: bool, _guard| {
            let typing_time = typing_notice_time;

            if let Some(connection) = &*connection.borrow() {
                let response = connection
                    .send_typing_notice(&*room_id, &*user_id, typing)
                    .await;

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

            // The `guard` expires here, releasing the mutex taken at the
            // beginning of `update_typing_notice`.
        };

        let typing_time = self.inner.typing_notice_time.borrow_mut();

        if input.len() < 4 && typing_time.is_some() {
            // If we have an active typing notice and our input is short, e.g.
            // we removed the input set the typing notice to false.
            Weechat::spawn(send(false, guard));
        } else if input.len() >= 4 {
            if let Some(typing_time) = &*typing_time {
                // If we have some valid input, check if the typing notice
                // expired and send one out if it indeed expired.
                if typing_time.elapsed() > TYPING_NOTICE_TIMEOUT {
                    Weechat::spawn(send(true, guard));
                }
            } else {
                // If we have some valid input and no active typing notice, send
                // one out.
                Weechat::spawn(send(true, guard));
            }
        }
    }

    pub fn handle_membership_event(
        &mut self,
        event: &MemberEvent,
        print_message: bool,
    ) {
        let buffer = self.weechat_buffer();
        let content = &event.content;

        match content.membership {
            MembershipState::Join => {
                let settings = NickSettings::new(&event.state_key);
                let _ = buffer.add_nick(settings);
            }
            MembershipState::Leave => {
                buffer.remove_nick(&event.state_key);
            }
            MembershipState::Ban => {
                buffer.remove_nick(&event.state_key);
            }
            _ => (),
        }

        // The state event should not be printed out, return early here.
        if !print_message {
            return;
        }

        let message = match content.membership {
            MembershipState::Join => "joined",
            MembershipState::Leave => "left",
            _ => return,
        };

        let message = format!(
            "{} ({}) has {} the room",
            content.displayname.as_ref().unwrap_or(&"".to_string()),
            event.state_key,
            message
        );
        let timestamp: u64 = event
            .origin_server_ts
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        buffer.print_date_tags(timestamp as i64, &[], &message);

        {
            let mut room = self.room_mut();
            room.handle_membership(&event);
        }

        // Names of rooms without display names can get affected by the member list so we need to
        // update them.
        self.update_buffer_name();
    }

    pub fn handle_text_message(
        &self,
        sender: &str,
        timestamp: u64,
        content: &TextMessageEventContent,
    ) {
        let buffer = self.weechat_buffer();
        let message = format!("{}\t{}", sender, content.body);
        buffer.print_date_tags(timestamp as i64, &[], &message);
    }

    pub fn handle_room_message(&self, event: &MessageEvent) {
        let sender = &event.sender;
        let timestamp: u64 = event
            .origin_server_ts
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match &event.content {
            MessageEventContent::Text(t) => {
                self.handle_text_message(&sender.to_string(), timestamp, t)
            }
            _ => (),
        }
    }

    pub fn handle_encrypted_message(&mut self, event: &EncryptedEvent) {
        let buffer = self.weechat_buffer();

        let sender = &event.sender;
        let timestamp: u64 = event
            .origin_server_ts
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let message = format!("{}\t{}", sender, "Unable to decrypt message");
        buffer.print_date_tags(timestamp as i64, &[], &message);
    }

    pub fn handle_room_name(&mut self, event: &NameEvent) {
        {
            let mut room = self.room_mut();
            room.handle_room_name(event);
        }
        self.update_buffer_name();
    }

    pub fn handle_room_event(&mut self, event: RoomEvent) {
        match &event {
            RoomEvent::RoomMember(e) => self.handle_membership_event(e, true),
            RoomEvent::RoomMessage(m) => self.handle_room_message(m),
            RoomEvent::RoomEncrypted(m) => self.handle_encrypted_message(m),
            RoomEvent::RoomName(n) => self.handle_room_name(n),
            event => {
                let mut room = self.room_mut();
                room.receive_timeline_event(event);
            }
        }
    }

    pub fn handle_state_event(&mut self, event: StateEvent) {
        match &event {
            StateEvent::RoomMember(e) => self.handle_membership_event(e, false),
            StateEvent::RoomName(n) => self.handle_room_name(n),
            _ => (),
        }
        {
            let mut room = self.room_mut();
            room.receive_state_event(&event);
        }
    }
}
