mod executor;
mod room_buffer;
mod server;

use std::collections::HashMap;
use tokio::runtime::Runtime;

use weechat::{
    weechat_plugin, ArgsWeechat, Weechat, WeechatPlugin, WeechatResult,
};

use crate::executor::cleanup_executor;
use crate::server::MatrixServer;

const PLUGIN_NAME: &str = "matrix";

struct Matrix {
    tokio: Option<Runtime>,
    servers: HashMap<String, MatrixServer>,
}

impl WeechatPlugin for Matrix {
    fn init(weechat: &Weechat, _args: ArgsWeechat) -> WeechatResult<Self> {
        let runtime = Runtime::new().unwrap();

        let server_name = "localhost";

        let mut server = MatrixServer::new(server_name);
        let mut servers = HashMap::new();

        server.connect(&runtime);
        servers.insert(server_name.to_owned(), server);

        Ok(Matrix {
            tokio: Some(runtime),
            servers,
        })
    }
}

impl Drop for Matrix {
    fn drop(&mut self) {
        let runtime = self.tokio.take();

        if let Some(r) = runtime {
            r.shutdown_now();
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
