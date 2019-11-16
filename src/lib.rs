mod executor;
mod room_buffer;
mod server;

use std::collections::HashMap;
use std::time::Duration;

use weechat::{
    weechat_plugin, ArgsWeechat, Weechat, WeechatPlugin, WeechatResult,
};

use crate::executor::{cleanup_executor, spawn_weechat};
use crate::server::MatrixServer;

const PLUGIN_NAME: &str = "matrix";

struct Matrix {
    servers: HashMap<String, MatrixServer>,
}

impl Matrix {
    fn autoconnect(&mut self) {
        for server in self.servers.values_mut() {
            server.connect();
        }
    }
}

impl WeechatPlugin for Matrix {
    fn init(weechat: &Weechat, _args: ArgsWeechat) -> WeechatResult<Self> {
        let server_name = "localhost";

        let mut server = MatrixServer::new(server_name);
        let mut servers = HashMap::new();

        servers.insert(server_name.to_owned(), server);

        spawn_weechat(async move {
            async_std::task::sleep(Duration::from_secs(1)).await;
            let matrix = plugin();
            matrix.autoconnect();
        });

        Ok(Matrix { servers })
    }
}

impl Drop for Matrix {
    fn drop(&mut self) {
        for server in self.servers.values_mut() {
            server.disconnect();
        }

        cleanup_executor();
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
