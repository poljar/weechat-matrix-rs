use std::{cell::RefCell, path::PathBuf, rc::Weak, sync::Arc, time::Duration};

use async_std::sync::{channel as async_channel, Receiver, Sender};
use tokio::runtime::Runtime;
use tracing::error;
use uuid::Uuid;

pub use matrix_sdk::{
    self,
    api::r0::{
        device::get_devices::Response as DevicesResponse,
        message::send_message_event::Response as RoomSendResponse,
        session::login::Response as LoginResponse,
        typing::create_typing_event::{Response as TypingResponse, Typing},
    },
    events::{
        room::message::{MessageEventContent, TextMessageEventContent},
        AnyMessageEventContent, AnySyncRoomEvent, AnySyncStateEvent,
    },
    identifiers::{RoomId, UserId},
    locks::RwLock,
    Client, ClientConfig, LoopCtrl, Result as MatrixResult, Room, SyncSettings,
};

use weechat::{Task, Weechat};

use crate::server::{InnerServer, MatrixServer};

const DEFAULT_SYNC_TIMEOUT: Duration = Duration::from_secs(30);
pub const TYPING_NOTICE_TIMEOUT: Duration = Duration::from_secs(4);

pub enum ClientMessage {
    LoginMessage(LoginResponse),
    SyncState(RoomId, AnySyncStateEvent),
    SyncEvent(RoomId, AnySyncRoomEvent),
    RestoredRoom(Arc<RwLock<Room>>),
}

/// Struc representing an active connection to the homeserver.
///
/// Since the rust-sdk `Client` object uses reqwest for the HTTP client making
/// requests requires the request to be made on a tokio runtime. The connection
/// wraps the `Client` object and makes sure that requests are run on the
/// runtime the `Connection` holds.
///
/// While this struct is alive a sync loop will be going on. To cancel the sync
/// loop drop the object.
pub struct Connection {
    #[used]
    receiver_task: Task<()>,
    client: Client,
    #[used]
    runtime: Runtime,
}

impl Connection {
    pub fn new(server: &MatrixServer, client: &Client) -> Self {
        let (tx, rx) = async_channel(1000);

        let server_name = server.name();

        let receiver_task = Weechat::spawn(Connection::response_receiver(
            rx,
            server.clone_inner_weak(),
        ));

        let server = server.inner();

        let runtime = Runtime::new().unwrap();

        let settings = server.settings();

        runtime.spawn(Connection::sync_loop(
            client.clone(),
            tx,
            settings.username.to_string(),
            settings.password.to_string(),
            server_name.to_string(),
            server.get_server_path(),
        ));

        Self {
            client: client.clone(),
            runtime,
            receiver_task,
        }
    }

    /// Send a message to the given room.
    ///
    /// # Arguments
    ///
    /// * `room_id` - The id of the room which the message should be sent to.
    ///
    /// * `content` - The content of the message that will be sent to the
    /// server.
    ///
    /// * `transaction_id` - Attach an unique id to this message, later on the
    /// event will contain the same id in the unsigned part of the event.
    pub async fn send_message(
        &self,
        room_id: &RoomId,
        content: AnyMessageEventContent,
        transaction_id: Option<Uuid>,
    ) -> MatrixResult<RoomSendResponse> {
        let room_id = room_id.to_owned();
        let client = self.client.clone();

        let handle = self
            .runtime
            .spawn(async move {
                client
                    .room_send(
                        &room_id,
                        content,
                        Some(transaction_id.unwrap_or_else(Uuid::new_v4)),
                    )
                    .await
            })
            .await;

        match handle {
            Ok(response) => response,
            Err(e) => panic!("Tokio error while sending a message {:?}", e),
        }
    }

    /// Get the list of our own devices.
    pub async fn devices(&self) -> MatrixResult<DevicesResponse> {
        let client = self.client.clone();
        let handle = self
            .runtime
            .spawn(async move { client.devices().await })
            .await;

        match handle {
            Ok(response) => response,
            Err(e) => panic!("Tokio error while sending a message {:?}", e),
        }
    }

    /// Set or reset a typing notice.
    ///
    /// # Arguments
    ///
    /// * `room_id` - The id of the room where the typing notice should be
    /// active.
    ///
    /// * `typing` - Should we set or unset the typing notice.
    pub async fn send_typing_notice(
        &self,
        room_id: &RoomId,
        typing: bool,
    ) -> MatrixResult<TypingResponse> {
        let room_id = room_id.to_owned();
        let client = self.client.clone();

        let handle = self
            .runtime
            .spawn(async move {
                let typing = if typing {
                    Typing::Yes(TYPING_NOTICE_TIMEOUT)
                } else {
                    Typing::No
                };

                client.typing_notice(&room_id, typing).await
            })
            .await;

        match handle {
            Ok(response) => response,
            Err(e) => panic!("Tokio error while sending a message {:?}", e),
        }
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
                    server.print_error(&format!(
                        "Error receiving message: {:?}",
                        e
                    ));
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
                Err(e) => server.print_error(&format!("Ruma error {}", e)),
            };
        }
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
                Connection::load_device_id(&username, server_path.clone());

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
                    &username,
                    &password,
                    device_id.as_deref(),
                    Some("Weechat-Matrix-rs"),
                )
                .await;

            match ret {
                Ok(response) => {
                    if let Err(e) = Connection::save_device_id(
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
            .sync_with_callback(sync_settings, async move |response| {
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

                LoopCtrl::Continue
            })
            .await;
    }
}
