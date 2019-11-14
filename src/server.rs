use async_std::sync::Sender as AsyncSender;
use std::collections::HashMap;
use url::Url;

use matrix_nio::api::r0::session::login::Response as LoginResponse;
use matrix_nio::events::collections::all::{RoomEvent, StateEvent};

use crate::room_buffer::RoomBuffer;

pub enum ServerMessage {
    ShutDown,
    RoomSend(String, String),
}

pub(crate) struct ServerConfig {}

#[derive(Default)]
pub(crate) struct ServerUser {
    user_id: String,
    device_id: String,
}

pub(crate) struct MatrixServer {
    server_name: String,
    connected: bool,
    homeserver: Url,
    room_buffers: HashMap<String, RoomBuffer>,
    config: ServerConfig,
    server_user: Option<ServerUser>,
    client_channel: AsyncSender<ServerMessage>,
}

impl MatrixServer {
    pub fn new(
        name: &str,
        homeserver: &Url,
        channel: AsyncSender<ServerMessage>,
    ) -> Self {
        MatrixServer {
            server_name: name.to_owned(),
            connected: false,
            homeserver: homeserver.clone(),
            room_buffers: HashMap::new(),
            config: ServerConfig {},
            server_user: None,
            client_channel: channel,
        }
    }

    pub fn receive_login(&mut self, response: LoginResponse) {
        let server_user = ServerUser {
            user_id: response.user_id.to_string(),
            device_id: response.device_id.clone(),
        };
        self.server_user = Some(server_user);
    }

    pub async fn send_message(&self, room_id: &str, message: &str) {
        self.client_channel
            .send(ServerMessage::RoomSend(
                room_id.to_owned(),
                message.to_owned(),
            ))
            .await;
    }

    pub(crate) fn get_or_create_room(
        &mut self,
        room_id: &str,
    ) -> &mut RoomBuffer {
        if !self.room_buffers.contains_key(room_id) {
            let buffer = RoomBuffer::new(
                &self.server_name,
                &self.homeserver,
                room_id,
                &self
                    .server_user
                    .as_ref()
                    .expect("Receiving events while not being logged in")
                    .user_id
                    .to_string(),
            );
            self.room_buffers.insert(room_id.to_string(), buffer);
        }

        self.room_buffers.get_mut(room_id).unwrap()
    }

    pub(crate) fn receive_joined_state_event(
        &mut self,
        room_id: &str,
        event: StateEvent,
    ) {
        let mut room = self.get_or_create_room(room_id);
        room.handle_state_event(event)
    }

    pub(crate) fn receive_joined_timeline_event(
        &mut self,
        room_id: &str,
        event: RoomEvent,
    ) {
        let mut room = self.get_or_create_room(room_id);
        room.handle_room_event(event)
    }
}
