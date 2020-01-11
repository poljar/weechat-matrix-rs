use async_std::sync::channel as async_channel;
use async_std::sync::{Receiver, Sender};
use async_task::JoinHandle;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use tokio::runtime::Runtime;
use url::Url;

use matrix_nio::api::r0::session::login::Response as LoginResponse;

use matrix_nio::{
    self,
    events::{
        collections::all::{RoomEvent, StateEvent},
        room::message::{MessageEventContent, TextMessageEventContent},
        EventResult,
    },
    AsyncClient, AsyncClientConfig, SyncSettings,
};

use weechat::config::{
    BooleanOptionSettings, ConfigSection, IntegerOptionSettings,
};
use weechat::Weechat;

use crate::executor::spawn_weechat;
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
pub struct ServerSettings {
    homeserver: Option<Url>,
}

pub struct LoginState {
    user_id: String,
    device_id: String,
}

pub struct Connection {
    client_channel: Sender<ServerMessage>,
    response_receiver: JoinHandle<(), ()>,
    runtime: Runtime,
}

impl Connection {
    pub async fn send_message(&self, room_id: &str, message: &str) {
        self.client_channel
            .send(ServerMessage::RoomSend(
                room_id.to_owned(),
                message.to_owned(),
            ))
            .await;
    }
}

#[derive(Clone)]
pub(crate) struct MatrixServer {
    server_name: Rc<String>,
    inner: Rc<RefCell<InnerServer>>,
    client: AsyncClient,
}

pub(crate) struct InnerServer {
    server_name: Rc<String>,
    connected: bool,
    room_buffers: HashMap<String, RoomBuffer>,
    settings: ServerSettings,
    config: Config,
    homeserver: Url,
    login_state: Option<LoginState>,
    connected_state: Rc<RefCell<Option<Connection>>>,
}

impl MatrixServer {
    pub fn new(
        name: &str,
        config: &Config,
        server_section: &mut ConfigSection,
    ) -> Self {
        let homeserver = Url::parse("http://localhost:8008").unwrap();

        let client_config = AsyncClientConfig::new()
            .proxy("http://localhost:8080")
            .unwrap()
            .disable_ssl_verification();

        let client = AsyncClient::new_with_config(
            homeserver.clone(),
            None,
            client_config,
        )
        .unwrap();

        let server_name = Rc::new(name.to_owned());

        let mut server = InnerServer {
            server_name: server_name.clone(),
            connected: false,
            room_buffers: HashMap::new(),
            settings: ServerSettings { homeserver: None },
            config: config.clone(),
            homeserver,
            login_state: None,
            connected_state: Rc::new(RefCell::new(None)),
        };
        server.create_server_conf(server_section);

        MatrixServer {
            server_name,
            client,
            inner: Rc::new(RefCell::new(server)),
        }
    }

    pub fn name(&self) -> &str {
        &self.server_name
    }
}

impl InnerServer {
    fn create_server_conf(&mut self, server_section: &mut ConfigSection) {
        let autoconnect = BooleanOptionSettings::new(format!(
            "{}.autoconnect",
            self.server_name
        ))
        .set_change_callback(|weechat, option| {
            weechat.print("Hello");
        });

        let server_buffer = IntegerOptionSettings::new(format!(
            "{}.server_buffer",
            self.server_name
        ));

        let autoconnect = server_section
            .new_boolean_option(autoconnect)
            .expect("Can't create autoconnect option");

        let server_buffer = server_section
            .new_integer_option(server_buffer)
            .expect("Can't create autoconnect option");
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
                &self.connected_state,
                &self.homeserver,
                &self.config,
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

    pub fn receive_login(&mut self, response: LoginResponse) {
        let login_state = LoginState {
            user_id: response.user_id.to_string(),
            device_id: response.device_id,
        };
        self.login_state = Some(login_state);
    }
}

impl MatrixServer {
    pub fn connect(&self) {
        let runtime = Runtime::new().unwrap();
        let server = self.inner.borrow_mut();

        let send_client = self.client.clone();
        let (tx, rx) = async_channel(1000);
        runtime.spawn(MatrixServer::sync_loop(self.client.clone(), tx.clone()));
        let response_receiver = spawn_weechat(MatrixServer::response_receiver(
            rx,
            Rc::downgrade(&self.inner),
        ));

        let (client_sender, client_receiver) = async_channel(10);
        runtime.spawn(MatrixServer::send_loop(
            send_client,
            client_receiver,
            tx,
        ));

        let mut connected_state = server.connected_state.borrow_mut();

        *connected_state = Some(Connection {
            response_receiver,
            client_channel: client_sender,
            runtime,
        });
    }

    pub fn disconnect(&self) {
        let server = self.inner.borrow_mut();
        let mut connected_state = server.connected_state.borrow_mut();
        let state = connected_state.take();

        if let Some(s) = state {
            s.response_receiver.cancel();
        }
    }

    /// Main client sync loop.
    /// This runs on the per server tokio executor.
    /// It communicates with the main Weechat thread using a async channel.
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

    /// Response receiver loop.
    /// This runs on the main Weechat thread and listens for responses coming
    /// from the client running in the tokio executor.
    pub async fn response_receiver(
        receiver: Receiver<Result<ThreadMessage, String>>,
        server: Weak<RefCell<InnerServer>>,
    ) {
        let weechat = unsafe { Weechat::weechat() };

        loop {
            let ret = match receiver.recv().await {
                Some(m) => m,
                None => {
                    weechat.print("Error receiving message");
                    return;
                }
            };

            let server_cell = server
                .upgrade()
                .expect("Can't upgrade server in sync receiver");
            let mut server = server_cell.borrow_mut();

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

    /// Send loop that waits for requests that need to be sent out using our
    /// Matrix client.
    pub async fn send_loop(
        mut client: AsyncClient,
        channel: Receiver<ServerMessage>,
        sender: Sender<Result<ThreadMessage, String>>,
    ) {
        while let Some(message) = channel.recv().await {
            match message {
                ServerMessage::ShutDown => return,
                ServerMessage::RoomSend(room_id, message) => {
                    let content =
                        MessageEventContent::Text(TextMessageEventContent {
                            body: message,
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
}
