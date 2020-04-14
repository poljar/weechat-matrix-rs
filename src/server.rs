//! Matrix server abstraction.
//!
//! A MatrixServer is created for every server the user configures.
//!
//! It will create a per server config subsection. If options are added to the
//! server they need to be removed from the server section when the server is
//! dropped.
//!
//! The server will create a tokio runtime which will spawn two tasks, one for
//! the sync loop and one to send out room messages.
//!
//! If will also spawn a task on the Weechat mainloop, this one waits for
//! responses from the sync loop.
//!
//!
//! Schematically this looks like the following diagram.
//!
//!                                 MatrixServer
//!   +--------------------------------------------------------------------+
//!   |                                                                    |
//!   |         Weechat mainloop                     Tokio runtime         |
//!   |   +---------------------------+        +------------------------+  |
//!   |   |                           |        |                        |  |
//!   |   |  +--------------------+   |        |   +----------------+   |  |
//!   |   |  |                    |   |        |   |                |   |  |
//!   |   |  |  Response receiver +<---------------+   Sync loop    |   |  |
//!   |   |  |                    |   |        |   |                |   |  |
//!   |   |  |                    |   |        |   |                |   |  |
//!   |   |  +--------------------+   |        |   +----------------+   |  |
//!   |   |                           |        |                        |  |
//!   |   |  +--------------------+   |        |   +----------------+   |  |
//!   |   |  |                    |   |        |   |                |   |  |
//!   |   |  |  Roombuffer input  +--------------->+   Send loop    |   |  |
//!   |   |  |      callback      +<---------------+                |   |  |
//!   |   |  |                    |   |        |   |                |   |  |
//!   |   |  +--------------------+   |        |   +----------------+   |  |
//!   |   |                           |        |                        |  |
//!   |   +---------------------------+        +------------------------+  |
//!   |                                                                    |
//!   +--------------------------------------------------------------------+
//!
//!
//! The tokio runtime and response receiver task will be alive only if the user
//! connects to the server while the room buffer input callback will print an
//! error if the server is disconnected.
//!
//! The server holds all the rooms which in turn hold the buffers, users, and
//! room metadata.
//!
//! The response receiver forwards events to the correct room. The response
//! receiver fetches events individually from a mpsc channel. This makes sure
//! that processing events will not block the Weechat mainloop for too long.
use async_std::sync::channel as async_channel;
use async_std::sync::{Receiver, Sender};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;
use tokio::runtime::Runtime;
use url::Url;
use uuid::Uuid;

use matrix_sdk::api::r0::session::login::Response as LoginResponse;

use matrix_sdk::{
    self,
    events::{
        collections::all::{RoomEvent, StateEvent},
        room::message::{MessageEventContent, TextMessageEventContent},
        EventResult,
    },
    identifiers::{RoomId, UserId},
    AsyncClient, AsyncClientConfig, SyncSettings,
};

use weechat::config::{
    BooleanOptionSettings, ConfigSection, StringOptionSettings,
};
use weechat::JoinHandle;
use weechat::Weechat;

use crate::room_buffer::RoomBuffer;
use crate::Config;
use crate::PLUGIN_NAME;

const DEFAULT_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

pub enum ThreadMessage {
    LoginMessage(LoginResponse),
    SyncState(RoomId, StateEvent),
    SyncEvent(RoomId, RoomEvent),
}

#[derive(Debug)]
pub enum ServerError {
    StartError(String),
}

pub enum ServerMessage {
    RoomSend(RoomId, String),
}

#[derive(Default)]
pub struct ServerSettings {
    homeserver: Option<Url>,
    proxy: Option<Url>,
    autoconnect: bool,
    username: String,
    password: String,
}

impl ServerSettings {
    pub fn new() -> Self {
        Default::default()
    }
}

pub struct LoginState {
    user_id: UserId,
    device_id: String,
}

pub struct Connection {
    client_channel: Sender<ServerMessage>,
    response_receiver: JoinHandle<(), ()>,
    // TODO move the runtime into the plugin, once we're able to cancel tokio
    // tasks.
    runtime: Runtime,
}

impl Connection {
    pub async fn send_message(&self, room_id: &RoomId, message: &str) {
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
}

pub(crate) struct InnerServer {
    server_name: Rc<String>,
    room_buffers: HashMap<RoomId, RoomBuffer>,
    settings: ServerSettings,
    config: Config,
    client: Option<AsyncClient>,
    login_state: Option<LoginState>,
    connected_state: Rc<RefCell<Option<Connection>>>,
}

impl MatrixServer {
    pub fn new(
        name: &str,
        config: &Config,
        server_section: &mut ConfigSection,
    ) -> Self {
        let server_name = Rc::new(name.to_owned());

        let server = InnerServer {
            server_name: server_name.clone(),
            room_buffers: HashMap::new(),
            settings: ServerSettings::new(),
            config: config.clone(),
            client: None,
            login_state: None,
            connected_state: Rc::new(RefCell::new(None)),
        };

        let server = Rc::new(RefCell::new(server));
        MatrixServer::create_server_conf(&server_name, server_section, &server);

        MatrixServer {
            server_name,
            inner: server,
        }
    }

    pub fn name(&self) -> &str {
        &self.server_name
    }

    pub fn parse_homeserver_url(value: String) -> Result<(), String> {
        let url = Url::parse(&value);

        match url {
            Ok(u) => {
                if u.cannot_be_a_base() {
                    Err(String::from("The Homeserver URL is missing a schema"))
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn create_server_conf(
        server_name: &str,
        server_section: &mut ConfigSection,
        server_ref: &Rc<RefCell<InnerServer>>,
    ) {
        let server = Rc::downgrade(server_ref);
        let server_copy = server.clone();
        let autoconnect =
            BooleanOptionSettings::new(format!("{}.autoconnect", server_name))
                .set_change_callback(move |_, option| {
                    let server = server.clone();
                    let value = option.value();

                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    let mut server = server_ref.borrow_mut();
                    server.settings.autoconnect = value;
                });

        server_section
            .new_boolean_option(autoconnect)
            .expect("Can't create autoconnect option");

        let server = server_copy;
        let server_copy = server.clone();

        let homeserver =
            StringOptionSettings::new(format!("{}.homeserver", server_name))
                .set_check_callback(|_, _, value| {
                    if value.is_empty() {
                        true
                    } else {
                        MatrixServer::parse_homeserver_url(value.to_string())
                            .is_ok()
                    }
                })
                .set_change_callback(move |_, option| {
                    let server = server.clone();
                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    let mut server = server_ref.borrow_mut();
                    let homeserver = Url::parse(option.value().as_ref()).expect(
                "Can't parse homeserver URL, did the check callback fail?",
                    );

                    server.settings.homeserver = Some(homeserver)
                });

        server_section
            .new_string_option(homeserver)
            .expect("Can't create homeserver option");

        let server = server_copy;
        let server_copy = server.clone();

        let proxy = StringOptionSettings::new(format!("{}.proxy", server_name))
            .set_check_callback(|_, _, value| {
                if value.is_empty() {
                    true
                } else {
                    MatrixServer::parse_homeserver_url(value.to_string())
                        .is_ok()
                }
            })
            .set_change_callback(move |_, option| {
                let server = server.clone();
                let server_ref = server
                    .upgrade()
                    .expect("Server got deleted while server config is alive");

                let mut server = server_ref.borrow_mut();
                let proxy = Url::parse(option.value().as_ref()).expect(
                    "Can't parse proxy URL, did the check callback fail?",
                );

                server.settings.proxy = Some(proxy)
            });

        server_section
            .new_string_option(proxy)
            .expect("Can't create proxy option");

        let server = server_copy;
        let server_copy = server.clone();

        let username =
            StringOptionSettings::new(format!("{}.username", server_name))
                .set_change_callback(move |_, option| {
                    let server = server.clone();

                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    let mut server = server_ref.borrow_mut();
                    server.settings.username = option.value().to_string();
                });

        server_section
            .new_string_option(username)
            .expect("Can't create username option");

        let server = server_copy;

        let password =
            StringOptionSettings::new(format!("{}.password", server_name))
                .set_change_callback(move |_, option| {
                    let server = server.clone();

                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    let mut server = server_ref.borrow_mut();
                    server.settings.password = option.value().to_string();
                });

        server_section
            .new_string_option(password)
            .expect("Can't create password option");
    }

    pub fn connected(&self) -> bool {
        self.inner.borrow().connected()
    }

    pub fn autoconnect(&self) -> bool {
        self.inner.borrow().settings.autoconnect
    }
}

impl Drop for MatrixServer {
    fn drop(&mut self) {
        // TODO close all the server buffers.
        let config = &self.inner.borrow().config;
        let mut config_borrow = config.borrow_mut();

        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");

        for option_name in &["homeserver", "autoconnect"] {
            let option_name = &format!("{}.{}", self.name(), option_name);
            section.free_option(option_name).unwrap_or_else(|_| {
                panic!(format!("Can't free option {}", option_name))
            });
        }
    }
}

impl InnerServer {
    pub(crate) fn get_or_create_room(
        &mut self,
        room_id: &RoomId,
    ) -> &mut RoomBuffer {
        if !self.room_buffers.contains_key(room_id) {
            let homeserver = self
                .settings
                .homeserver
                .as_ref()
                .expect("Creating room buffer while no homeserver");
            let login_state = self
                .login_state
                .as_ref()
                .expect("Receiving events while not being logged in");
            let buffer = RoomBuffer::new(
                &self.server_name,
                &self.connected_state,
                homeserver,
                &self.config,
                room_id.clone(),
                &login_state.user_id,
            );
            self.room_buffers.insert(room_id.clone(), buffer);
        }

        self.room_buffers.get_mut(room_id).unwrap()
    }

    /// Is the server connected.
    pub fn connected(&self) -> bool {
        self.connected_state.borrow().is_some()
    }

    pub(crate) fn receive_joined_state_event(
        &mut self,
        room_id: &RoomId,
        event: StateEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_state_event(event)
    }

    pub(crate) fn receive_joined_timeline_event(
        &mut self,
        room_id: &RoomId,
        event: RoomEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_room_event(event)
    }

    pub fn receive_login(&mut self, response: LoginResponse) {
        let login_state = LoginState {
            user_id: response.user_id,
            device_id: response.device_id,
        };
        self.login_state = Some(login_state);
    }

    pub fn create_client(&mut self) -> Result<AsyncClient, ServerError> {
        let homeserver =
            self.settings.homeserver.as_ref().ok_or_else(|| {
                ServerError::StartError("Homeserver not configured".to_owned())
            })?;

        let mut client_config = AsyncClientConfig::new();

        if let Some(proxy) = &self.settings.proxy {
            client_config = client_config
                .proxy(proxy.as_str())
                .unwrap()
                .disable_ssl_verification();
        }

        let client = AsyncClient::new_with_config(
            homeserver.clone(),
            None,
            client_config,
        )
        .unwrap();
        self.client = Some(client.clone());
        Ok(client)
    }
}

impl MatrixServer {
    pub fn connect(&self) -> Result<(), ServerError> {
        if self.connected() {
            return Ok(());
        }

        let runtime = Runtime::new().unwrap();
        let mut server = self.inner.borrow_mut();

        let client = if let Some(c) = server.client.as_ref() {
            c.clone()
        } else {
            server.create_client()?
        };

        // Check if the homeserver setting changed and swap our client if it
        // did.
        let client = if client.homeserver()
            != server.settings.homeserver.as_ref().unwrap()
        {
            // TODO close all the room buffers of the server here, they don't
            // belong to our client anymore.
            server.create_client()?
        } else {
            client
        };

        let (tx, rx) = async_channel(1000);
        runtime.spawn(MatrixServer::sync_loop(
            client.clone(),
            tx,
            server.settings.username.to_string(),
            server.settings.password.to_string(),
        ));
        let response_receiver = Weechat::spawn(
            MatrixServer::response_receiver(rx, Rc::downgrade(&self.inner)),
        );

        let (client_sender, client_receiver) = async_channel(10);
        runtime.spawn(MatrixServer::send_loop(client, client_receiver));

        let mut connected_state = server.connected_state.borrow_mut();

        *connected_state = Some(Connection {
            response_receiver,
            client_channel: client_sender,
            runtime,
        });

        Ok(())
    }

    pub fn disconnect(&self) {
        // TODO these messages should go to the server buffer.
        if !self.connected() {
            Weechat::print(&format!(
                "{}{}: Server {}{}{} is not connected.",
                Weechat::prefix("error"),
                PLUGIN_NAME,
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            return;
        }

        let server = self.inner.borrow_mut();
        let mut connected_state = server.connected_state.borrow_mut();
        let state = connected_state.take();

        if let Some(s) = state {
            s.response_receiver.cancel();
        }

        Weechat::print(&format!("{}: Disconnected from server.", PLUGIN_NAME));
    }

    /// Main client sync loop.
    /// This runs on the per server tokio executor.
    /// It communicates with the main Weechat thread using a async channel.
    pub async fn sync_loop(
        mut client: AsyncClient,
        channel: Sender<Result<ThreadMessage, String>>,
        username: String,
        password: String,
    ) {
        if !client.logged_in().await {
            let ret = client
                .login(
                    username,
                    password,
                    None,
                    Some("Weechat-Matrix".to_owned()),
                )
                .await;

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

        let sync_token = client.sync_token().await;
        let sync_settings = SyncSettings::new().timeout(DEFAULT_SYNC_TIMEOUT);

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
                                    room_id.clone(),
                                    e,
                                )))
                                .await;
                        }
                    }
                    for event in room.timeline.events {
                        if let EventResult::Ok(e) = event {
                            channel
                                .send(Ok(ThreadMessage::SyncEvent(
                                    room_id.clone(),
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
        loop {
            let ret = match receiver.recv().await {
                Some(m) => m,
                None => {
                    Weechat::print("Error receiving message");
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
                Err(e) => Weechat::print(&format!("Ruma error {}", e)),
            };
        }
    }

    /// Send loop that waits for requests that need to be sent out using our
    /// Matrix client.
    pub async fn send_loop(
        mut client: AsyncClient,
        channel: Receiver<ServerMessage>,
    ) {
        while let Some(message) = channel.recv().await {
            match message {
                ServerMessage::RoomSend(room_id, message) => {
                    let content =
                        MessageEventContent::Text(TextMessageEventContent {
                            body: message,
                            format: None,
                            formatted_body: None,
                            relates_to: None,
                        });

                    // TODO awaiting here means we can only send one message
                    // at a time, we need to spawn a task here and return a
                    // oneshot channel that the caller can await.
                    // TODO the room should remember the UUID for the local echo
                    // implementation.
                    let ret = client
                        .room_send(&room_id, content, Some(Uuid::new_v4()))
                        .await;

                    match ret {
                        Ok(_r) => (),
                        Err(_e) => (),
                    }
                }
            }
        }
    }
}
