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
use matrix_sdk::events::room::message::MessageEvent;
use matrix_sdk::events::room::name::NameEvent;
use matrix_sdk::identifiers::{RoomId, UserId};
use matrix_sdk::Room;
use url::Url;

use async_trait::async_trait;

use crate::render::RenderableEvent;
use crate::server::Connection;
use crate::Config;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;
use weechat::buffer::{
    Buffer, BufferHandle, BufferInputCallbackAsync, BufferSettingsAsync,
    NickSettings,
};
use weechat::Weechat;

pub(crate) struct RoomMember {
    nick: String,
    user_id: String,
    prefix: String,
    color: String,
}

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
        }
    }
}

impl RoomBuffer {
    pub fn new(
        server_name: &str,
        connected_state: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
        config: &Config,
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
            .close_callback(|weechat: &Weechat, buffer: &Buffer| {
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

    pub fn room_mut(&mut self) -> RefMut<'_, Room> {
        self.inner.room.borrow_mut()
    }

    pub fn room(&self) -> Ref<'_, Room> {
        self.inner.room.borrow()
    }

    pub fn weechat_buffer(&mut self) -> Buffer {
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

    pub fn update_buffer_name(&mut self) {
        let name = self.calculate_buffer_name();
        self.weechat_buffer().set_name(&name)
    }

    /// Calculate the display name for a room member from its UserId. If no member with that ID is in
    /// the room, the string representation of the ID will be returned.
    fn calculate_user_name(&self, user_id: &UserId) -> String {
        let name = self
            .inner
            .room
            .borrow()
            .members
            .get(user_id)
            // TODO: get rid of clone?
            .and_then(|member| member.display_name.clone())
            .unwrap_or_else(|| format!("{}", user_id));

        // count members with the same display name
        let count = self
            .inner
            .room
            .borrow()
            .members
            .values()
            .filter(|member| {
                member
                    .display_name
                    .as_ref()
                    .map(|n| n == &name)
                    .unwrap_or(false)
            })
            .count();

        if count > 1 {
            // more than one member with the same display name -> append the ID
            format!("{} ({})", name, user_id)
        } else {
            name
        }
    }

    fn render_event(&self, event: &impl RenderableEvent) -> String {
        event.render(&self.calculate_user_name(event.sender()))
    }

    pub fn handle_membership_event(
        &mut self,
        event: &MemberEvent,
        print_message: bool,
    ) {
        let mut buffer = self.weechat_buffer();
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

        let message = self.render_event(event);

        let timestamp: u64 = event
            .origin_server_ts
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // this is needed so we can borrow `self` for `calculate_user_name`
        let buffer = self.weechat_buffer();
        buffer.print_date_tags(timestamp as i64, &[], &message);

        {
            let mut room = self.room_mut();
            room.handle_membership(&event);
        }

        // Names of rooms without display names can get affected by the member list so we need to
        // update them.
        self.update_buffer_name();
    }

    pub fn handle_room_message(&mut self, event: &MessageEvent) {
        let timestamp: u64 = event
            .origin_server_ts
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let message = self.render_event(event);

        let buffer = self.weechat_buffer();
        buffer.print_date_tags(timestamp as i64, &[], &message)
    }

    pub fn handle_encrypted_message(&mut self, event: &EncryptedEvent) {
        let timestamp: u64 = event
            .origin_server_ts
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let message = self.render_event(event);
        let buffer = self.weechat_buffer();
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
