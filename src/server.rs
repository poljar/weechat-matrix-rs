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
use indoc::indoc;
use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    path::PathBuf,
    rc::{Rc, Weak},
};
use url::Url;

use matrix_sdk::{
    self,
    api::r0::session::login::Response as LoginResponse,
    events::{AnySyncRoomEvent, AnySyncStateEvent},
    identifiers::{DeviceIdBox, RoomId, UserId},
    Client, ClientConfig, Room,
};

use weechat::{
    buffer::{BufferBuilder, BufferHandle},
    config::{BooleanOptionSettings, ConfigSection, StringOptionSettings},
    Prefix, Weechat,
};

use crate::{
    config::Config,
    connection::{Connection, InteractiveAuthInfo},
    room::RoomHandle,
    ConfigHandle, PLUGIN_NAME,
};

#[derive(Debug)]
pub enum ServerError {
    StartError(String),
    IoError(String),
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
    user_id: UserId,
}

#[derive(Clone)]
pub struct MatrixServer {
    #[allow(clippy::rc_buffer)]
    server_name: Rc<String>,
    inner: Rc<RefCell<InnerServer>>,
}

impl std::fmt::Debug for MatrixServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct("MatrixServer");
        fmt.field("name", &self.server_name).finish()
    }
}

pub struct InnerServer {
    #[allow(clippy::rc_buffer)]
    server_name: Rc<String>,
    rooms: HashMap<RoomId, RoomHandle>,
    settings: ServerSettings,
    current_settings: ServerSettings,
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
            rooms: HashMap::new(),
            settings: ServerSettings::new(),
            current_settings: ServerSettings::new(),
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

    pub fn clone_inner_weak(&self) -> Weak<RefCell<InnerServer>> {
        Rc::downgrade(&self.inner)
    }

    pub fn connection(&self) -> Option<Connection> {
        self.inner().connection.borrow().clone()
    }

    pub async fn delete_devices(&self, devices: Vec<DeviceIdBox>) {
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
            match c.delete_devices(devices.clone(), None).await {
                Ok(_) => print_success(),
                Err(e) => {
                    if let Some(info) = e.uiaa_response() {
                        let auth_info = InteractiveAuthInfo {
                            user: self.inner().settings().username.clone(),
                            password: self.inner().settings().password.clone(),
                            session: info.session.clone(),
                        };

                        if let Err(e) = c
                            .delete_devices(devices.clone(), Some(auth_info))
                            .await
                        {
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

    pub async fn export_keys(&self, file: PathBuf, passphrase: String) {
        let client = self.inner().get_client().unwrap();

        let export = async move {
            client.export_keys(file, &passphrase, |_| true).await
        };

        if let Some(c) = self.connection() {
            if let Err(e) = c.spawn(export).await {
                self.print_error(&format!(
                    "Error exporting E2EE keys {:#?}",
                    e
                ));
            } else {
                self.print_network("Sucessfully exported E2EE keys")
            }
        };
    }

    pub async fn import_keys(&self, file: PathBuf, passphrase: String) {
        let client = self.inner().get_client().unwrap();

        if let Some(c) = self.connection() {
            self.print_network(&format!(
                "Importing E2EE keys from {}, this may take a while..",
                file.display()
            ));
            let import =
                async move { client.import_keys(file, &passphrase).await };

            match c.spawn(import).await {
                Ok(num) => {
                    if num > 0 {
                        self.print_network(&format!(
                            "Sucessfully imported {} E2EE keys",
                            num
                        ));
                    } else {
                        self.print_network(
                            "No keys were imported, either the export \
                                   is empty or it contains sessions that we \
                                   already have",
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

    pub async fn devices(&self) {
        if let Some(c) = self.connection() {
            let response = match c.devices().await {
                Ok(r) => r,
                Err(e) => {
                    self.print_error(&format!(
                        "Error fetching devices {:?}",
                        e
                    ));
                    return;
                }
            };

            if response.devices.is_empty() {
                self.print_error("No devices were found for this server");
                return;
            }

            self.print_network(&format!(
                indoc!(
                    "
                    Devices for server {}{}{}:
                      Device ID      Device name                   Last seen
                "
                ),
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            let lines: Vec<String> = response
                .devices
                .iter()
                .map(|d| {
                    let device_color = Weechat::info_get(
                        "nick_color_name",
                        d.device_id.as_str(),
                    )
                    .expect("Can't get device color");

                    let last_seen_date =
                        d.last_seen_ts.map_or("?".to_owned(), |d| {
                            let date: DateTime<Utc> = d.into();
                            date.format("%Y/%m/%d %H:%M").to_string()
                        });

                    let last_seen = format!(
                        "{} @ {}",
                        d.last_seen_ip.as_deref().unwrap_or("-"),
                        last_seen_date
                    );

                    format!(
                        "  {}{:<15}{}{:<30}{:<}",
                        Weechat::color(&device_color),
                        d.device_id.as_str(),
                        Weechat::color("reset"),
                        d.display_name.as_deref().unwrap_or(""),
                        last_seen,
                    )
                })
                .collect();

            let line = lines.join("\n");
            self.print_network(&line);
        };
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
        server_ref: &Rc<RefCell<InnerServer>>,
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
                    MatrixServer::is_url_valid(&value)
                })
                .set_change_callback(move |_, option| {
                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    let mut server = server_ref.borrow_mut();

                    server.settings.homeserver =
                        MatrixServer::parse_url_unchecked(&option.value());
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

                let mut server = server_ref.borrow_mut();
                server.settings.proxy =
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

                    let mut server = server_ref.borrow_mut();
                    server.settings.username = option.value().to_string();
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

                    let mut server = server_ref.borrow_mut();
                    server.settings.password = option.value().to_string();
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

                    let mut server = server_ref.borrow_mut();
                    server.settings.ssl_verify = value;
                });

        server_section
            .new_boolean_option(ssl_verify)
            .expect("Can't create autoconnect option");
    }

    pub fn connected(&self) -> bool {
        self.inner.borrow().connected()
    }

    pub fn autoconnect(&self) -> bool {
        self.inner.borrow().settings.autoconnect
    }

    pub fn is_connection_secure(&self) -> bool {
        self.inner.borrow().current_settings.ssl_verify
            && self
                .inner
                .borrow()
                .current_settings
                .homeserver
                .as_ref()
                .map(|u| u.scheme() == "https")
                .unwrap_or(false)
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

        let client = self.inner.borrow_mut().get_or_create_client()?;
        let connection = Connection::new(&self, &client);

        *self.inner.borrow_mut().connection.borrow_mut() = Some(connection);

        self.print_network(&format!(
            "Connected to {}{}{}",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));

        Ok(())
    }

    pub fn print(&self, message: &str) {
        self.inner().print(message)
    }

    pub fn print_with_prefix(&self, prefix: &str, message: &str) {
        self.inner().print_with_prefix(prefix, message)
    }

    pub fn print_network(&self, message: &str) {
        self.inner().print_network(message)
    }

    pub fn print_error(&self, message: &str) {
        self.inner().print_error(message)
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
            let server = self.inner();
            let mut connection = server.connection.borrow_mut();
            connection.take();
        }

        self.print_network(&format!(
            "Disconnected from {}{}{}",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));
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

impl Drop for InnerServer {
    fn drop(&mut self) {
        // TODO close all the server buffers.
        let config = &self.config;
        let mut config_borrow = config.borrow_mut();

        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");

        for option_name in
            &["homeserver", "autoconnect", "password", "proxy", "username"]
        {
            let option_name = &format!("{}.{}", self.server_name, option_name);
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
    ) -> &RoomHandle {
        if !self.rooms.contains_key(room_id) {
            let homeserver = self
                .settings
                .homeserver
                .as_ref()
                .expect("Creating room buffer while no homeserver");
            let login_state = self
                .login_state
                .as_ref()
                .expect("Receiving events while not being logged in");
            let room = self
                .client
                .as_ref()
                .expect("Receiving events without a client")
                .get_joined_room(room_id);

            let room = room.expect(&format!(
                "Receiving events for a room while no room found {}",
                room_id
            ));
            let buffer = RoomHandle::new(
                &self.server_name,
                &self.connection,
                self.config.inner.clone(),
                room,
                homeserver,
                room_id.clone(),
                &login_state.user_id,
            );
            self.rooms.insert(room_id.clone(), buffer);
        }

        self.rooms.get_mut(room_id).unwrap()
    }

    pub fn settings(&self) -> &ServerSettings {
        &self.settings
    }

    pub fn rooms(&self) -> &HashMap<RoomId, RoomHandle> {
        &self.rooms
    }

    pub fn config(&self) -> Ref<Config> {
        self.config.borrow()
    }

    pub async fn restore_room(&mut self, _room: Room) {
        // let homeserver = self
        //     .settings
        //     .homeserver
        //     .as_ref()
        //     .expect("Creating room buffer while no homeserver");

        // let buffer = RoomHandle::restore(
        //     room,
        //     &self.connection,
        //     self.config.inner.clone(),
        //     homeserver,
        // )
        // .await;
        // let room_id = buffer.room_id().to_owned();

        // self.rooms.insert(room_id, buffer);
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

    fn get_client(&self) -> Option<Client> {
        self.client.clone()
    }

    fn get_or_create_client(&mut self) -> Result<Client, ServerError> {
        let client = if let Some(c) = self.client.as_ref() {
            c.clone()
        } else {
            self.create_client()?
        };

        // Check if the homeserver setting changed and swap our client if it
        // did.
        if self.current_settings != self.settings {
            // TODO if the homeserver changed close all the room buffers of the
            // server here, they don't belong to our client anymore.
            self.create_client()
        } else {
            Ok(client)
        }
    }

    /// Borrow the server buffer handle.
    pub fn server_buffer(&self) -> Ref<Option<BufferHandle>> {
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

    pub fn receive_joined_state_event(
        &mut self,
        room_id: &RoomId,
        event: AnySyncStateEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_sync_state_event(event)
    }

    pub async fn receive_joined_timeline_event(
        &mut self,
        room_id: &RoomId,
        event: AnySyncRoomEvent,
    ) {
        let room = self.get_or_create_room(room_id);
        room.handle_sync_room_event(event).await
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

    pub fn get_server_path(&self) -> PathBuf {
        let mut path = Weechat::home_dir();
        let server_name: &str = &self.server_name;
        path.push("matrix-rust");
        path.push(server_name);

        path
    }

    pub fn create_client(&mut self) -> Result<Client, ServerError> {
        let settings = self.settings.clone();

        let homeserver = settings.homeserver.as_ref().ok_or_else(|| {
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

        if let Some(proxy) = settings.proxy.as_ref() {
            client_config = client_config.proxy(proxy.as_str()).unwrap();
        }

        if !settings.ssl_verify {
            client_config = client_config.disable_ssl_verification();
        }

        let client =
            Client::new_with_config(homeserver.clone(), client_config).unwrap();
        self.current_settings = settings;
        self.client = Some(client.clone());

        Ok(client)
    }
}
