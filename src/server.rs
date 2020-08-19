//! Matrix server abstraction.
//!
//! A MatrixServer is created for every server the user configures.
//!
//! It will create a per server config subsection. If options are added to the
//! server they need to be removed from the server section when the server is
//! dropped.
//!
//! The server will create a tokio runtime which will spawn a task for the sync
//! loop.
//!
//! It will also spawn a task on the Weechat mainloop, this one waits for
//! responses from the sync loop.
//!
//! A separate task is spawned every time Weechat wants to send a message to the
//! server.
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
//!   |   |  |                    |   |  Spawn |   |                |   |  |
//!   |   |  |  Roombuffer input  +--------------->+ Send coroutine |   |  |
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
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;
use tokio::runtime::Runtime;
use tracing::error;
use url::Url;
use uuid::Uuid;

use matrix_sdk::api::r0::session::login::Response as LoginResponse;

use matrix_sdk::{
    self,
    api::r0::{
        message::send_message_event::Response as RoomSendResponse,
        typing::create_typing_event::Response as TypingResponse,
    },
    events::{
        room::message::{MessageEventContent, TextMessageEventContent},
        AnySyncRoomEvent, AnySyncStateEvent,
    },
    identifiers::{RoomId, UserId},
    Client, ClientConfig, Result as MatrixResult, Room, SyncSettings,
};

use weechat::buffer::{BufferBuilder, BufferHandle};
use weechat::config::{
    BooleanOptionSettings, ConfigSection, StringOptionSettings,
};
use weechat::JoinHandle;
use weechat::Weechat;

use crate::room_buffer::RoomBuffer;
use crate::PLUGIN_NAME;
use crate::{config::Config, ConfigHandle};

const DEFAULT_SYNC_TIMEOUT: Duration = Duration::from_secs(30);
pub const TYPING_NOTICE_TIMEOUT: Duration = Duration::from_secs(4);

pub enum ClientMessage {
    LoginMessage(LoginResponse),
    SyncState(RoomId, AnySyncStateEvent),
    SyncEvent(RoomId, AnySyncRoomEvent),
    RestoredRoom(Room),
}

#[derive(Debug)]
pub enum ServerError {
    StartError(String),
    IoError(String),
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

pub struct LoginInfo {
    user_id: UserId,
}

pub struct Connection {
    response_receiver: JoinHandle<(), ()>,
    client: Client,
    #[used]
    runtime: Runtime,
}

impl Connection {
    pub async fn send_message(
        &self,
        room_id: &RoomId,
        message: String,
    ) -> MatrixResult<RoomSendResponse> {
        let room_id = room_id.to_owned();
        let client = self.client.clone();

        let handle = self
            .runtime
            .spawn(async move {
                let content =
                    MessageEventContent::Text(TextMessageEventContent {
                        body: message,
                        formatted: None,
                        relates_to: None,
                    });

                client
                    .room_send(&room_id, content, Some(Uuid::new_v4()))
                    .await
            })
            .await;

        match handle {
            Ok(response) => response,
            Err(e) => panic!("Tokio error while sending a message {:?}", e),
        }
    }
    pub async fn send_typing_notice(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        typing: bool,
    ) -> MatrixResult<TypingResponse> {
        let room_id = room_id.to_owned();
        let user_id = user_id.to_owned();
        let client = self.client.clone();

        let handle = self
            .runtime
            .spawn(async move {
                let timeout = if typing {
                    Some(TYPING_NOTICE_TIMEOUT)
                } else {
                    None
                };

                client
                    .typing_notice(&room_id, &user_id, typing, timeout)
                    .await
            })
            .await;

        match handle {
            Ok(response) => response,
            Err(e) => panic!("Tokio error while sending a message {:?}", e),
        }
    }
}

#[derive(Clone)]
pub(crate) struct MatrixServer {
    server_name: Rc<String>,
    inner: Rc<RefCell<InnerServer>>,
}

impl std::fmt::Debug for MatrixServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct("MatrixServer");
        fmt.field("name", &self.server_name).finish()
    }
}

pub(crate) struct InnerServer {
    server_name: Rc<String>,
    pub room_buffers: HashMap<RoomId, RoomBuffer>,
    settings: ServerSettings,
    config: ConfigHandle,
    client: Option<Client>,
    login_state: Option<LoginInfo>,
    connection: Rc<RefCell<Option<Connection>>>,
    server_buffer: Rc<RefCell<Option<BufferHandle>>>,
}

impl MatrixServer {
    pub fn new(
        name: &str,
        config: &ConfigHandle,
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
            connection: Rc::new(RefCell::new(None)),
            server_buffer: Rc::new(RefCell::new(None)),
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

    pub fn inner(&self) -> Ref<'_, InnerServer> {
        self.inner.borrow()
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

                if option.value().is_empty() {
                    server.settings.proxy = None
                } else {
                    let proxy = Url::parse(option.value().as_ref()).expect(
                        "Can't parse proxy URL, did the check callback fail?",
                    );

                    server.settings.proxy = Some(proxy)
                }
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

    fn save_device_id(
        user_name: &str,
        mut server_path: PathBuf,
        response: &LoginResponse,
    ) -> std::io::Result<()> {
        server_path.push(user_name);
        server_path.set_extension("device_id");
        std::fs::write(&server_path, &response.device_id.to_string())
    }

    fn load_device_id(
        user_name: &str,
        mut server_path: PathBuf,
    ) -> std::io::Result<Option<String>> {
        server_path.push(user_name);
        server_path.set_extension("device_id");

        let device_id = std::fs::read_to_string(server_path);

        if let Err(e) = device_id {
            // A file not found error is ok, report the rest.
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e);
            }
            return Ok(None);
        };

        let device_id = device_id.unwrap_or_default();

        if device_id.is_empty() {
            Ok(None)
        } else {
            Ok(Some(device_id))
        }
    }

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
            server.server_name.to_string(),
            server.get_server_path(),
        ));
        let response_receiver = Weechat::spawn(
            MatrixServer::response_receiver(rx, Rc::downgrade(&self.inner)),
        );

        let mut connection = server.connection.borrow_mut();

        *connection = Some(Connection {
            client,
            response_receiver,
            runtime,
        });

        Ok(())
    }

    pub fn print(&self, message: &str) {
        self.inner().print(message)
    }

    pub fn error(&self, message: &str) {
        self.inner().error(message)
    }

    pub fn disconnect(&self) {
        if !self.connected() {
            self.error(&format!(
                "{}{}: Server {}{}{} is not connected.",
                Weechat::prefix("error"),
                PLUGIN_NAME,
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            return;
        }

        {
            let server = self.inner();
            let mut connection = server.connection.borrow_mut();
            let state = connection.take();

            if let Some(s) = state {
                s.response_receiver.cancel();
            }
        }

        self.print(&format!("{}: Disconnected from server.", PLUGIN_NAME));
    }

    /// Main client sync loop.
    /// This runs on the per server tokio executor.
    /// It communicates with the main Weechat thread using a async channel.
    pub async fn sync_loop(
        client: Client,
        channel: Sender<Result<ClientMessage, String>>,
        username: String,
        password: String,
        server_name: String,
        server_path: PathBuf,
    ) {
        if !client.logged_in().await {
            let device_id =
                MatrixServer::load_device_id(&username, server_path.clone());

            let device_id = match device_id {
                Err(e) => {
                    channel
                        .send(Err(format!(
                        "Error while reading the device id for server {}: {:?}",
                        server_name, e
                    )))
                        .await;
                    return;
                }
                Ok(d) => d,
            };

            let first_login = device_id.is_none();

            let ret = client
                .login(
                    username.clone(),
                    password,
                    device_id,
                    Some("Weechat-Matrix-rs".to_owned()),
                )
                .await;

            match ret {
                Ok(response) => {
                    if let Err(e) = MatrixServer::save_device_id(
                        &username,
                        server_path.clone(),
                        &response,
                    ) {
                        channel
                            .send(Err(format!(
                            "Error while writing the device id for server {}: {:?}",
                            server_name, e
                        ))).await;
                        return;
                    }

                    channel
                        .send(Ok(ClientMessage::LoginMessage(response)))
                        .await
                }
                Err(e) => {
                    channel
                        .send(Err(format!("Failed to log in: {:?}", e)))
                        .await;
                    return;
                }
            }

            if !first_login {
                let joined_rooms = client.joined_rooms();
                for room in joined_rooms.read().await.values() {
                    let room = room.read().await;
                    let room: &Room = &*room;
                    channel
                        .send(Ok(ClientMessage::RestoredRoom(room.clone())))
                        .await
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
                        if let Ok(e) = event.deserialize() {
                            channel
                                .send(Ok(ClientMessage::SyncState(
                                    room_id.clone(),
                                    e,
                                )))
                                .await;
                        } else {
                            error!(
                                "Failed deserializing state event: {:#?}",
                                event
                            );
                        }
                    }
                    for event in room.timeline.events {
                        if let Ok(e) = event.deserialize() {
                            channel
                                .send(Ok(ClientMessage::SyncEvent(
                                    room_id.clone(),
                                    e,
                                )))
                                .await;
                        } else {
                            error!(
                                "Failed deserializing timeline event: {:#?}",
                                event
                            );
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
        receiver: Receiver<Result<ClientMessage, String>>,
        server: Weak<RefCell<InnerServer>>,
    ) {
        loop {
            let ret = receiver.recv().await;

            let server_cell = server
                .upgrade()
                .expect("Can't upgrade server in sync receiver");
            let mut server = server_cell.borrow_mut();

            let message = match ret {
                Ok(m) => m,
                Err(e) => {
                    server.error(&format!("Error receiving message: {:?}", e));
                    return;
                }
            };

            match message {
                Ok(message) => match message {
                    ClientMessage::LoginMessage(r) => server.receive_login(r),
                    ClientMessage::SyncEvent(r, e) => {
                        server.receive_joined_timeline_event(&r, e)
                    }
                    ClientMessage::SyncState(r, e) => {
                        server.receive_joined_state_event(&r, e)
                    }
                    ClientMessage::RestoredRoom(room) => {
                        server.restore_room(room)
                    }
                },
                Err(e) => server.error(&format!("Ruma error {}", e)),
            };
        }
    }

    pub fn get_info_str(&self, details: bool) -> String {
        let mut s = String::from(&format!(
            "{}{}{} [{}]",
            Weechat::color("chat_server"),
            self.server_name.as_ref().to_owned(),
            Weechat::color("reset"),
            if self.connected() {
                "connected"
            } else {
                "not connected"
            }
        ));

        if !details {
            return s;
        }

        let settings = &self.inner.borrow().settings;
        s.push_str(&format!(
            "\n\
                 {:indent$}homeserver: {}\n\
                 {:indent$}proxy: {}\n\
                 {:indent$}autoconnect: {}\n\
                 {:indent$}username: {}\n",
            "",
            settings.homeserver.as_ref().map_or("", |url| url.as_str()),
            "",
            settings.proxy.as_ref().map_or("", |url| url.as_str()),
            "",
            settings.autoconnect,
            "",
            settings.username,
            indent = 8
        ));
        s
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

        for option_name in
            &["homeserver", "autoconnect", "password", "proxy", "username"]
        {
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
                &self.connection,
                homeserver,
                room_id.clone(),
                &login_state.user_id,
            );
            self.room_buffers.insert(room_id.clone(), buffer);
        }

        self.room_buffers.get_mut(room_id).unwrap()
    }

    pub fn room_buffers(&self) -> &HashMap<RoomId, RoomBuffer> {
        &self.room_buffers
    }

    pub fn config(&self) -> Ref<Config> {
        self.config.borrow()
    }

    fn restore_room(&mut self, room: Room) {
        let homeserver = self
            .settings
            .homeserver
            .as_ref()
            .expect("Creating room buffer while no homeserver");

        let room_id = room.room_id.clone();

        let buffer = RoomBuffer::restore(
            room,
            &self.server_name,
            &self.connection,
            homeserver,
        );

        self.room_buffers.insert(room_id, buffer);
    }

    fn create_server_buffer(&self) -> BufferHandle {
        let buffer_handle =
            BufferBuilder::new(&format!("server.{}", self.server_name))
                .build()
                .expect("Can't create Matrix debug buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created server buffer");

        buffer.set_short_name(&self.server_name);
        buffer.set_localvar("type", "server");
        buffer.set_localvar("nick", &self.settings.username);
        buffer.set_localvar("server", &self.server_name);

        buffer_handle
    }

    pub fn server_buffer<'a>(
        &self,
        server_buffer: &'a mut RefMut<Option<BufferHandle>>,
    ) -> &'a BufferHandle {
        if let Some(buffer) = server_buffer.as_ref() {
            if buffer.upgrade().is_err() {
                let buffer = self.create_server_buffer();
                **server_buffer = Some(buffer);
            }
        } else {
            let buffer = self.create_server_buffer();
            **server_buffer = Some(buffer);
        }

        server_buffer.as_ref().unwrap()
    }

    /// Print a neutral message to the server buffer.
    pub fn print(&self, message: &str) {
        let mut server_buffer = self.server_buffer.borrow_mut();
        let buffer = self.server_buffer(&mut server_buffer).upgrade().unwrap();
        buffer.print(message);
    }

    /// Print an error message to the server buffer.
    pub fn error(&self, message: &str) {
        let mut server_buffer = self.server_buffer.borrow_mut();
        let buffer = self.server_buffer(&mut server_buffer).upgrade().unwrap();
        buffer.print(&format!("{}\t{}", Weechat::prefix("error"), message));
    }

    /// Is the server connected.
    pub fn connected(&self) -> bool {
        self.connection.borrow().is_some()
    }

    pub(crate) fn receive_joined_state_event(
        &mut self,
        room_id: &RoomId,
        event: AnySyncStateEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_state_event(event)
    }

    pub(crate) fn receive_joined_timeline_event(
        &mut self,
        room_id: &RoomId,
        event: AnySyncRoomEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_room_event(event)
    }

    pub fn receive_login(&mut self, response: LoginResponse) {
        let login_state = LoginInfo {
            user_id: response.user_id,
        };
        self.login_state = Some(login_state);
    }

    fn create_server_dir(&self) -> std::io::Result<()> {
        let path = self.get_server_path();
        std::fs::create_dir_all(path)
    }

    fn get_server_path(&self) -> PathBuf {
        let mut path = Weechat::home_dir();
        let server_name: &str = &self.server_name;
        path.push("matrix-rust");
        path.push(server_name);

        path
    }

    pub fn create_client(&mut self) -> Result<Client, ServerError> {
        let homeserver =
            self.settings.homeserver.as_ref().ok_or_else(|| {
                ServerError::StartError("Homeserver not configured".to_owned())
            })?;

        self.create_server_dir().map_err(|e| {
            ServerError::IoError(format!(
                "Error creating the session dir: {}",
                e
            ))
        })?;

        let mut client_config =
            ClientConfig::new().store_path(self.get_server_path());

        if let Some(proxy) = &self.settings.proxy {
            client_config = client_config
                .proxy(proxy.as_str())
                .unwrap()
                .disable_ssl_verification();
        }

        let client =
            Client::new_with_config(homeserver.clone(), client_config).unwrap();
        self.client = Some(client.clone());
        Ok(client)
    }
}
