#![feature(get_mut_unchecked)]

mod commands;
mod config;
mod connection;
mod debug;
mod render;
mod room;
mod server;
mod bar_items;

use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    rc::Rc,
};

use weechat::{
    buffer::{Buffer, BufferHandle},
    hooks::{SignalCallback, SignalData, SignalHook},
    plugin, Args, Plugin, ReturnCode, Weechat,
};

use crate::{
    commands::Commands, config::ConfigHandle, room::RoomHandle,
    server::MatrixServer,
    bar_items::BarItems,
};

const PLUGIN_NAME: &str = "matrix";

#[derive(Clone, Debug)]
pub struct Servers(Rc<RefCell<HashMap<String, MatrixServer>>>);

impl Servers {
    fn new() -> Self {
        Servers(Rc::new(RefCell::new(HashMap::new())))
    }

    fn borrow(&self) -> Ref<'_, HashMap<String, MatrixServer>> {
        self.0.borrow()
    }

    fn borrow_mut(&self) -> RefMut<'_, HashMap<String, MatrixServer>> {
        self.0.borrow_mut()
    }

    /// Find a `MatrixServer` that the given buffer belongs to.
    ///
    /// Returns None if the buffer doesn't belong to any of our servers of
    /// rooms.
    pub fn find_server(&self, buffer: &Buffer) -> Option<MatrixServer> {
        let servers = self.borrow();

        for server in servers.values() {
            if let Some(b) = &*server.inner().server_buffer() {
                if b.upgrade().map_or(false, |b| &b == buffer) {
                    return Some(server.clone());
                }
            }

            for room in server.inner().rooms().values() {
                let buffer_handle = room.buffer_handle();

                if let Ok(b) = buffer_handle.upgrade() {
                    if buffer == &b {
                        return Some(server.clone());
                    }
                }
            }
        }

        None
    }

    /// Find a `RoomHandle` that the given buffer belongs to.
    ///
    /// Returns None if the buffer doesn't belong to any of our servers of
    /// rooms.
    pub fn find_room(&self, buffer: &Buffer) -> Option<RoomHandle> {
        let servers = self.borrow();

        for server in servers.values() {
            for room in server.inner().rooms().values() {
                if let Ok(b) = room.buffer_handle().upgrade() {
                    if buffer == &b {
                        return Some(room.clone());
                    }
                }
            }
        }

        None
    }
}

impl SignalCallback for Servers {
    fn callback(
        &mut self,
        _: &Weechat,
        _signal_name: &str,
        data: Option<SignalData>,
    ) -> ReturnCode {
        if let Some(data) = data {
            if let SignalData::Buffer(buffer) = data {
                if let Some(room) = self.find_room(&buffer) {
                    room.update_typing_notice();
                }
            }
        }
        ReturnCode::Ok
    }
}

struct Matrix {
    servers: Servers,
    #[used]
    commands: Commands,
    #[used]
    config: ConfigHandle,
    #[used]
    bar_items: BarItems,
    #[used]
    typing_notice_signal: SignalHook,
    debug_buffer: RefCell<Option<BufferHandle>>,
}

impl std::fmt::Debug for Matrix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct("Matrix");
        fmt.field("servers", &self.servers).finish()
    }
}

impl Matrix {
    fn autoconnect(servers: &mut HashMap<String, MatrixServer>) {
        for server in servers.values_mut() {
            if server.autoconnect() {
                match server.connect() {
                    Ok(_) => (),
                    Err(e) => Weechat::print(&format!("{:?}", e)),
                }
            }
        }
    }

    fn create_default_server(
        servers: &mut HashMap<String, MatrixServer>,
        config: &ConfigHandle,
    ) {
        // TODO change this to matrix.org.
        let server_name = "localhost";

        let mut config_borrow = config.borrow_mut();
        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");

        let server = MatrixServer::new(server_name, config, &mut section);
        servers.insert(server_name.to_owned(), server);
    }
}

impl Plugin for Matrix {
    fn init(_: &Weechat, _args: Args) -> Result<Self, ()> {
        let servers = Servers::new();
        let config = ConfigHandle::new(&servers);
        let commands = Commands::hook_all(&servers, &config)?;

        // TODO move the bar creation into a separate file.
        let bar_items = BarItems::hook_all(servers.clone())?;

        tracing_subscriber::fmt()
            .with_writer(debug::Debug)
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();

        {
            let config_borrow = config.borrow();
            if config_borrow.read().is_err() {
                return Err(());
            }
        }

        {
            let mut servers_borrow = servers.borrow_mut();
            if servers_borrow.is_empty() {
                Matrix::create_default_server(&mut servers_borrow, &config)
            }
        }

        let typing = SignalHook::new("input_text_changed", servers.clone())
            .expect("Can't create signal hook for the typing notice cb");

        let plugin = Matrix {
            servers: servers.clone(),
            commands,
            config,
            bar_items,
            debug_buffer: RefCell::new(None),
            typing_notice_signal: typing,
        };

        Weechat::spawn(async move {
            let mut servers = servers.borrow_mut();
            Matrix::autoconnect(&mut servers);
        })
        .detach();

        Ok(plugin)
    }
}

impl Drop for Matrix {
    fn drop(&mut self) {
        let mut servers = self.servers.borrow_mut();

        // Buffer close callbacks get called after this, so disconnect here so
        // we don't leave all our rooms.
        //
        // TODO set a flag on the server as well so we don't even try to leave
        // the rooms, once leaving the rooms is implemented when the buffer gets
        // closed.
        for server in servers.values_mut() {
            server.disconnect();
        }
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
