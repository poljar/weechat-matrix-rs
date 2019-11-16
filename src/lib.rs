mod executor;
mod room_buffer;
mod server;

use url::Url;

use tokio::runtime::Runtime;

use async_std;
use async_std::sync::channel as async_channel;
use std::collections::HashMap;

use matrix_nio::{AsyncClient, AsyncClientConfig};
use server::MatrixServer;

use weechat::{
    weechat_plugin, ArgsWeechat, Weechat, WeechatPlugin, WeechatResult,
};

use crate::executor::{cleanup_executor, spawn_weechat};

const PLUGIN_NAME: &str = "matrix";

struct Matrix {
    tokio: Option<Runtime>,
    servers: HashMap<String, MatrixServer>,
}

impl WeechatPlugin for Matrix {
    fn init(weechat: &Weechat, _args: ArgsWeechat) -> WeechatResult<Self> {
        let runtime = Runtime::new().unwrap();

        let homeserver = Url::parse("http://localhost:8008").unwrap();

        let config = AsyncClientConfig::new()
            .proxy("http://localhost:8080")
            .unwrap()
            .disable_ssl_verification();
        let client =
            AsyncClient::new_with_config(homeserver.clone(), None, config)
                .unwrap();
        let send_client = client.clone();

        let (tx, rx) = async_channel(10);

        let server_name = "localhost";

        let server = MatrixServer::new(server_name, &homeserver, tx);
        let mut servers = HashMap::new();
        servers.insert(server_name.to_owned(), server);
        runtime.spawn(async move {
            MatrixServer::send_loop(send_client, rx).await;
        });

        let (tx, rx) = async_channel(1000);

        runtime.spawn(async move {
            MatrixServer::sync_loop(client, tx).await;
        });

        spawn_weechat(MatrixServer::sync_receiver(rx));

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
