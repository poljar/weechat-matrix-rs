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

use chrono::{offset::Utc, DateTime};
use std::{
    cell::{Ref, RefCell, RefMut},
    cmp::Reverse,
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::Duration,
};
use tracing::error;
use url::Url;

use matrix_sdk::{
    self,
    deserialized_responses::AmbiguityChange,
    encryption::{LocalTrust, RoomKeyImportResult},
    media::{MediaFormat, MediaRequestParameters},
    room::Room,
    ruma::{
        api::client::session::login::v3::Response as LoginResponse,
        events::{
            room::{member::RoomMemberEventContent, MediaSource},
            AnySyncStateEvent, AnySyncTimelineEvent, AnyToDeviceEvent,
            SyncStateEvent,
        },
        DeviceId, DeviceKeyAlgorithm, MilliSecondsSinceUnixEpoch,
        OwnedDeviceId, OwnedMxcUri, OwnedRoomAliasId, OwnedRoomId,
        OwnedRoomOrAliasId, OwnedServerName, OwnedUserId, RoomId, UserId,
    },
    Client, Error,
};

use weechat::{
    buffer::{Buffer, BufferBuilder, BufferHandle},
    config::{BooleanOptionSettings, ConfigSection, StringOptionSettings},
    Prefix, Weechat,
};

const JOIN_ROOM_TIMEOUT: Duration = Duration::from_secs(120);

use crate::{
    config::ServerBuffer,
    connection::{Connection, InteractiveAuthInfo},
    room::RoomHandle,
    verification_buffer::VerificationBuffer,
    ConfigHandle, Servers, PLUGIN_NAME,
};

fn with_entered_runtime_until_final_drop<F>(
    runtime: Rc<tokio::runtime::Runtime>,
    f: F,
) where
    F: FnOnce(),
{
    with_entered_runtime_until_drop(runtime, f, drop);
}

fn with_entered_runtime_until_drop<F, D>(
    runtime: Rc<tokio::runtime::Runtime>,
    f: F,
    drop_runtime: D,
) where
    F: FnOnce(),
    D: FnOnce(Rc<tokio::runtime::Runtime>),
{
    let handle = runtime.handle().clone();
    let guard = handle.enter();

    f();

    drop_runtime(runtime);
    drop(guard);
}

#[derive(Debug)]
pub enum ServerError {
    StartError(String),
    ClientError(matrix_sdk::ClientBuildError),
    IoError(String),
}

#[derive(Debug, Clone, Copy)]
enum DeviceTrust {
    Verified,
    Unverified,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerSettings {
    pub homeserver: Option<Url>,
    pub proxy: Option<Url>,
    pub autoconnect: bool,
    pub username: String,
    pub password: String,
    pub ssl_verify: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            ssl_verify: true,
            proxy: None,
            autoconnect: false,
            homeserver: None,
            username: "".to_owned(),
            password: "".to_owned(),
        }
    }
}

impl ServerSettings {
    pub fn new() -> Self {
        Default::default()
    }
}

pub struct LoginInfo {
    user_id: OwnedUserId,
}

#[derive(Clone)]
pub struct MatrixServer {
    inner: Rc<InnerServer>,
}

impl std::ops::Deref for MatrixServer {
    type Target = InnerServer;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::fmt::Debug for MatrixServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct("MatrixServer");
        fmt.field("name", &self.server_name).finish()
    }
}

pub struct InnerServer {
    servers: Servers,
    server_name: Rc<str>,
    rooms: Rc<RefCell<HashMap<OwnedRoomId, RoomHandle>>>,
    settings: Rc<RefCell<ServerSettings>>,
    current_settings: Rc<RefCell<ServerSettings>>,
    config: ConfigHandle,
    client: Rc<RefCell<Option<Client>>>,
    login_state: Rc<RefCell<Option<LoginInfo>>>,
    connection: Rc<RefCell<Option<Connection>>>,
    server_buffer: Rc<RefCell<Option<BufferHandle>>>,
    verification_buffers: Rc<RefCell<HashMap<String, VerificationBuffer>>>,
}

impl MatrixServer {
    pub fn new(
        name: &str,
        config: &ConfigHandle,
        server_section: &mut ConfigSection,
        servers: Servers,
    ) -> Self {
        let server_name: Rc<str> = name.to_string().into();

        let server = InnerServer {
            servers,
            server_name: server_name.clone(),
            rooms: Rc::new(RefCell::new(HashMap::new())),
            settings: Rc::new(RefCell::new(ServerSettings::new())),
            current_settings: Rc::new(RefCell::new(ServerSettings::new())),
            config: config.clone(),
            client: Rc::new(RefCell::new(None)),
            login_state: Rc::new(RefCell::new(None)),
            connection: Rc::new(RefCell::new(None)),
            server_buffer: Rc::new(RefCell::new(None)),
            verification_buffers: Rc::new(RefCell::new(HashMap::new())),
        };

        let server = server.into();

        MatrixServer::create_server_conf(&server_name, server_section, &server);

        MatrixServer { inner: server }
    }

    pub fn clone_weak(&self) -> Weak<InnerServer> {
        Rc::downgrade(&self.inner)
    }

    /// Join a Matrix room by ID or alias.
    pub async fn join_room(&self, room_id_or_alias: String) {
        let Ok(room_id_or_alias) =
            room_id_or_alias.parse::<OwnedRoomOrAliasId>()
        else {
            self.print_error("Invalid room ID or alias.");
            return;
        };

        let Some(connection) = self.connection() else {
            self.print_error("Not connected. Please connect first.");
            return;
        };

        self.print_network(&format!("Joining room {}...", room_id_or_alias));

        let client = connection.client().clone();
        let target = room_id_or_alias.to_string();
        let result = connection
            .spawn(async move {
                tokio::time::timeout(JOIN_ROOM_TIMEOUT, async move {
                    let servers =
                        resolve_alias_servers(&client, &room_id_or_alias).await;

                    client
                        .join_room_by_id_or_alias(&room_id_or_alias, &servers)
                        .await
                })
                .await
            })
            .await;

        match result {
            Ok(Ok(room)) => {
                self.print_network(&format!(
                    "Successfully joined room {}",
                    room.room_id()
                ));
            }
            Ok(Err(error)) => {
                self.print_error(&format!(
                    "Failed to join {}: {}",
                    target,
                    format_join_error(&error, &target)
                ));
            }
            Err(error) => {
                self.print_error(&format!(
                    "Timed out joining {} after {} seconds: {}",
                    target,
                    JOIN_ROOM_TIMEOUT.as_secs(),
                    error
                ));
            }
        }
    }

    pub fn connect(&self) -> Result<(), ServerError> {
        if self.connected() {
            self.print_error(&format!(
                "Already connected to {}{}{}",
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            return Ok(());
        }

        let client = self.get_or_create_client()?;
        let connection = Connection::new(self, &client);
        self.set_connection(connection);

        self.print_network(&format!(
            "Connected to {}{}{}",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));

        Ok(())
    }

    pub fn complete_sso_login(&self, login_token: String) {
        if self
            .get_client()
            .map(|client| client.matrix_auth().logged_in())
            .unwrap_or(false)
        {
            self.print_error(&format!(
                "Already connected to {}{}{}",
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));
            return;
        }

        if self.connected() {
            self.connection.borrow_mut().take();
        }

        let client = match self.get_or_create_client() {
            Ok(client) => client,
            Err(e) => {
                self.print_error(&format!(
                    "Failed to create Matrix client: {:?}",
                    e
                ));
                return;
            }
        };

        let server_name = self.name().to_owned();
        let server_path = self.get_server_path();

        let response = self.servers.runtime().block_on(async {
            client
                .matrix_auth()
                .login_token(&login_token)
                .initial_device_display_name("WeeChat-Matrix-rs")
                .send()
                .await
        });

        match response {
            Ok(response) => {
                if let Err(e) = Connection::save_device_id(
                    &server_name,
                    server_path,
                    &response,
                ) {
                    self.print_error(&format!(
                        "Error while writing the device id for server {}{}{}: {:?}",
                        Weechat::color("chat_server"),
                        self.name(),
                        Weechat::color("reset"),
                        e
                    ));
                    return;
                }

                self.receive_login(response);
                let connection = Connection::new(self, &client);
                self.set_connection(connection);
                self.print_network(&format!(
                    "Completed SSO login for {}{}{}",
                    Weechat::color("chat_server"),
                    self.name(),
                    Weechat::color("reset")
                ));
            }
            Err(e) => {
                self.print_error(&format!(
                    "Failed to complete SSO login: {:?}",
                    e
                ));
            }
        }
    }

    fn inner(&self) -> Rc<InnerServer> {
        self.inner.clone()
    }

    pub fn merge_server_buffers(&self) {
        let server_buffer = self.inner.server_buffer.borrow_mut();

        if let Some(buffer) =
            server_buffer.as_ref().and_then(|b| b.upgrade().ok())
        {
            self.inner.merge_server_buffer(&buffer);
        }
    }

    /// Parse an URL returning a None if the string is empty.
    ///
    /// # Panics
    ///
    /// This panics if the string can't be parsed as an URL.
    fn parse_url_unchecked(value: &str) -> Option<Url> {
        if value.is_empty() {
            None
        } else {
            Some(
                Url::parse(value)
                    .expect("Can't parse URL, did the check callback fail?"),
            )
        }
    }

    /// Parse an URL returning an error if the parse step fails.
    pub fn parse_url(value: String) -> Result<(), String> {
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

    /// Check if the provided value is a valid URL.
    fn is_url_valid(value: &str) -> bool {
        if value.is_empty() {
            true
        } else {
            MatrixServer::parse_url(value.to_string()).is_ok()
        }
    }

    fn create_server_conf(
        server_name: &str,
        server_section: &mut ConfigSection,
        server_ref: &Rc<InnerServer>,
    ) {
        let server = Rc::downgrade(server_ref);
        let server_copy = server.clone();
        let autoconnect =
            BooleanOptionSettings::new(format!("{}.autoconnect", server_name))
                .set_change_callback(move |_, option| {
                    let value = option.value();

                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    server_ref.settings.borrow_mut().autoconnect = value;
                });

        server_section
            .new_boolean_option(autoconnect)
            .expect("Can't create autoconnect option");

        let server = server_copy;
        let server_copy = server.clone();

        let homeserver =
            StringOptionSettings::new(format!("{}.homeserver", server_name))
                .set_check_callback(|_, _, value| {
                    Weechat::eval_string_expression(&value)
                        .map(|value| MatrixServer::is_url_valid(&value))
                        .unwrap_or(false)
                })
                .set_change_callback(move |_, option| {
                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    let homeserver =
                        Weechat::eval_string_expression(&option.value())
                            .expect("Can't evaluate homeserver");

                    server_ref.settings.borrow_mut().homeserver =
                        MatrixServer::parse_url_unchecked(&homeserver);
                });

        server_section
            .new_string_option(homeserver)
            .expect("Can't create homeserver option");

        let server = server_copy;
        let server_copy = server.clone();

        let proxy = StringOptionSettings::new(format!("{}.proxy", server_name))
            .set_check_callback(|_, _, value| {
                MatrixServer::is_url_valid(&value)
            })
            .set_change_callback(move |_, option| {
                let server_ref = server
                    .upgrade()
                    .expect("Server got deleted while server config is alive");

                server_ref.settings.borrow_mut().proxy =
                    MatrixServer::parse_url_unchecked(&option.value());
            });

        server_section
            .new_string_option(proxy)
            .expect("Can't create proxy option");

        let server = server_copy;
        let server_copy = server.clone();

        let username =
            StringOptionSettings::new(format!("{}.username", server_name))
                .set_change_callback(move |_, option| {
                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    server_ref.settings.borrow_mut().username =
                        Weechat::eval_string_expression(&option.value())
                            .expect("Can't evaluate username");
                });

        server_section
            .new_string_option(username)
            .expect("Can't create username option");

        let server = server_copy;
        let server_copy = server.clone();

        let password =
            StringOptionSettings::new(format!("{}.password", server_name))
                .set_change_callback(move |_, option| {
                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    server_ref.settings.borrow_mut().password =
                        Weechat::eval_string_expression(&option.value())
                            .expect("Can't evaluate password");
                });

        server_section
            .new_string_option(password)
            .expect("Can't create password option");

        let server = server_copy;

        let ssl_verify =
            BooleanOptionSettings::new(format!("{}.ssl_verify", server_name))
                .default_value(true)
                .set_change_callback(move |_, option| {
                    let value = option.value();

                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    server_ref.settings.borrow_mut().ssl_verify = value;
                });

        server_section
            .new_boolean_option(ssl_verify)
            .expect("Can't create autoconnect option");
    }
}

fn format_join_error(error: &Error, target: &str) -> String {
    let message = error
        .as_client_api_error()
        .map(ToString::to_string)
        .unwrap_or_else(|| error.to_string());

    if target.starts_with('#')
        && message.contains("Expected RoomID of the form")
    {
        format!(
            "{message}; the alias may point to a room v12 ID without a server part. Update the homeserver/client stack or join from a homeserver that supports room v12."
        )
    } else {
        message
    }
}

async fn resolve_alias_servers(
    client: &Client,
    room_id_or_alias: &OwnedRoomOrAliasId,
) -> Vec<OwnedServerName> {
    let Ok(alias) = room_id_or_alias.as_str().parse::<OwnedRoomAliasId>()
    else {
        return Vec::new();
    };

    client
        .resolve_room_alias(&alias)
        .await
        .map(|response| response.servers)
        .unwrap_or_default()
}

impl Drop for MatrixServer {
    fn drop(&mut self) {
        // TODO close all the server buffers.
        // Only free the server config if it's the only clone of the InnerServer
        if Rc::strong_count(&self.inner) == 1 {
            let config = &self.config;
            let mut config_borrow = config.borrow_mut();

            let Some(mut section) = config_borrow.search_section_mut("server")
            else {
                error!(
                    "Can't get server section while dropping Matrix server {}",
                    self.server_name
                );
                return;
            };

            for option_name in &[
                "autoconnect",
                "homeserver",
                "password",
                "proxy",
                "ssl_verify",
                "username",
            ] {
                let option_name =
                    &format!("{}.{}", self.server_name, option_name);
                if section.free_option(option_name).is_err() {
                    error!("Can't free option {}", option_name);
                }
            }
        }
    }
}

impl InnerServer {
    pub fn name(&self) -> &str {
        &self.server_name
    }

    pub fn rooms(&self) -> Vec<RoomHandle> {
        self.rooms.borrow().values().cloned().collect()
    }

    pub fn verifications(&self) -> Vec<VerificationBuffer> {
        self.verification_buffers
            .borrow()
            .values()
            .cloned()
            .collect()
    }

    pub(crate) fn get_or_create_room(&self, room_id: &RoomId) -> RoomHandle {
        if !self.rooms.borrow().contains_key(room_id) {
            let homeserver = self
                .settings
                .borrow()
                .homeserver
                .clone()
                .expect("Creating room buffer while no homeserver");
            let login_state = self.login_state.borrow();
            let login_state = login_state
                .as_ref()
                .expect("Receiving events while not being logged in");
            let client = self.client.borrow();
            let room = client
                .as_ref()
                .expect("Receiving events without a client")
                .get_room(room_id);

            let room = room.unwrap_or_else(|| {
                panic!(
                    "Receiving events for a room while no room found {}",
                    room_id
                )
            });
            let buffer = RoomHandle::new(
                &self.server_name,
                self.servers.runtime().to_owned(),
                &self.connection,
                self.config.inner.clone(),
                room,
                homeserver,
                room_id,
                &login_state.user_id,
            );
            self.rooms.borrow_mut().insert(room_id.to_owned(), buffer);
        }

        self.rooms.borrow().get(room_id).cloned().unwrap()
    }

    pub fn config(&self) -> ConfigHandle {
        self.config.clone()
    }

    pub fn user_name(&self) -> String {
        self.settings.borrow().username.clone()
    }

    pub fn password(&self) -> String {
        self.settings.borrow().password.clone()
    }

    pub fn user_id_domain(&self) -> Option<String> {
        if let Some(login_state) = self.login_state.borrow().as_ref() {
            return Some(login_state.user_id.server_name().as_str().to_owned());
        }

        self.settings
            .borrow()
            .homeserver
            .as_ref()
            .and_then(|url| url.host_str().map(str::to_owned))
    }

    /// Set the display name for this account on the homeserver.
    pub async fn set_display_name(&self, name: Option<&str>) {
        let Some(connection) = self.connection() else {
            self.print_error("Not connected to a server");
            return;
        };

        let client = connection.client().clone();
        let name = name.map(str::to_owned);
        let result = connection
            .spawn(async move {
                client.account().set_display_name(name.as_deref()).await
            })
            .await;

        if let Err(e) = result {
            self.print_error(&format!("Failed to set display name: {}", e));
        }
    }

    /// Get the current display name for this account from the homeserver.
    pub async fn get_display_name(&self) -> Option<String> {
        let connection = self.connection()?;
        let client = connection.client().clone();

        connection
            .spawn(async move { client.account().get_display_name().await })
            .await
            .ok()
            .flatten()
    }

    pub async fn restore_room_by_id(&self, room_id: OwnedRoomId) {
        if self.rooms.borrow().contains_key(&room_id) {
            return;
        }

        let Some(client) = self.get_client() else {
            return;
        };

        let Some(room) = client.get_room(&room_id) else {
            error!("Can't restore room {}, room not found", room_id);
            return;
        };

        self.restore_room(room).await;
    }

    pub async fn restore_room(&self, room: Room) {
        let homeserver = self
            .settings
            .borrow()
            .homeserver
            .clone()
            .expect("Creating room buffer while no homeserver");

        match RoomHandle::restore(
            &self.server_name,
            self.servers.runtime().to_owned(),
            room,
            &self.connection,
            self.config.inner.clone(),
            homeserver,
        )
        .await
        {
            Ok(buffer) => {
                let room_id = buffer.room_id().to_owned();

                self.rooms.borrow_mut().insert(room_id, buffer);
            }
            Err(e) => self.print_error(&format!("Error restoring room: {}", e)),
        }
    }

    pub async fn get_or_create_dm(
        &self,
        user_id: OwnedUserId,
    ) -> Option<RoomHandle> {
        let Some(connection) = self.connection() else {
            self.print_error("Not connected. Please connect first.");
            return None;
        };

        let client = connection.client().clone();
        let target = user_id.clone();
        let room = connection
            .spawn(async move {
                if let Some(room) = client.get_dm_room(&target) {
                    Ok(room)
                } else {
                    client.create_dm(&target).await
                }
            })
            .await;

        let room = match room {
            Ok(room) => room,
            Err(error) => {
                self.print_error(&format!(
                    "Failed to open direct message with {}: {}",
                    user_id, error
                ));
                return None;
            }
        };

        let room_id = room.room_id().to_owned();
        if !self.rooms.borrow().contains_key(&room_id) {
            self.restore_room(room).await;
        }

        self.rooms.borrow().get(&room_id).cloned()
    }

    fn create_server_buffer(&self) -> BufferHandle {
        let buffer_handle =
            BufferBuilder::new(&format!("server.{}", self.server_name))
                .build()
                .expect("Can't create Matrix debug buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created server buffer");

        let settings = self.settings.borrow();

        buffer.set_title(&format!(
            "Matrix: {}",
            settings
                .homeserver
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_else(|| self.server_name.to_string()),
        ));
        buffer.set_short_name(&self.server_name);
        buffer.set_localvar("type", "server");
        buffer.set_localvar("nick", &settings.username);
        buffer.set_localvar("server", &self.server_name);

        self.merge_server_buffer(&buffer);

        buffer_handle
    }

    fn merge_server_buffer(&self, buffer: &Buffer) {
        match self.config.borrow().look().server_buffer() {
            ServerBuffer::MergeWithCore => {
                buffer.unmerge();

                let core_buffer = buffer.core_buffer();
                buffer.merge(&core_buffer);
            }
            ServerBuffer::Independent => buffer.unmerge(),
            ServerBuffer::MergeWithoutCore => {
                let servers = self.servers.borrow();

                let server = if let Some(server) = servers.values().next() {
                    server
                } else {
                    return;
                };

                if server.name() == &*self.server_name {
                    buffer.unmerge();
                } else {
                    let inner = server.inner();

                    if let Some(Ok(other_buffer)) =
                        inner.server_buffer().as_ref().map(|b| b.upgrade())
                    {
                        let core_buffer = buffer.core_buffer();

                        buffer.unmerge_to((core_buffer.number() + 1) as u16);
                        buffer.merge(&other_buffer);
                    };
                }
            }
        }
    }

    fn get_client(&self) -> Option<Client> {
        self.client.borrow().clone()
    }

    fn get_or_create_client(&self) -> Result<Client, ServerError> {
        let client = if let Some(c) = self.get_client() {
            c
        } else {
            self.create_client()?
        };

        // Check if the homeserver setting changed and swap our client if it
        // did.
        if *self.current_settings.borrow() != *self.settings.borrow() {
            // TODO if the homeserver changed close all the room buffers of the
            // server here, they don't belong to our client anymore.
            self.create_client()
        } else {
            Ok(client)
        }
    }

    /// Borrow the server buffer handle.
    pub fn server_buffer(&self) -> Ref<'_, Option<BufferHandle>> {
        self.server_buffer.borrow()
    }

    fn get_or_create_buffer<'a>(
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
    fn print(&self, message: &str) {
        let mut server_buffer = self.server_buffer.borrow_mut();
        let buffer = self
            .get_or_create_buffer(&mut server_buffer)
            .upgrade()
            .unwrap();
        buffer.print(message);
    }

    /// Print a message with a given prefix to the server buffer.
    pub fn print_with_prefix(&self, prefix: &str, message: &str) {
        self.print(&format!("{}{}: {}", prefix, PLUGIN_NAME, message));
    }

    /// Print a message to a Matrix buffer, falling back to the server buffer if
    /// the original command buffer disappeared.
    fn print_with_prefix_to(
        &self,
        buffer: Option<&BufferHandle>,
        prefix: &str,
        message: &str,
    ) {
        if let Some(Ok(buffer)) = buffer.map(|buffer| buffer.upgrade()) {
            buffer.print(&format!("{}{}: {}", prefix, PLUGIN_NAME, message));
        } else {
            self.print_with_prefix(prefix, message);
        }
    }

    /// Print an network message to the server buffer.
    pub fn print_network(&self, message: &str) {
        self.print_with_prefix(&Weechat::prefix(Prefix::Network), message);
    }

    /// Print an error message to the server buffer.
    pub fn print_error(&self, message: &str) {
        self.print_with_prefix(&Weechat::prefix(Prefix::Error), message);
    }

    /// Is the server connected.
    pub fn connected(&self) -> bool {
        self.connection.borrow().is_some()
    }

    pub async fn receive_to_device_event(&self, event: AnyToDeviceEvent) {
        let handle_event = |event, transaction_id: String| async move {
            if let Some(b) =
                self.verification_buffers.borrow().get(&transaction_id)
            {
                b.handle_event(event).await;
            }
        };

        let mut refresh_status_bar = false;

        match &event {
            AnyToDeviceEvent::RoomKey(_) => {}
            AnyToDeviceEvent::RoomKeyRequest(_) => {}
            AnyToDeviceEvent::KeyVerificationRequest(e) => {
                refresh_status_bar = true;
                if let Some(client) = self.get_client() {
                    if let Some(request) = client
                        .encryption()
                        .get_verification_request(
                            &e.sender,
                            &e.content.transaction_id,
                        )
                        .await
                    {
                        let buffer = VerificationBuffer::new(
                            &self.server_name,
                            &e.sender,
                            request,
                            self.connection.clone(),
                        );
                        buffer.handle_event(&event).await;
                        self.verification_buffers.borrow_mut().insert(
                            e.content.transaction_id.to_string(),
                            buffer,
                        );
                    }
                }
            }
            AnyToDeviceEvent::KeyVerificationStart(e) => {
                refresh_status_bar = true;
                if let Some(client) = self.get_client() {
                    use matrix_sdk::encryption::verification::Verification;
                    match client
                        .encryption()
                        .get_verification(
                            &e.sender,
                            e.content.transaction_id.as_str(),
                        )
                        .await
                    {
                        Some(Verification::SasV1(sas)) => {
                            if !sas.is_cancelled() {
                                let buffer = self
                                    .verification_buffers
                                    .borrow()
                                    .get(e.content.transaction_id.as_str())
                                    .cloned();

                                if let Some(mut buffer) = buffer {
                                    let _ = buffer.update(sas).await;
                                    buffer.handle_event(&event).await;
                                } else {
                                    let buffer = VerificationBuffer::new(
                                        &self.server_name,
                                        &e.sender,
                                        sas,
                                        self.connection.clone(),
                                    );
                                    buffer.handle_event(&event).await;
                                    self.verification_buffers
                                        .borrow_mut()
                                        .insert(
                                            e.content
                                                .transaction_id
                                                .to_string(),
                                            buffer,
                                        );
                                }
                            }
                        }
                        Some(Verification::QrV1(qr)) => {
                            if let Some(buffer) = self
                                .verification_buffers
                                .borrow_mut()
                                .get_mut(e.content.transaction_id.as_str())
                            {
                                buffer.update_qr(qr).await;
                                buffer.handle_event(&event).await;
                            }
                        }
                        Some(_) => unreachable!(),
                        None => todo!(),
                    }
                }
            }
            AnyToDeviceEvent::KeyVerificationCancel(e) => {
                refresh_status_bar = true;
                handle_event(&event, e.content.transaction_id.to_string())
                    .await;
            }
            AnyToDeviceEvent::KeyVerificationAccept(e) => {
                refresh_status_bar = true;
                handle_event(&event, e.content.transaction_id.to_string()).await
            }
            AnyToDeviceEvent::KeyVerificationKey(e) => {
                refresh_status_bar = true;
                handle_event(&event, e.content.transaction_id.to_string()).await
            }
            AnyToDeviceEvent::KeyVerificationMac(e) => {
                refresh_status_bar = true;
                handle_event(&event, e.content.transaction_id.to_string()).await
            }
            AnyToDeviceEvent::KeyVerificationDone(e) => {
                refresh_status_bar = true;
                handle_event(&event, e.content.transaction_id.to_string()).await
            }
            _ => {}
        }

        if refresh_status_bar {
            Weechat::bar_item_update("buffer_modes");
        }
    }

    pub async fn receive_member(
        &self,
        room_id: OwnedRoomId,
        member: SyncStateEvent<RoomMemberEventContent>,
        is_state: bool,
        ambiguity_change: Option<AmbiguityChange>,
    ) {
        let room = self.rooms.borrow().get(&room_id).cloned();

        if let Some(room) = room {
            room.handle_membership_event(
                &member,
                is_state,
                ambiguity_change.as_ref(),
            )
            .await;
        } else {
            error!("Room with id {} not found.", room_id);
        }
    }

    pub async fn receive_joined_state_event(
        &self,
        room_id: &RoomId,
        event: AnySyncStateEvent,
    ) {
        let refresh_parent_spaces =
            matches!(&event, AnySyncStateEvent::RoomName(_));
        let room = self.get_or_create_room(room_id);
        room.handle_sync_state_event(&event, true).await;

        if refresh_parent_spaces {
            for room in self.rooms() {
                room.update_parent_spaces();
            }
        }
    }

    pub async fn receive_joined_timeline_event(
        &self,
        room_id: &RoomId,
        event: AnySyncTimelineEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_sync_room_event(event).await
    }

    pub fn receive_login(&self, response: LoginResponse) {
        let login_state = LoginInfo {
            user_id: response.user_id,
        };

        *self.login_state.borrow_mut() = Some(login_state);
    }

    pub fn receive_sso_url(&self, url: &str) {
        self.print_network(&format!(
            "Open this URL to finish SSO login for {}{}{}: {}",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset"),
            url,
        ));
    }

    fn create_server_dir(&self) -> std::io::Result<()> {
        let path = self.get_server_path();
        std::fs::create_dir_all(path)
    }

    pub fn get_server_path(&self) -> PathBuf {
        let mut path = Weechat::home_dir();
        let server_name: &str = &self.server_name;
        path.push("matrix-rust");
        path.push(server_name);

        path
    }

    fn get_server_cache_path(&self) -> PathBuf {
        let mut path = Weechat::home_dir();
        let server_name: &str = &self.server_name;
        path.push("matrix-rust");
        path.push(format!("{}-cache", server_name));

        path
    }

    pub fn connection(&self) -> Option<Connection> {
        self.connection.borrow().clone()
    }

    fn set_connection(&self, connection: Connection) {
        *self.connection.borrow_mut() = Some(connection);
    }

    pub fn create_client(&self) -> Result<Client, ServerError> {
        let settings = self.settings.borrow();

        let homeserver = settings.homeserver.as_ref().ok_or_else(|| {
            ServerError::StartError("Homeserver not configured".to_owned())
        })?;

        self.create_server_dir().map_err(|e| {
            ServerError::IoError(format!(
                "Error creating the session dir: {}",
                e
            ))
        })?;

        let mut client_builder = Client::builder()
            .homeserver_url(homeserver)
            .sqlite_store_with_cache_path(
                self.get_server_path(),
                self.get_server_cache_path(),
                Some("DEFAULT_PASSPHRASE"),
            );

        if let Some(proxy) = settings.proxy.as_ref() {
            client_builder = client_builder.proxy(proxy);
        }

        if !settings.ssl_verify {
            client_builder = client_builder.disable_ssl_verification();
        }

        let client: Client = self
            .servers
            .runtime()
            .block_on(client_builder.build())
            .map_err(ServerError::ClientError)?;

        *self.current_settings.borrow_mut() = settings.clone();
        *self.client.borrow_mut() = Some(client.clone());

        Ok(client)
    }

    pub async fn delete_devices(&self, devices: Vec<OwnedDeviceId>) {
        let formatted = devices
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<String>>()
            .join(", ");

        let print_success = || {
            self.print_network(&format!(
                "Successfully deleted device(s) {}",
                formatted
            ));
        };

        let print_fail = |e| {
            self.print_error(&format!(
                "Error deleting device(s) {} {:#?}",
                formatted, e
            ));
        };

        if let Some(c) = self.connection() {
            let delete = |auth_info| async {
                if let [device] = devices.as_slice() {
                    c.delete_device(device.clone(), auth_info).await.map(|_| ())
                } else {
                    c.delete_devices(devices.clone(), auth_info)
                        .await
                        .map(|_| ())
                }
            };

            match delete(None).await {
                Ok(_) => print_success(),
                Err(e) => {
                    if let Some(info) = e.as_uiaa_response() {
                        let auth_info = {
                            let settings = self.settings.borrow();
                            InteractiveAuthInfo {
                                user: settings.username.clone(),
                                password: settings.password.clone(),
                                session: info.session.clone(),
                            }
                        };

                        if let Err(e) = delete(Some(auth_info)).await {
                            print_fail(e);
                        } else {
                            print_success();
                        }
                    } else {
                        print_fail(e)
                    }
                }
            }
        };
    }

    pub async fn export_keys(
        &self,
        file: PathBuf,
        passphrase: String,
        room_id: Option<OwnedRoomId>,
    ) {
        let Some(client) = self.get_client() else {
            self.print_error("Can't export E2EE keys while disconnected");
            return;
        };
        let success_message = room_id.as_ref().map_or_else(
            || "Successfully exported E2EE keys".to_owned(),
            |room_id| {
                format!(
                    "Exported E2EE keys matching room {room_id}; the Matrix SDK does not report whether any sessions matched"
                )
            },
        );

        let export = async move {
            client
                .encryption()
                .export_room_keys(file, &passphrase, |session| {
                    room_id.as_ref().map_or(true, |room_id| {
                        session.room_id().as_str() == room_id.as_str()
                    })
                })
                .await
        };

        if let Some(c) = self.connection() {
            if let Err(e) = c.spawn(export).await {
                self.print_error(&format!(
                    "Error exporting E2EE keys {:#?}",
                    e
                ));
            } else {
                self.print_network(&success_message)
            }
        } else {
            self.print_error("Can't export E2EE keys while disconnected");
        }
    }

    pub async fn import_keys(&self, file: PathBuf, passphrase: String) {
        let client = self.get_client().unwrap();

        if let Some(c) = self.connection() {
            self.print_network(&format!(
                "Importing E2EE keys from {}, this may take a while..",
                file.display()
            ));
            let import = async move {
                client
                    .encryption()
                    .import_room_keys(file, &passphrase)
                    .await
            };

            match c.spawn(import).await {
                Ok(RoomKeyImportResult {
                    imported_count,
                    total_count,
                    ..
                }) => {
                    if imported_count > 0 {
                        self.print_network(&format!(
                            "Successfully imported {} E2EE keys",
                            imported_count
                        ));
                    } else if total_count > 0 {
                        self.print_network(
                            "No keys were imported, the key export contains only \
                            keys that we already have",
                        );
                    } else {
                        self.print_network(
                            "No keys were imported, either the key export is empty"
                        );
                    }
                }
                Err(e) => {
                    self.print_error(&format!(
                        "Error importing E2EE keys {:#?}",
                        e
                    ));
                }
            }
        };
    }

    pub async fn download_media(
        &self,
        uri: OwnedMxcUri,
        file: PathBuf,
        output_buffer: Option<BufferHandle>,
    ) {
        let connection = if let Some(c) = self.connection() {
            c
        } else {
            self.print_with_prefix_to(
                output_buffer.as_ref(),
                &Weechat::prefix(Prefix::Error),
                "You must be connected to execute this command",
            );
            return;
        };

        let client = connection.client().clone();
        let request = MediaRequestParameters {
            source: MediaSource::Plain(uri),
            format: MediaFormat::File,
        };
        let display_file =
            Self::display_download_path(&file).display().to_string();

        self.print_with_prefix_to(
            output_buffer.as_ref(),
            &Weechat::prefix(Prefix::Network),
            &format!("Downloading media to {}", display_file),
        );

        match connection
            .spawn(async move {
                client.media().get_media_content(&request, true).await
            })
            .await
        {
            Ok(content) => {
                if let Some(parent) = file.parent() {
                    if !parent.as_os_str().is_empty() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            self.print_with_prefix_to(
                                output_buffer.as_ref(),
                                &Weechat::prefix(Prefix::Error),
                                &format!(
                                    "Error creating media directory {}: {:#?}",
                                    parent.display(),
                                    e
                                ),
                            );
                            return;
                        }
                    }
                }

                if let Err(e) = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file)
                    .and_then(|mut file| file.write_all(&content))
                {
                    self.print_with_prefix_to(
                        output_buffer.as_ref(),
                        &Weechat::prefix(Prefix::Error),
                        &format!(
                            "Error writing media to {}: {:#?}",
                            display_file, e
                        ),
                    );
                } else {
                    self.print_with_prefix_to(
                        output_buffer.as_ref(),
                        &Weechat::prefix(Prefix::Network),
                        &format!(
                            "Successfully downloaded media to {}",
                            display_file
                        ),
                    );
                }
            }
            Err(e) => {
                self.print_with_prefix_to(
                    output_buffer.as_ref(),
                    &Weechat::prefix(Prefix::Error),
                    &format!("Error downloading media {:#?}", e),
                );
            }
        }
    }

    fn display_download_path(file: &Path) -> PathBuf {
        if file.is_absolute() {
            file.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(file))
                .unwrap_or_else(|_| file.to_path_buf())
        }
    }

    async fn list_own_devices(
        &self,
        connection: Connection,
    ) -> Result<(), Error> {
        let client = connection.client();
        let mut response = connection.devices().await?;

        if response.devices.is_empty() {
            self.print_error("No devices were found for this server");
            return Ok(());
        }

        self.print_network(&format!(
            "Devices for server {}{}{}:",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));

        response.devices.sort_by_key(|d| Reverse(d.last_seen_ts));
        let own_device_id = client.device_id();
        let own_user_id = client
            .session_meta()
            .map(|s| s.user_id.to_owned())
            .expect("Getting our own devices while not being logged in");

        let mut lines: Vec<String> = Vec::new();

        for device_info in response.devices {
            let client = client.clone();
            let own_user_id = own_user_id.clone();
            let device_info_move = device_info.clone();
            let device = match connection
                .spawn(async move {
                    client
                        .clone()
                        .encryption()
                        .get_device(&own_user_id, &device_info_move.device_id)
                        .await
                })
                .await
            {
                Ok(d) => d,
                Err(e) => {
                    self.print_error(&format!("Failed to obtain device: {e}"));
                    continue;
                }
            };

            let own_device = own_device_id == Some(&device_info.device_id);

            let device_trust = if own_device {
                DeviceTrust::Verified
            } else {
                device
                    .as_ref()
                    .map(|d| {
                        if d.is_verified() {
                            DeviceTrust::Verified
                        } else {
                            DeviceTrust::Unverified
                        }
                    })
                    .unwrap_or(DeviceTrust::Unsupported)
            };

            let info = Self::format_device(
                &device_info.device_id,
                device.and_then(|d| {
                    d.get_key(DeviceKeyAlgorithm::Ed25519)
                        .map(|f| f.to_base64())
                }),
                device_info.display_name.as_deref(),
                own_device,
                device_trust,
                device_info.last_seen_ip,
                device_info.last_seen_ts,
            );

            lines.push(info);
        }

        let line = lines.join("\n");
        self.print(&line);

        Ok(())
    }

    async fn list_other_devices(
        &self,
        connection: Connection,
        user_id: &UserId,
    ) -> Result<(), Error> {
        let devices = connection
            .client()
            .encryption()
            .get_user_devices(user_id)
            .await?;

        let lines: Vec<_> = devices
            .devices()
            .map(|device| {
                let device_trust = if device.is_verified() {
                    DeviceTrust::Verified
                } else {
                    DeviceTrust::Unverified
                };

                Self::format_device(
                    device.device_id(),
                    device
                        .get_key(DeviceKeyAlgorithm::Ed25519)
                        .map(|f| f.to_base64()),
                    device.display_name(),
                    false,
                    device_trust,
                    None,
                    None,
                )
            })
            .collect();

        let user_color = Weechat::info_get("nick_color_name", user_id.as_str())
            .expect("Can't get user color");

        if lines.is_empty() {
            self.print_error(&format!(
                "No devices were found for user {}{}{} on this server",
                Weechat::color(&user_color),
                user_id.as_str(),
                Weechat::color("reset"),
            ));
        } else {
            self.print_network(&format!(
                "Devices for user {}{}{} on server {}{}{}:",
                Weechat::color(&user_color),
                user_id.as_str(),
                Weechat::color("reset"),
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            let line = lines.join("\n");
            self.print(&line);
        }

        Ok(())
    }

    fn format_device(
        device_id: &DeviceId,
        fingerprint: Option<String>,
        display_name: Option<&str>,
        is_own_device: bool,
        device_trust: DeviceTrust,
        last_seen_ip: Option<String>,
        last_seen_ts: Option<MilliSecondsSinceUnixEpoch>,
    ) -> String {
        let device_color =
            Weechat::info_get("nick_color_name", device_id.as_str())
                .expect("Can't get device color");

        let last_seen_date = last_seen_ts
            .and_then(|d| {
                d.to_system_time().map(|d| {
                    let date: DateTime<Utc> = d.into();
                    date.format("%Y/%m/%d %H:%M").to_string()
                })
            })
            .unwrap_or_else(|| "?".to_string());

        let last_seen = format!(
            "{} @ {}",
            last_seen_ip.as_deref().unwrap_or("-"),
            last_seen_date
        );

        let (bold, color) = if is_own_device {
            (Weechat::color("bold"), format!("*{}", device_color))
        } else {
            ("", device_color)
        };

        let verified = match device_trust {
            DeviceTrust::Verified => {
                format!(
                    "{}Trusted{}",
                    Weechat::color("green"),
                    Weechat::color("reset")
                )
            }
            DeviceTrust::Unverified => {
                format!(
                    "{}Not trusted{}",
                    Weechat::color("red"),
                    Weechat::color("reset")
                )
            }
            DeviceTrust::Unsupported => {
                format!(
                    "{}No encryption support{}",
                    Weechat::color("darkgray"),
                    Weechat::color("reset")
                )
            }
        };

        let fingerprint = if let Some(fingerprint) = fingerprint {
            let fingerprint = fingerprint
                .chars()
                .collect::<Vec<char>>()
                .chunks(4)
                .map(|c| c.iter().collect::<String>())
                .collect::<Vec<String>>()
                .join(" ");

            format!(
                "{}{}{}",
                Weechat::color("magenta"),
                fingerprint,
                Weechat::color("reset")
            )
        } else {
            format!(
                "{}-{}",
                Weechat::color("darkgray"),
                Weechat::color("reset")
            )
        };

        format!(
            "       \
                                    Name: {}{}\n  \
                               Device ID: {}{}{}\n   \
                                Security: {}\n\
                             Fingerprint: {}\n  \
                               Last seen: {}\n",
            bold,
            display_name.unwrap_or(""),
            Weechat::color(&color),
            device_id.as_str(),
            Weechat::color("reset"),
            verified,
            fingerprint,
            last_seen,
        )
    }

    pub async fn devices(&self, user_id: Option<OwnedUserId>) {
        let connection = if let Some(c) = self.connection() {
            c
        } else {
            self.print_error("You must be connected to execute this command");
            return;
        };

        let ret = if let Some(user_id) = user_id.as_ref() {
            if Some(user_id.as_ref()) == connection.client().user_id() {
                self.list_own_devices(connection).await
            } else {
                self.list_other_devices(connection, user_id).await
            }
        } else {
            self.list_own_devices(connection).await
        };

        if let Err(e) = ret {
            self.print_error(&format!("Error fetching devices {:?}", e));
        }
    }

    pub async fn start_verification(&self, user_id: OwnedUserId) {
        let Some(connection) = self.connection() else {
            self.print_error("You must be connected to execute this command");
            return;
        };

        let client = connection.client().clone();
        let requested_user_id = user_id.clone();
        let result = connection
            .spawn(async move {
                let Some(identity) = client
                    .encryption()
                    .request_user_identity(&requested_user_id)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(None);
                };

                identity
                    .request_verification()
                    .await
                    .map(Some)
                    .map_err(|error| error.to_string())
            })
            .await;

        match result {
            Ok(Some(request)) => {
                self.print_network(&format!(
                    "Sent a verification request to {}",
                    user_id
                ));

                if request.room_id().is_none() {
                    let flow_id = request.flow_id().to_owned();
                    let buffer = VerificationBuffer::new(
                        &self.server_name,
                        &user_id,
                        request,
                        self.connection.clone(),
                    );
                    self.verification_buffers
                        .borrow_mut()
                        .insert(flow_id, buffer);
                }
            }
            Ok(None) => self.print_error(&format!(
                "No cross-signing identity was found for {}",
                user_id
            )),
            Err(error) => self.print_error(&format!(
                "Error starting verification with {}: {}",
                user_id, error
            )),
        }
    }

    pub async fn mark_device_verified(
        &self,
        user_id: OwnedUserId,
        device_id: OwnedDeviceId,
    ) {
        let Some(connection) = self.connection() else {
            self.print_error("You must be connected to execute this command");
            return;
        };

        let client = connection.client().clone();
        let requested_user_id = user_id.clone();
        let requested_device_id = device_id.clone();
        let result = connection
            .spawn(async move {
                client
                    .encryption()
                    .request_user_identity(&requested_user_id)
                    .await
                    .map_err(|error| error.to_string())?;

                let Some(device) = client
                    .encryption()
                    .get_device(&requested_user_id, &requested_device_id)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok::<_, String>(false);
                };

                device
                    .set_local_trust(LocalTrust::Verified)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(true)
            })
            .await;

        match result {
            Ok(true) => self.print_network(&format!(
                "Marked device {} of {} as locally verified on this client",
                device_id, user_id
            )),
            Ok(false) => self.print_error(&format!(
                "Device {} of {} was not found",
                device_id, user_id
            )),
            Err(error) => self.print_error(&format!(
                "Error marking device {} of {} as locally verified: {}",
                device_id, user_id, error
            )),
        }
    }

    pub async fn verification_info(&self, user_id: Option<OwnedUserId>) {
        let Some(connection) = self.connection() else {
            self.print_error("You must be connected to execute this command");
            return;
        };

        let client = connection.client().clone();
        let result = connection
            .spawn(async move {
                let encryption = client.encryption();
                let refresh_identity = user_id.is_some();
                let mut users = if let Some(user_id) = user_id {
                    vec![user_id]
                } else {
                    encryption
                        .tracked_users()
                        .await
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .collect::<Vec<_>>()
                };
                users.sort();

                let mut report = Vec::new();

                for user_id in users {
                    let identity = if refresh_identity {
                        encryption
                            .request_user_identity(&user_id)
                            .await
                            .map_err(|error| error.to_string())?
                    } else {
                        encryption
                            .get_user_identity(&user_id)
                            .await
                            .map_err(|error| error.to_string())?
                    };
                    let identity_state = match identity.as_ref() {
                        Some(identity)
                            if identity.has_verification_violation() =>
                        {
                            "verification violation"
                        }
                        Some(identity) if identity.is_verified() => "verified",
                        Some(identity)
                            if identity.was_previously_verified() =>
                        {
                            "previously verified"
                        }
                        Some(_) => "not verified",
                        None => "no cross-signing identity",
                    };

                    report.push(format!("{}: {}", user_id, identity_state));

                    let devices = encryption
                        .get_user_devices(&user_id)
                        .await
                        .map_err(|error| error.to_string())?;
                    let mut device_lines = devices
                        .devices()
                        .map(|device| {
                            let state = if device
                                .is_verified_with_cross_signing()
                            {
                                "verified by cross-signing"
                            } else {
                                match device.local_trust_state() {
                                    LocalTrust::Verified => {
                                        "locally trusted only"
                                    }
                                    LocalTrust::BlackListed => "blacklisted",
                                    LocalTrust::Ignored => "ignored",
                                    LocalTrust::Unset
                                        if device.is_verified() =>
                                    {
                                        "verified"
                                    }
                                    LocalTrust::Unset => "not verified",
                                }
                            };
                            let name = device
                                .display_name()
                                .map(|name| format!(" ({name})"))
                                .unwrap_or_default();

                            format!(
                                "  {}{}: {}",
                                device.device_id(),
                                name,
                                state
                            )
                        })
                        .collect::<Vec<_>>();
                    device_lines.sort();
                    report.extend(device_lines);
                }

                Ok::<_, String>(report)
            })
            .await;

        match result {
            Ok(report) if report.is_empty() => self.print_error(
                "No tracked Matrix contacts have verification information",
            ),
            Ok(report) => {
                self.print_network("Matrix verification state:");
                self.print(&report.join("\n"));
            }
            Err(error) => self.print_error(&format!(
                "Error fetching Matrix verification state: {}",
                error
            )),
        }
    }

    pub fn autoconnect(&self) -> bool {
        self.settings.borrow().autoconnect
    }

    pub fn is_connection_secure(&self) -> bool {
        let settings = self.current_settings.borrow();

        settings.ssl_verify
            && settings
                .homeserver
                .as_ref()
                .map(|u| u.scheme() == "https")
                .unwrap_or(false)
    }

    pub fn disconnect(&self) {
        if !self.connected() {
            self.print_error(&format!(
                "Not connected to {}{}{}",
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            return;
        }

        {
            let connection = self.connection.borrow_mut().take();
            if let Some(connection) = connection.as_ref() {
                connection.shutdown();
            }
            drop(connection);
        }

        self.print_network(&format!(
            "Disconnected from {}{}{}",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));
    }

    pub fn shutdown(&self) {
        let connection = self.connection.borrow_mut().take();
        let runtime = connection.as_ref().map(|c| {
            c.shutdown();
            c.runtime()
        });

        if let Some(runtime) = runtime {
            with_entered_runtime_until_final_drop(runtime, || {
                self.shutdown_sdk_state();
                drop(connection);
            });
        } else {
            let runtime = self.servers.runtime().to_owned();
            let _guard = runtime.enter();
            self.shutdown_sdk_state();
        }
    }

    fn shutdown_sdk_state(&self) {
        for verification in self.verification_buffers.borrow().values() {
            verification.release_sdk_state();
        }
        self.verification_buffers.borrow_mut().clear();

        for room in self.rooms.borrow().values() {
            room.release_sdk_state();
        }
        self.rooms.borrow_mut().clear();

        let mut client = self.client.borrow_mut();
        client.take();
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

        let settings = self.settings.borrow();
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

#[cfg(test)]
mod tests {
    use super::{with_entered_runtime_until_drop, InnerServer};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    };
    use tokio::runtime::{Handle, Runtime};

    const NOT_DROPPED: u8 = 0;
    const DROPPED_OUTSIDE_RUNTIME: u8 = 1;
    const DROPPED_INSIDE_RUNTIME: u8 = 2;

    struct DropProbe(Arc<AtomicU8>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let context = if Handle::try_current().is_ok() {
                DROPPED_INSIDE_RUNTIME
            } else {
                DROPPED_OUTSIDE_RUNTIME
            };

            self.0.store(context, Ordering::SeqCst);
        }
    }

    #[test]
    fn keeps_absolute_download_path_for_display() {
        let path = Path::new("/tmp/matrix-media");

        assert_eq!(InnerServer::display_download_path(path), path);
    }

    #[test]
    fn expands_relative_download_path_for_display() {
        let path = Path::new("matrix-media");
        let expected = std::env::current_dir()
            .expect("current working directory")
            .join(path);

        assert_eq!(InnerServer::display_download_path(path), expected);
    }

    #[test]
    fn preserves_absolute_pathbuf_for_display() {
        let path = PathBuf::from("/tmp/matrix-media");

        assert_eq!(InnerServer::display_download_path(&path), path);
    }

    #[test]
    fn final_runtime_drop_happens_while_handle_is_entered() {
        let runtime = Rc::new(Runtime::new().expect("runtime"));
        let drop_context = Arc::new(AtomicU8::new(NOT_DROPPED));
        let final_drop_context = Arc::clone(&drop_context);

        with_entered_runtime_until_drop(
            runtime,
            || {},
            |runtime| {
                let probe = DropProbe(final_drop_context);
                drop(runtime);
                drop(probe);
            },
        );

        assert_eq!(drop_context.load(Ordering::SeqCst), DROPPED_INSIDE_RUNTIME);
    }
}
