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
use matrix_sdk::Room;
use url::Url;

use crate::server::Connection;
use crate::Config;
use std::cell::RefCell;
use std::rc::Rc;
use weechat::buffer::{Buffer, BufferHandle, BufferSettings, NickSettings};
use weechat::Weechat;

pub(crate) struct RoomMember {
    nick: String,
    user_id: String,
    prefix: String,
    color: String,
}

pub(crate) struct RoomBuffer {
    server_name: String,
    homeserver: Url,
    buffer_handle: BufferHandle,
    room_id: String,
    prev_batch: Option<String>,
    typing_notice_time: Option<u64>,
    room: Room,
    printed_before_ack_queue: Vec<String>,
}

impl RoomBuffer {
    pub fn new(
        server_name: &str,
        connected_state: &Rc<RefCell<Option<Connection>>>,
        homeserver: &Url,
        config: &Config,
        room_id: String,
        own_user_id: &str,
    ) -> Self {
        let state = Rc::downgrade(connected_state);

        let buffer_settings = BufferSettings::new(&room_id.to_string())
            .input_data((state, room_id.to_owned()))
            .input_callback(async move |data, buffer, input| {
                {
                    let (client_rc, room_id) = data.unwrap();
                    let client_rc = client_rc.upgrade().expect(
                        "Can't upgrade server, server has been deleted?",
                    );
                    let client = client_rc.borrow();
                    let buffer = buffer
                        .upgrade()
                        .expect("Running input cb but buffer is closed");

                    if client.is_none() {
                        buffer.print("Error not connected");
                        return;
                    }
                    if let Some(s) = client.as_ref() {
                        // TODO check for errors and print them out.
                        s.send_message(&room_id, &input).await;
                    }
                }
            })
            .close_callback(|weechat, buffer| {
                // TODO remove the roombuffer from the server here.
                // TODO leave the room if the plugin isn't unloading.
                Ok(())
            });

        let buffer_handle = Weechat::buffer_new(buffer_settings)
            .expect("Can't create new room buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created buffer");

        buffer.enable_nicklist();

        RoomBuffer {
            server_name: server_name.to_owned(),
            homeserver: homeserver.clone(),
            room_id: room_id.clone(),
            buffer_handle,
            prev_batch: None,
            typing_notice_time: None,
            room: Room::new(&room_id, &own_user_id.to_string()),
            printed_before_ack_queue: Vec::new(),
        }
    }

    pub fn weechat_buffer(&mut self) -> Buffer {
        self.buffer_handle
            .upgrade()
            .expect("Buffer got closed but Room is still lingering around")
    }

    pub fn calculate_buffer_name(&self) -> String {
        let room_name = self.room.calculate_name();

        if room_name.starts_with("#") {
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
        let timestamp: u64 = event.origin_server_ts.into();
        let timestamp = timestamp / 1000;

        buffer.print_date_tags(timestamp as i64, &[], &message);
        self.room.handle_membership(&event);

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
        let timestamp = timestamp / 1000;
        let message = format!("{}\t{}", sender, content.body);
        buffer.print_date_tags(timestamp as i64, &[], &message);
    }

    pub fn handle_room_message(&mut self, event: &MessageEvent) {
        let sender = &event.sender;
        let timestamp: u64 = event.origin_server_ts.into();

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
        let timestamp: u64 = event.origin_server_ts.into();
        let timestamp = timestamp / 1000;
        let message = format!("{}\t{}", sender, "Unable to decrypt message");
        buffer.print_date_tags(timestamp as i64, &[], &message);
    }

    pub fn handle_room_name(&mut self, event: &NameEvent) {
        self.room.handle_room_name(event);
        self.update_buffer_name();
    }

    pub fn handle_room_event(&mut self, event: RoomEvent) {
        match &event {
            RoomEvent::RoomMember(e) => self.handle_membership_event(e, true),
            RoomEvent::RoomMessage(m) => self.handle_room_message(m),
            RoomEvent::RoomEncrypted(m) => self.handle_encrypted_message(m),
            RoomEvent::RoomName(n) => self.handle_room_name(n),
            event => {
                self.room.receive_timeline_event(event);
            }
        }
    }

    pub fn handle_state_event(&mut self, event: StateEvent) {
        match &event {
            StateEvent::RoomMember(e) => self.handle_membership_event(e, false),
            StateEvent::RoomName(n) => self.handle_room_name(n),
            _ => (),
        }
        self.room.receive_state_event(&event);
    }
}
