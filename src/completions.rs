use std::borrow::Cow;

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
}

impl Completions {
    pub fn hook_all(servers: Servers) -> Result<Self, ()> {
        Ok(Self {
            servers: ServersCompletion::create(servers.clone())?,
            users: UsersCompletion::create(servers)?,
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
}

impl UsersCompletion {
    fn create(servers: Servers) -> Result<CompletionHook, ()> {
        let comp = UsersCompletion { servers };

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
                    .servers
                    .runtime()
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
