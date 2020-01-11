use async_std::sync::channel as async_channel;
use async_std::sync::{Receiver, Sender};
use async_task::JoinHandle;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime;
use tokio::runtime::Runtime;
use url::Url;

use matrix_nio::api::r0::session::login::Response as LoginResponse;
use matrix_nio::api::r0::sync::sync_events::IncomingResponse as SyncResponse;

use matrix_nio::{
    self,
    events::{
        collections::all::{RoomEvent, StateEvent},
        room::message::{MessageEventContent, TextMessageEventContent},
        EventResult,
    },
    AsyncClient, AsyncClientConfig, SyncSettings,
};

use weechat::Weechat;

use crate::executor::spawn_weechat;
use crate::plugin;
use crate::room_buffer::RoomBuffer;
use crate::Config;

const DEFAULT_SYNC_TIMEOUT: i32 = 30000;

pub enum ThreadMessage {
    LoginMessage(LoginResponse),
    SyncState(String, StateEvent),
    SyncEvent(String, RoomEvent),
}

#[derive(Debug)]
pub enum ServerError {
    StartError(String),
}

pub enum ServerMessage {
    ShutDown,
    RoomSend(String, String),
}

#[derive(Default)]
pub struct ServerConfig {
    homeserver: Option<Url>,
}

pub struct LoginState {
    user_id: String,
    device_id: String,
}

struct ConnectedState {
    client_channel: Sender<ServerMessage>,
    sync_receiver: JoinHandle<(), ()>,
    runtime: Runtime,
}

pub(crate) struct MatrixServer {
    server_name: String,
    connected: bool,
    room_buffers: HashMap<String, RoomBuffer>,
    config: ServerConfig,
    client: AsyncClient,
    homeserver: Url,
    login_state: Option<LoginState>,
    connected_state: Option<ConnectedState>,
}

impl MatrixServer {
    pub fn new(name: &str, config: &Config) -> Self {
        let homeserver = Url::parse("http://localhost:8008").unwrap();

        let config = AsyncClientConfig::new()
            .proxy("http://localhost:8080")
            .unwrap()
            .disable_ssl_verification();

        let client =
            AsyncClient::new_with_config(homeserver.clone(), None, config)
                .unwrap();

        MatrixServer {
            server_name: name.to_owned(),
            connected: false,
            room_buffers: HashMap::new(),
            config: ServerConfig { homeserver: None },
            client,
            homeserver,
            login_state: None,
            connected_state: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.server_name
    }

    pub fn connect(&mut self) {
        let runtime = Runtime::new().unwrap();

        let send_client = self.client.clone();
        let (tx, rx) = async_channel(1000);
        runtime.spawn(MatrixServer::sync_loop(self.client.clone(), tx));
        let sync_receiver_handle =
            spawn_weechat(MatrixServer::sync_receiver(rx));

        let (client_sender, client_receiver) = async_channel(10);
        runtime.spawn(MatrixServer::send_loop(send_client, client_receiver));

        self.connected_state = Some(ConnectedState {
            sync_receiver: sync_receiver_handle,
            client_channel: client_sender,
            runtime,
        });
    }

    pub fn disconnect(&mut self) {
        let connected_state = self.connected_state.take();
        if let Some(s) = connected_state {
            s.sync_receiver.cancel();
        }
    }

    pub async fn sync_loop(
        mut client: AsyncClient,
        channel: Sender<Result<ThreadMessage, String>>,
    ) {
        if !client.logged_in() {
            let ret = client.login("example", "wordpass", None).await;

            match ret {
                Ok(response) => {
                    channel
                        .send(Ok(ThreadMessage::LoginMessage(response)))
                        .await
                }
                Err(_e) => {
                    channel.send(Err("No logging in".to_string())).await;
                    return;
                }
            }
        }

        let sync_token = client.sync_token();
        let sync_settings = SyncSettings::new()
            .timeout(DEFAULT_SYNC_TIMEOUT)
            .expect("Invalid sync timeout");

        let sync_settings = if let Some(t) = sync_token {
            sync_settings.token(t)
        } else {
            sync_settings
        };

        let sync_channel = &channel;

        client
            .sync_forever(sync_settings, async move |response| {
                let channel = sync_channel;
                for (room_id, room) in response.rooms.join {
                    for event in room.state.events {
                        if let EventResult::Ok(e) = event {
                            channel
                                .send(Ok(ThreadMessage::SyncState(
                                    room_id.to_string(),
                                    e,
                                )))
                                .await;
                        }
                    }
                    for event in room.timeline.events {
                        if let EventResult::Ok(e) = event {
                            channel
                                .send(Ok(ThreadMessage::SyncEvent(
                                    room_id.to_string(),
                                    e,
                                )))
                                .await;
                        }
                    }
                }
            })
            .await;
    }

    pub async fn sync_receiver(
        receiver: Receiver<Result<ThreadMessage, String>>,
    ) {
        let weechat = unsafe { Weechat::weechat() };
        let plugin = plugin();

        let server = match plugin.servers.get_mut("localhost") {
            Some(s) => s,
            None => return,
        };

        loop {
            let ret = match receiver.recv().await {
                Some(m) => m,
                None => {
                    weechat.print("Error receiving message");
                    return;
                }
            };

            match ret {
                Ok(message) => match message {
                    ThreadMessage::LoginMessage(r) => server.receive_login(r),
                    ThreadMessage::SyncEvent(r, e) => {
                        server.receive_joined_timeline_event(&r, e)
                    }
                    ThreadMessage::SyncState(r, e) => {
                        server.receive_joined_state_event(&r, e)
                    }
                },
                Err(e) => weechat.print(&format!("Ruma error {}", e)),
            };
        }
    }

    pub async fn send_loop(
        mut client: AsyncClient,
        channel: Receiver<ServerMessage>,
    ) {
        while let Some(message) = channel.recv().await {
            match message {
                ServerMessage::ShutDown => return,
                ServerMessage::RoomSend(room_id, message) => {
                    let content =
                        MessageEventContent::Text(TextMessageEventContent {
                            body: message.to_owned(),
                            format: None,
                            formatted_body: None,
                            relates_to: None,
                        });

                    let ret = client.room_send(&room_id, content).await;

                    match ret {
                        Ok(_r) => (),
                        Err(_e) => (),
                    }
                }
            }
        }
    }

    pub fn receive_login(&mut self, response: LoginResponse) {
        let login_state = LoginState {
            user_id: response.user_id.to_string(),
            device_id: response.device_id,
        };
        self.login_state = Some(login_state);
    }

    pub async fn send_message(&self, room_id: &str, message: &str) {
        self.connected_state
            .as_ref()
            .expect("Sending a message while not connected")
            .client_channel
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
            let login_state = self
                .login_state
                .as_ref()
                .expect("Receiving events while not being logged in");
            let buffer = RoomBuffer::new(
                &self.server_name,
                &self.homeserver,
                room_id,
                &login_state.user_id,
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
        let room = self.get_or_create_room(room_id);
        room.handle_state_event(event)
    }

    pub(crate) fn receive_joined_timeline_event(
        &mut self,
        room_id: &str,
        event: RoomEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_room_event(event)
    }
}
