mod bar_items;
mod commands;
mod completions;
mod config;
mod connection;
mod debug;
mod render;
mod room;
mod server;
mod utils;
mod verification_buffer;

use std::{
    cell::{Ref, RefCell},
    collections::HashMap,
    rc::Rc,
};

use tracing_subscriber::layer::SubscriberExt;

use verification_buffer::VerificationBuffer;
use weechat::{
    buffer::{Buffer, BufferHandle},
    hooks::{SignalCallback, SignalData, SignalHook},
    plugin, Args, Plugin, ReturnCode, Weechat,
};

use crate::{
    bar_items::BarItems, commands::Commands, completions::Completions,
    config::ConfigHandle, room::RoomHandle, server::MatrixServer,
};

const PLUGIN_NAME: &str = "matrix";

#[derive(Clone, Debug)]
pub struct Servers(Rc<RefCell<HashMap<String, MatrixServer>>>);

#[allow(clippy::large_enum_variant)]
pub enum BufferOwner {
    Server(MatrixServer),
    Room(MatrixServer, RoomHandle),
    Verification(MatrixServer, VerificationBuffer),
    None,
}

impl BufferOwner {
    fn into_server(self) -> Option<MatrixServer> {
        match self {
            BufferOwner::Server(s) => Some(s),
            BufferOwner::Room(s, _) => Some(s),
            BufferOwner::Verification(s, _) => Some(s),
            BufferOwner::None => None,
        }
    }

    fn into_room(self) -> Option<RoomHandle> {
        if let BufferOwner::Room(_, r) = self {
            Some(r)
        } else {
            None
        }
    }
}

impl Servers {
    fn new() -> Self {
        Servers(Rc::new(RefCell::new(HashMap::new())))
    }

    fn borrow(&self) -> Ref<'_, HashMap<String, MatrixServer>> {
        self.0.borrow()
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    pub fn contains(&self, server_name: &str) -> bool {
        self.0.borrow().contains_key(server_name)
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    pub fn insert(&self, server: MatrixServer) {
        self.0
            .borrow_mut()
            .insert(server.name().to_string(), server);
    }

    pub fn get(&self, server_name: &str) -> Option<MatrixServer> {
        self.0.borrow().get(server_name).cloned()
    }

    pub fn remove(&self, server_name: &str) -> Option<MatrixServer> {
        self.0.borrow_mut().remove(server_name)
    }

    pub fn buffer_owner(&self, buffer: &Buffer) -> BufferOwner {
        let servers = self.borrow();

        for server in servers.values() {
            if let Some(b) = &*server.server_buffer() {
                if b.upgrade().map_or(false, |b| &b == buffer) {
                    return BufferOwner::Server(server.clone());
                }
            }

            for room in server.rooms() {
                let buffer_handle = room.buffer_handle();

                if let Ok(b) = buffer_handle.upgrade() {
                    if buffer == &b {
                        return BufferOwner::Room(server.clone(), room);
                    }
                }
            }

            for verification in server.verifications() {
                if let Ok(b) = verification.buffer().upgrade() {
                    if buffer == &b {
                        return BufferOwner::Verification(
                            server.clone(),
                            verification,
                        );
                    }
                }
            }
        }

        BufferOwner::None
    }

    /// Find a `MatrixServer` that the given buffer belongs to.
    ///
    /// Returns None if the buffer doesn't belong to any of our servers of
    /// rooms.
    pub fn find_server(&self, buffer: &Buffer) -> Option<MatrixServer> {
        self.buffer_owner(buffer).into_server()
    }

    /// Find a `RoomHandle` that the given buffer belongs to.
    ///
    /// Returns None if the buffer doesn't belong to any of our servers of
    /// rooms.
    pub fn find_room(&self, buffer: &Buffer) -> Option<RoomHandle> {
        self.buffer_owner(buffer).into_room()
    }
}

impl SignalCallback for Servers {
    fn callback(
        &mut self,
        _: &Weechat,
        _signal_name: &str,
        data: Option<SignalData>,
    ) -> ReturnCode {
        if let Some(SignalData::Buffer(buffer)) = data {
            if let Some(room) = self.find_room(&buffer) {
                room.update_typing_notice();
            }
        }
        ReturnCode::Ok
    }
}

struct Matrix {
    servers: Servers,
    #[allow(dead_code)]
    commands: Commands,
    config: ConfigHandle,
    #[allow(dead_code)]
    bar_items: BarItems,
    #[allow(dead_code)]
    typing_notice_signal: SignalHook,
    #[allow(dead_code)]
    completions: Completions,
    debug_buffer: RefCell<Option<BufferHandle>>,
}

impl std::fmt::Debug for Matrix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct("Matrix");
        fmt.field("servers", &self.servers).finish()
    }
}

impl Matrix {
    fn autoconnect(servers: &HashMap<String, MatrixServer>) {
        for server in servers.values() {
            if server.autoconnect() {
                match server.connect() {
                    Ok(_) => (),
                    Err(e) => Weechat::print(&format!("{:?}", e)),
                }
            }
        }
    }

    fn create_default_server(servers: Servers, config: &ConfigHandle) {
        // TODO change this to matrix.org.
        let server_name = "localhost";

        let mut config_borrow = config.borrow_mut();
        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");

        let server = MatrixServer::new(
            server_name,
            config,
            &mut section,
            servers.clone(),
        );
        servers.insert(server);
    }
}

impl Plugin for Matrix {
    fn init(_: &Weechat, _args: Args) -> Result<Self, ()> {
        let servers = Servers::new();
        let config = ConfigHandle::new(&servers);
        let commands = Commands::hook_all(&servers, &config)?;

        let bar_items = BarItems::hook_all(servers.clone())?;
        let completions = Completions::hook_all(servers.clone())?;

        let subscriber = {
            let filter =
                tracing_subscriber::filter::EnvFilter::from_default_env();

            tracing_subscriber::registry().with(filter).with(
                tracing_subscriber::fmt::layer().with_writer(debug::Debug),
            )
        };

        let _ = tracing::subscriber::set_global_default(subscriber).map_err(
            |_err| Weechat::print("Unable to set global default subscriber"),
        );

        {
            let config_borrow = config.borrow();
            if config_borrow.read().is_err() {
                return Err(());
            }
        }

        if servers.is_empty() {
            Matrix::create_default_server(servers.clone(), &config)
        }

        let typing = SignalHook::new("input_text_changed", servers.clone())
            .expect("Can't create signal hook for the typing notice cb");

        let plugin = Matrix {
            servers: servers.clone(),
            commands,
            config,
            bar_items,
            completions,
            debug_buffer: RefCell::new(None),
            typing_notice_signal: typing,
        };

        Weechat::spawn(async move {
            let servers = servers.borrow();
            Matrix::autoconnect(&servers);
        })
        .detach();

        Ok(plugin)
    }
}

impl Drop for Matrix {
    fn drop(&mut self) {
        let servers = self.servers.borrow();

        // Buffer close callbacks get called after this, so disconnect here so
        // we don't leave all our rooms.
        //
        // TODO set a flag on the server as well so we don't even try to leave
        // the rooms, once leaving the rooms is implemented when the buffer gets
        // closed.
        for server in servers.values() {
            server.disconnect();
        }

        drop(servers);

        self.servers.clear();
    }
}

plugin!(
    Matrix,
    name: "matrix",
    author: "Damir Jelić <poljar@termina.org.uk>",
    description: "Matrix protocol",
    version: "0.1.0",
    license: "ISC"
);
