use std::{borrow::Cow, convert::TryFrom, rc::Rc};

use matrix_sdk::ruma::UserId;
use tokio::runtime::Runtime;
use weechat::{
    buffer::Buffer,
    hooks::{
        Completion, CompletionCallback, CompletionHook, CompletionPosition,
    },
    Weechat,
};

use crate::Servers;

#[allow(dead_code)]
pub struct Completions {
    servers: CompletionHook,
    users: CompletionHook,
    devices: CompletionHook,
}

impl Completions {
    pub fn hook_all(
        servers: Servers,
        runtime: Rc<Runtime>,
    ) -> Result<Self, ()> {
        Ok(Self {
            servers: ServersCompletion::create(servers.clone())?,
            users: UsersCompletion::create(servers.clone(), runtime.clone())?,
            devices: DeviceCompletion::create(servers, runtime)?,
        })
    }
}

struct ServersCompletion {
    servers: Servers,
}

impl ServersCompletion {
    fn create(servers: Servers) -> Result<CompletionHook, ()> {
        let comp = ServersCompletion { servers };

        CompletionHook::new(
            "matrix_servers",
            "Completion for the list of added Matrix servers",
            comp,
        )
    }
}

impl CompletionCallback for ServersCompletion {
    fn callback(
        &mut self,
        _weechat: &Weechat,
        _buffer: &Buffer,
        _completion_name: Cow<str>,
        completion: &Completion,
    ) -> Result<(), ()> {
        for server_name in self.servers.borrow().keys() {
            completion.add_with_options(
                server_name,
                false,
                CompletionPosition::Sorted,
            );
        }
        Ok(())
    }
}

struct UsersCompletion {
    servers: Servers,
    runtime: Rc<Runtime>,
}

impl UsersCompletion {
    fn create(
        servers: Servers,
        runtime: Rc<Runtime>,
    ) -> Result<CompletionHook, ()> {
        let comp = UsersCompletion { servers, runtime };

        CompletionHook::new(
            "matrix-users",
            "Completion for the list of Matrix users",
            comp,
        )
    }
}

impl CompletionCallback for UsersCompletion {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
        completion: &Completion,
    ) -> Result<(), ()> {
        if let Some(server) = self.servers.find_server(buffer) {
            if let Some(connection) = server.connection() {
                let tracked_users = self
                    .runtime
                    .block_on(connection.client().encryption().tracked_users());

                for user in tracked_users.into_iter() {
                    completion.add_with_options(
                        user.as_str(),
                        true,
                        CompletionPosition::Sorted,
                    )
                }
            }
        }

        Ok(())
    }
}

struct DeviceCompletion {
    servers: Servers,
    runtime: Rc<Runtime>,
}

impl DeviceCompletion {
    fn create(
        servers: Servers,
        runtime: Rc<Runtime>,
    ) -> Result<CompletionHook, ()> {
        let comp = DeviceCompletion { servers, runtime };

        CompletionHook::new(
            "matrix-devices",
            "Completion for the list of devices a Matrix user has",
            comp,
        )
    }
}

impl CompletionCallback for DeviceCompletion {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
        completion: &Completion,
    ) -> Result<(), ()> {
        if let Some(server) = self.servers.find_server(buffer) {
            if let Some(connection) = server.connection() {
                let args = completion.arguments().unwrap_or_default();
                let args: Vec<_> = args.split_ascii_whitespace().collect();

                if let Some(user_id) =
                    args.first().and_then(|u| Box::<UserId>::try_from(*u).ok())
                {
                    let devices = self
                        .runtime
                        .block_on(
                            connection
                                .client()
                                .encryption()
                                .get_user_devices(&user_id),
                        )
                        .map_err(|_| ())?;

                    for device_id in devices.keys() {
                        completion.add_with_options(
                            device_id.as_str(),
                            true,
                            CompletionPosition::Sorted,
                        )
                    }
                }
            }
        }

        Ok(())
    }
}
