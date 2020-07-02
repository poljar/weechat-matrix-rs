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

use crate::server::Connection;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;
use weechat::buffer::{
    Buffer, BufferHandle, BufferInputCallbackAsync, BufferSettingsAsync,
    NickSettings,
};
use weechat::Weechat;

pub struct RoomBuffer {
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
    typing_notice_time: Rc<RefCell<Option<u64>>>,
    room: Rc<RefCell<Room>>,
    printed_before_ack_queue: Rc<RefCell<Vec<String>>>,
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for MatrixRoom {
    async fn callback(&mut self, buffer: BufferHandle, input: String) {
        let room_id = &self.room_id;
        let connection = &self.connection;

        let buffer = buffer
            .upgrade()
            .expect("Running input cb but buffer is closed");

        if let Some(c) = &*connection.borrow() {
            // TODO check for errors and print them out.
            c.send_message(&room_id, &input).await;
        } else {
            buffer.print("Error not connected");
            return;
        }
    }
}

impl RoomBuffer {
    pub fn new(
        server_name: &str,
        connected_state: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
        room_id: RoomId,
        own_user_id: &UserId,
    ) -> Self {
        let room = MatrixRoom {
            server_name: Rc::new(server_name.to_owned()),
            homeserver: Rc::new(homeserver.clone()),
            room_id: Rc::new(room_id.clone()),
            connection: connected_state.clone(),
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
            inner: room,
            buffer_handle,
        }
    }

    pub fn restore(
        room: Room,
        server_name: &str,
        connected_state: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
    ) -> Self {
        let mut room_buffer = RoomBuffer::new(
            server_name,
            connected_state,
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

        room_buffer
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

    pub fn update_buffer_name(&mut self) {
        let name = self.calculate_buffer_name();
        self.weechat_buffer().set_name(&name)
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
        &mut self,
        sender: &str,
        timestamp: u64,
        content: &TextMessageEventContent,
    ) {
        let buffer = self.weechat_buffer();
        let message = format!("{}\t{}", sender, content.body);
        buffer.print_date_tags(timestamp as i64, &[], &message);
    }

    pub fn handle_room_message(&mut self, event: &MessageEvent) {
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
