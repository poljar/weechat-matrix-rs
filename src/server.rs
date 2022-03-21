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

use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    path::PathBuf,
    rc::{Rc, Weak},
};
use tokio::runtime::Runtime;
use tracing::error;
use url::Url;

use matrix_sdk::{
    deserialized_responses::AmbiguityChange,
    encryption::verification::{Verification, VerificationRequest},
    room::Joined,
    ruma::{
        api::client::{
            session::login::v3::Response as LoginResponse, uiaa::UiaaInfo,
        },
        events::{
            room::{member::RoomMemberEventContent, message::MessageType},
            AnySyncMessageEvent, AnySyncRoomEvent, AnySyncStateEvent,
            AnyToDeviceEvent, SyncStateEvent,
        },
        identifiers::{RoomId, UserId},
    },
    store::{OpenStoreError, StateStore},
    Client,
};

use weechat::{
    buffer::{Buffer, BufferBuilder, BufferHandle},
    config::{BooleanOptionSettings, ConfigSection, StringOptionSettings},
    Prefix, Weechat,
};

use crate::{
    config::ServerBuffer,
    connection::{Connection, InteractiveAuthInfo},
    room::RoomHandle,
    verification_buffer::VerificationBuffer,
    ConfigHandle, Servers, PLUGIN_NAME,
};

#[derive(Debug)]
pub enum ServerError {
    Store(OpenStoreError),
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
    user_id: Box<UserId>,
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
    rooms: Rc<RefCell<HashMap<Box<RoomId>, RoomHandle>>>,
    settings: Rc<RefCell<ServerSettings>>,
    current_settings: Rc<RefCell<ServerSettings>>,
    config: ConfigHandle,
    client: Rc<RefCell<Option<Client>>>,
    login_state: Rc<RefCell<Option<LoginInfo>>>,
    connection: Rc<RefCell<Option<Connection>>>,
    global_runtime: Rc<Runtime>,
    server_buffer: Rc<RefCell<Option<BufferHandle>>>,
    verification_buffers: Rc<RefCell<HashMap<Box<UserId>, VerificationBuffer>>>,
}

impl MatrixServer {
    pub fn new(
        name: &str,
        config: &ConfigHandle,
        server_section: &mut ConfigSection,
        servers: Servers,
        global_runtime: Rc<Runtime>,
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
            global_runtime,
        };

        let server = server.into();

        MatrixServer::create_server_conf(&server_name, server_section, &server);

        MatrixServer { inner: server }
    }

    pub fn auth_info(&self, uiaa_info: &UiaaInfo) -> InteractiveAuthInfo {
        let settings = self.settings.borrow();

        InteractiveAuthInfo {
            user: settings.username.clone(),
            password: settings.password.clone(),
            session: uiaa_info.session.clone(),
        }
    }

    pub fn clone_weak(&self) -> Weak<InnerServer> {
        Rc::downgrade(&self.inner)
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
        let connection = Connection::new(&self, &client);
        self.set_connection(connection);

        self.print_network(&format!(
            "Connected to {}{}{}",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));

        Ok(())
    }

    fn inner(&self) -> Rc<InnerServer> {
        self.inner.clone()
    }

    pub fn merge_server_buffers(&self) {
        let server_buffer = self.inner.server_buffer.borrow_mut();

        if let Some(buffer) =
            server_buffer.as_ref().map(|b| b.upgrade().ok()).flatten()
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
                    MatrixServer::is_url_valid(&value)
                })
                .set_change_callback(move |_, option| {
                    let server_ref = server.upgrade().expect(
                        "Server got deleted while server config is alive",
                    );

                    server_ref.settings.borrow_mut().homeserver =
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

impl Drop for MatrixServer {
    fn drop(&mut self) {
        // TODO close all the server buffers.
        // Only free the server config if it's the only clone of the InnerServer
        if Rc::strong_count(&self.inner) == 1 {
            let config = &self.config;
            let mut config_borrow = config.borrow_mut();

            let mut section = config_borrow
                .search_section_mut("server")
                .expect("Can't get server section");

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
                section.free_option(option_name).unwrap_or_else(|_| {
                    panic!("Can't free option {}", option_name)
                });
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
                .get_joined_room(room_id);

            let room = room.unwrap_or_else(|| {
                panic!(
                    "Receiving events for a room while no room found {}",
                    room_id
                )
            });
            let buffer = RoomHandle::new(
                &self.server_name,
                self.global_runtime.clone(),
                &self.connection,
                self.config.inner.clone(),
                room,
                homeserver,
                room_id,
                &login_state.user_id,
            );
            self.rooms.borrow_mut().insert(room_id.into(), buffer);
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

    pub async fn restore_room(&self, room: Joined) {
        let homeserver = self
            .settings
            .borrow()
            .homeserver
            .clone()
            .expect("Creating room buffer while no homeserver");

        match RoomHandle::restore(
            &self.server_name,
            room,
            self.global_runtime.clone(),
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
            Err(e) => self.print_error(&format!(
                "Error restoring room: {}",
                e.to_string()
            )),
        }
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
    pub fn print(&self, message: &str) {
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

    pub fn create_or_update_verification_buffer(
        &self,
        verification: VerificationRequest,
    ) {
        let other_user = verification.other_user_id().to_owned();

        let buffer = self
            .verification_buffers
            .borrow()
            .get(verification.other_user_id())
            .cloned();

        if let Some(buffer) = buffer {
            buffer.replace_verification(verification);
        } else if let Ok(buffer) = VerificationBuffer::new(
            &self.server_name,
            &other_user,
            verification,
            self.connection.clone(),
            &self.verification_buffers,
        ) {
            self.verification_buffers
                .borrow_mut()
                .insert(other_user, buffer);
        } else {
            self.print_error(&format!(
                "Error creating a verification buffer for {}",
                other_user,
            ));
        }
    }

    async fn handle_verification_request(
        &self,
        sender: &UserId,
        flow_id: &str,
    ) {
        if let Some(client) = self.get_client() {
            if let Some(request) = client
                .encryption()
                .get_verification_request(sender, flow_id)
                .await
            {
                self.create_or_update_verification_buffer(request)
            }
        }
    }

    async fn handle_verification_start(&self, sender: &UserId, flow_id: &str) {
        if let Some(client) = self.get_client() {
            match client.encryption().get_verification(sender, flow_id).await {
                Some(Verification::SasV1(sas)) => {
                    if !sas.is_cancelled() {
                        let buffer = self
                            .verification_buffers
                            .borrow()
                            .get(sender)
                            .cloned();

                        if let Some(buffer) = buffer {
                            if sas.started_from_request() {
                                if let Err(e) = buffer.update(sas).await {
                                    self.print_error(&format!("Error updating the verification buffer {:?}", e))
                                }
                            } else {
                                buffer.replace_verification(sas);
                            }
                        } else {
                            if let Ok(buffer) = VerificationBuffer::new(
                                &self.server_name,
                                sender,
                                sas.clone(),
                                self.connection.clone(),
                                &self.verification_buffers,
                            ) {
                                self.verification_buffers.borrow_mut().insert(
                                    sas.other_user_id().to_owned(),
                                    buffer,
                                );
                            } else {
                                self.print_error(
                                    &format!("Error creating a verification buffer for {}", sender)
                                );
                            }
                        }
                    }
                }
                Some(Verification::QrV1(qr)) => {
                    if let Some(buffer) = self
                        .verification_buffers
                        .borrow_mut()
                        .get_mut(qr.other_user_id())
                    {
                        buffer.update_qr(qr).await;
                    }
                }
                None => {
                    // Check if we're passive and print a message if there's a
                    // buffer, otherwise this might be a stale start event.
                }
            }
        }
    }

    pub async fn receive_room_verification_event(
        &self,
        event: &AnySyncMessageEvent,
    ) {
        let handle_event = |event: AnySyncMessageEvent| async move {
            if let Some(b) =
                self.verification_buffers.borrow().get(event.sender())
            {
                b.handle_room_event(&event).await;
            }
        };

        match event {
            AnySyncMessageEvent::RoomMessage(e) => {
                if let MessageType::VerificationRequest(_) = &e.content.msgtype
                {
                    self.handle_verification_request(
                        &e.sender,
                        e.event_id.as_str(),
                    )
                    .await;
                    handle_event(event.clone()).await;
                }
            }
            AnySyncMessageEvent::KeyVerificationStart(e) => {
                self.handle_verification_start(
                    &e.sender,
                    e.content.relates_to.event_id.as_str(),
                )
                .await;
                handle_event(event.clone()).await;
            }
            AnySyncMessageEvent::KeyVerificationReady(_)
            | AnySyncMessageEvent::KeyVerificationCancel(_)
            | AnySyncMessageEvent::KeyVerificationAccept(_)
            | AnySyncMessageEvent::KeyVerificationKey(_)
            | AnySyncMessageEvent::KeyVerificationMac(_)
            | AnySyncMessageEvent::KeyVerificationDone(_) => {
                handle_event(event.clone()).await;
            }
            _ => {}
        }
    }

    pub async fn receive_to_device_event(&self, event: AnyToDeviceEvent) {
        let handle_event = |event: AnyToDeviceEvent| async move {
            if let Some(b) =
                self.verification_buffers.borrow().get(event.sender())
            {
                b.handle_event(&event).await;
            }
        };

        match &event {
            AnyToDeviceEvent::RoomKey(_) => {}
            AnyToDeviceEvent::RoomKeyRequest(_) => {}
            AnyToDeviceEvent::KeyVerificationRequest(e) => {
                self.handle_verification_request(
                    &e.sender,
                    e.content.transaction_id.as_str(),
                )
                .await;
                handle_event(event).await;
            }
            AnyToDeviceEvent::KeyVerificationStart(e) => {
                self.handle_verification_start(
                    &e.sender,
                    e.content.transaction_id.as_str(),
                )
                .await;
                handle_event(event).await;
            }
            AnyToDeviceEvent::KeyVerificationCancel(_)
            | AnyToDeviceEvent::KeyVerificationAccept(_)
            | AnyToDeviceEvent::KeyVerificationReady(_)
            | AnyToDeviceEvent::KeyVerificationKey(_)
            | AnyToDeviceEvent::KeyVerificationDone(_)
            | AnyToDeviceEvent::KeyVerificationMac(_) => {
                handle_event(event).await
            }
            _ => {}
        }
    }

    pub async fn receive_member(
        &self,
        room_id: Box<RoomId>,
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
        let room = self.get_or_create_room(room_id);
        room.handle_sync_state_event(&event, true).await
    }

    pub async fn receive_joined_timeline_event(
        &self,
        room_id: &RoomId,
        event: AnySyncRoomEvent,
    ) {
        if let AnySyncRoomEvent::Message(e) = &event {
            self.receive_room_verification_event(e).await
        }

        let room = self.get_or_create_room(room_id);
        room.handle_sync_room_event(event).await
    }

    pub fn receive_login(&self, response: LoginResponse) {
        let login_state = LoginInfo {
            user_id: response.user_id,
        };

        *self.login_state.borrow_mut() = Some(login_state);
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

        let store = StateStore::open_with_passphrase(
            self.get_server_path(),
            "DEFAULT_PASSPHRASE",
        )
        .unwrap();
        let crypto_store =
            store.open_crypto_store(Some("DEFAULT_PASSPHRASE")).unwrap();

        let mut builder = Client::builder()
            .homeserver_url(homeserver)
            .state_store(Box::new(store))
            .crypto_store(Box::new(crypto_store));

        if let Some(proxy) = settings.proxy.as_ref() {
            builder = builder.proxy(proxy.as_str());
        }

        if !settings.ssl_verify {
            builder = builder.disable_ssl_verification();
        }

        let client = self.global_runtime.block_on(builder.build()).unwrap();

        *self.current_settings.borrow_mut() = settings.clone();
        *self.client.borrow_mut() = Some(client.clone());

        Ok(client)
    }

    pub async fn export_keys(&self, file: PathBuf, passphrase: String) {
        let client = self.get_client().unwrap();

        let export = async move {
            client
                .encryption()
                .export_keys(file, &passphrase, |_| true)
                .await
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
        let client = self.get_client().unwrap();

        if let Some(c) = self.connection() {
            self.print_network(&format!(
                "Importing E2EE keys from {}, this may take a while..",
                file.display()
            ));
            let import = async move {
                client.encryption().import_keys(file, &passphrase).await
            };

            match c.spawn(import).await {
                Ok(counts) => {
                    if counts.imported_count > 0 {
                        self.print_network(&format!(
                            "Sucessfully imported {} E2EE keys",
                            counts.imported_count
                        ));
                    } else if counts.total_count > 0 {
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
            let mut connection = self.connection.borrow_mut();
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
