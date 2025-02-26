use std::{
    future::Future,
    path::PathBuf,
    rc::{Rc, Weak},
    time::Duration,
};

use tokio::{
    runtime::Runtime,
    sync::mpsc::{channel, Receiver, Sender},
};

use tracing::error;

use matrix_sdk::{
    self,
    config::SyncSettings,
    deserialized_responses::AmbiguityChange,
    room::{Messages, MessagesOptions, Room},
    ruma::{
        api::client::{
            device::{
                delete_devices::v3::Response as DeleteDevicesResponse,
                get_devices::v3::Response as DevicesResponse,
            },
            filter::{
                FilterDefinition, LazyLoadOptions, RoomEventFilter, RoomFilter,
            },
            message::send_message_event::v3::Response as RoomSendResponse,
            session::login::v3::Response as LoginResponse,
            sync::sync_events::v3::Filter,
            uiaa::{AuthData, Password, UserIdentifier},
        },
        events::{
            room::member::RoomMemberEventContent, AnyMessageLikeEventContent,
            AnySyncStateEvent, AnySyncTimelineEvent, AnyToDeviceEvent,
            SyncStateEvent,
        },
        OwnedDeviceId, OwnedRoomId, OwnedTransactionId,
    },
    Client, LoopCtrl, Result as MatrixResult, RoomMemberships,
};

use weechat::{Task, Weechat};

use crate::{
    room::PrevBatch,
    server::{InnerServer, MatrixServer},
};

const DEFAULT_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

pub struct InteractiveAuthInfo {
    pub user: String,
    pub password: String,
    pub session: Option<String>,
}

impl InteractiveAuthInfo {
    pub fn as_auth_data(&self) -> AuthData {
        AuthData::Password(Password::new(
            UserIdentifier::UserIdOrLocalpart(self.user.clone()),
            self.password.clone(),
        ))
    }
}

pub enum ClientMessage {
    LoginMessage(LoginResponse),
    SyncState(OwnedRoomId, AnySyncStateEvent),
    SyncEvent(OwnedRoomId, AnySyncTimelineEvent),
    ToDeviceEvent(AnyToDeviceEvent),
    MemberEvent(
        OwnedRoomId,
        SyncStateEvent<RoomMemberEventContent>,
        bool,
        Option<AmbiguityChange>,
    ),
    RestoredRoom(Room),
}

/// Struct representing an active connection to the homeserver.
///
/// Since the rust-sdk `Client` object uses reqwest for the HTTP client making
/// requests requires the request to be made on a tokio runtime. The connection
/// wraps the `Client` object and makes sure that requests are run on the
/// runtime the `Connection` holds.
///
/// While this struct is alive a sync loop will be going on. To cancel the sync
/// loop drop the object.
#[derive(Debug, Clone)]
pub struct Connection {
    #[allow(dead_code)]
    receiver_task: Rc<Task<()>>,
    client: Client,
    pub runtime: Rc<Runtime>,
}

impl Connection {
    pub fn client(&self) -> &Client {
        &self.client
    }

    pub async fn spawn<F>(&self, future: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime
            .spawn(future)
            .await
            .expect("Tokio error while sending a message")
    }

    pub fn new(server: &MatrixServer, client: &Client) -> Self {
        let (tx, rx) = channel(10_000);

        let server_name = server.name();

        let receiver_task = Weechat::spawn(Connection::response_receiver(
            rx,
            server.clone_weak(),
        ));

        let runtime = Runtime::new().unwrap();

        runtime.spawn(Connection::sync_loop(
            client.clone(),
            tx,
            server.user_name(),
            server.password(),
            server_name.to_string(),
            server.get_server_path(),
        ));

        Self {
            client: client.clone(),
            runtime: runtime.into(),
            receiver_task: receiver_task.into(),
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
        room: Room,
        content: AnyMessageLikeEventContent,
        transaction_id: Option<OwnedTransactionId>,
    ) -> MatrixResult<RoomSendResponse> {
        self.spawn(async move {
            let mut msg = room.send(content);
            if let Some(txn_id) = transaction_id.as_deref() {
                msg = msg.with_transaction_id(txn_id);
            }

            msg.await
        })
        .await
    }

    pub async fn delete_devices(
        &self,
        devices: Vec<OwnedDeviceId>,
        auth_info: Option<InteractiveAuthInfo>,
    ) -> MatrixResult<DeleteDevicesResponse> {
        let client = self.client.clone();
        Ok(self
            .spawn(async move {
                if let Some(info) = auth_info {
                    let auth = Some(info.as_auth_data());
                    client.delete_devices(&devices, auth).await
                } else {
                    client.delete_devices(&devices, None).await
                }
            })
            .await?)
    }

    /// Fetch historical messages for the given room.
    pub async fn room_messages(
        &self,
        room: Room,
        prev_batch: PrevBatch,
    ) -> MatrixResult<Messages> {
        self.spawn(async move {
            let request = match &prev_batch {
                PrevBatch::Backwards(t) => {
                    MessagesOptions::backward().from(Some(t.as_ref()))
                }
                PrevBatch::Forward(t) => {
                    MessagesOptions::forward().from(Some(t.as_ref()))
                }
            };

            room.messages(request).await
        })
        .await
    }

    /// Get the list of our own devices.
    pub async fn devices(&self) -> MatrixResult<DevicesResponse> {
        let client = self.client.clone();
        Ok(self.spawn(async move { client.devices().await }).await?)
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
        room: Room,
        typing: bool,
    ) -> MatrixResult<()> {
        self.spawn(async move { room.typing_notice(typing).await })
            .await
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
        mut receiver: Receiver<Result<ClientMessage, String>>,
        server: Weak<InnerServer>,
    ) {
        while let Some(message) = receiver.recv().await {
            let server = if let Some(s) = server.upgrade() {
                s
            } else {
                return;
            };

            match message {
                Ok(message) => match message {
                    ClientMessage::LoginMessage(r) => server.receive_login(r),
                    ClientMessage::SyncEvent(r, e) => {
                        server.receive_joined_timeline_event(&r, e).await
                    }
                    ClientMessage::SyncState(r, e) => {
                        server.receive_joined_state_event(&r, e).await
                    }
                    ClientMessage::RestoredRoom(room) => {
                        server.restore_room(room).await
                    }
                    ClientMessage::ToDeviceEvent(e) => {
                        server.receive_to_device_event(e).await
                    }
                    ClientMessage::MemberEvent(
                        room_id,
                        e,
                        is_state,
                        change,
                    ) => {
                        server
                            .receive_member(room_id, e, is_state, change)
                            .await
                    }
                },
                Err(e) => server.print_error(&format!("Ruma error {}", e)),
            };
        }
    }

    #[allow(clippy::field_reassign_with_default)]
    fn sync_filter() -> FilterDefinition {
        let mut filter = FilterDefinition::default();
        let mut room_filter = RoomFilter::default();
        let mut event_filter = RoomEventFilter::default();

        event_filter.lazy_load_options = LazyLoadOptions::Enabled {
            include_redundant_members: false,
        };
        event_filter.limit = Some(10u16.into());

        room_filter.state = event_filter;
        filter.room = room_filter;

        filter
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
        if !client.logged_in() {
            let device_id =
                Connection::load_device_id(&username, server_path.clone());

            let device_id = match device_id {
                Err(e) => {
                    // TODO: do we want to do something with channel.send()
                    // errors?
                    let _ = channel
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

            let mut builder = client
                .matrix_auth()
                .login_username(&username, &password)
                .initial_device_display_name("WeeChat-Matrix-rs");

            if let Some(device_id) = device_id.as_ref() {
                builder = builder.device_id(device_id);
            };

            match builder.send().await {
                Ok(response) => {
                    if let Err(e) = Connection::save_device_id(
                        &username,
                        server_path.clone(),
                        &response,
                    ) {
                        let _ = channel
                            .send(Err(format!(
                            "Error while writing the device id for server {}: {:?}",
                            server_name, e
                        ))).await;
                        return;
                    }

                    if channel
                        .send(Ok(ClientMessage::LoginMessage(response)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(e) => {
                    let _ = channel
                        .send(Err(format!("Failed to log in: {:?}", e)))
                        .await;
                    return;
                }
            }

            if !first_login {
                for room in client.joined_rooms() {
                    if channel
                        .send(Ok(ClientMessage::RestoredRoom(room)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }

        let filter = client
            .get_or_upload_filter("sync", Connection::sync_filter())
            .await
            .unwrap();

        let sync_token: Option<String> = None;
        let sync_settings = SyncSettings::new()
            .timeout(DEFAULT_SYNC_TIMEOUT)
            .filter(Filter::FilterId(filter));

        let sync_settings = if let Some(t) = sync_token {
            sync_settings.token(t)
        } else {
            sync_settings
        };

        let sync_channel = &channel;

        let client_ref = &client;

        let _ret = client
            .sync_with_callback(sync_settings, |response| async move {
                for event in response.to_device.iter().filter_map(|e| e.deserialize().ok()){
                    if sync_channel
                        .send(Ok(ClientMessage::ToDeviceEvent(event)))
                        .await
                        .is_err()
                    {
                        return LoopCtrl::Break;
                    }
                }

                for (room_id, room) in response.rooms.join {
                    for event in
                        room.state.iter().filter_map(|e| e.deserialize().ok())
                    {
                        if let AnySyncStateEvent::RoomMember(m) = event {
                            let change = response
                                .ambiguity_changes
                                .changes
                                .get(&room_id)
                                .and_then(|c| c.get(m.event_id()))
                                .cloned();

                            if sync_channel
                                .send(Ok(ClientMessage::MemberEvent(
                                    room_id.clone(),
                                    m,
                                    true,
                                    change,
                                )))
                                .await
                                .is_err()
                            {
                                return LoopCtrl::Break;
                            }
                        } else if sync_channel
                            .send(Ok(ClientMessage::SyncState(
                                room_id.clone(),
                                event,
                            )))
                            .await
                            .is_err()
                        {
                            return LoopCtrl::Break;
                        }
                    }

                    for event in room
                        .timeline
                        .events
                        .iter()
                        .filter_map(|e| e.event.deserialize().ok())
                    {
                        if let AnySyncTimelineEvent::State(
                            AnySyncStateEvent::RoomMember(m),
                        ) = event
                        {
                            let change = response
                                .ambiguity_changes
                                .changes
                                .get(&room_id)
                                .and_then(|c| c.get(m.event_id()))
                                .cloned();

                            if sync_channel
                                .send(Ok(ClientMessage::MemberEvent(
                                    room_id.clone(),
                                    m,
                                    false,
                                    change,
                                )))
                                .await
                                .is_err()
                            {
                                return LoopCtrl::Break;
                            }
                        } else if sync_channel
                            .send(Ok(ClientMessage::SyncEvent(
                                room_id.clone(),
                                event,
                            )))
                            .await
                            .is_err()
                        {
                            return LoopCtrl::Break;
                        }
                    }

                    if let Some(r) = client_ref.get_room(&room_id) {
                        if !r.are_members_synced() {
                            let room_id = room_id.clone();
                            let channel = sync_channel.clone();

                            tokio::spawn(async move {
                                if let Ok(members) =
                                    r.members(RoomMemberships::ACTIVE).await
                                {
                                    for member in members {
                                        if let Err(e) = channel
                                            .send(Ok(
                                                ClientMessage::MemberEvent(
                                                    room_id.clone(),
                                                    member.event().as_sync().expect("Member event is not a StateSyncEvent").to_owned(),
                                                    true,
                                                    None,
                                                ),
                                            ))
                                            .await
                                        {
                                            error!(
                                                "Failed to send room member {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            });
                        }
                    }
                }

                LoopCtrl::Continue
            })
            .await;
    }
}
