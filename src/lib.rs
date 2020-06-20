#![feature(async_closure)]

mod commands;
mod config;
mod debug;
mod room_buffer;
mod server;

use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;
use tracing_subscriber;

use weechat::{weechat_plugin, ArgsWeechat, Weechat, WeechatPlugin};

use crate::commands::Commands;
use crate::config::Config;
use crate::server::MatrixServer;

const PLUGIN_NAME: &str = "matrix";

#[derive(Clone)]
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
}

struct Matrix {
    servers: Servers,
    #[used]
    commands: Commands,
    #[used]
    config: Config,
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
        config: &Config,
    ) {
        let server_name = "localhost";
        let mut config_borrow = config.borrow_mut();
        let mut section = config_borrow
            .search_section_mut("server")
            .expect("Can't get server section");
        let server = MatrixServer::new(server_name, config, &mut section);
        servers.insert(server_name.to_owned(), server);
    }
}

impl WeechatPlugin for Matrix {
    fn init(weechat: &Weechat, _args: ArgsWeechat) -> Result<Self, ()> {
        let servers = Servers::new();
        let config = Config::new(weechat, &servers);
        let commands = Commands::hook_all(weechat, &servers, &config);

        tracing_subscriber::fmt().with_writer(debug::Debug).init();

        let matrix = Matrix {
            servers: servers.clone(),
            commands,
            config: config.clone(),
        };

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

        Weechat::spawn(async move {
            let mut servers = servers.borrow_mut();
            Matrix::autoconnect(&mut servers);
        });

        Ok(matrix)
    }
}

impl Drop for Matrix {
    fn drop(&mut self) {
        let mut servers = self.servers.borrow_mut();
        for server in servers.values_mut() {
            server.disconnect();
        }
    }
}

weechat_plugin!(
    Matrix,
    name: "matrix",
    author: "Damir Jelić <poljar@termina.org.uk>",
    description: "Matrix protocol",
    version: "0.1.0",
    license: "ISC"
);
