use std::{borrow::Cow, collections::BTreeSet};

use matrix_sdk::ruma::MxcUri;

use weechat::{
    buffer::Buffer,
    hooks::{
        Completion, CompletionCallback, CompletionHook, CompletionPosition,
    },
    Weechat,
};

use crate::commands::is_buffer_target;
use crate::Servers;

#[allow(dead_code)]
pub struct Completions {
    buffer_short_names: CompletionHook,
    servers: CompletionHook,
    users: CompletionHook,
    media: CompletionHook,
    nicks: CompletionHook,
}

impl Completions {
    pub fn hook_all(servers: Servers) -> Result<Self, ()> {
        Ok(Self {
            buffer_short_names: BufferShortNamesCompletion::create(
                servers.clone(),
            )?,
            servers: ServersCompletion::create(servers.clone())?,
            users: UsersCompletion::create(servers.clone())?,
            media: MediaCompletion::create(servers.clone())?,
            nicks: NicksCompletion::create(servers)?,
        })
    }
}

struct BufferShortNamesCompletion {
    servers: Servers,
}

impl BufferShortNamesCompletion {
    fn create(servers: Servers) -> Result<CompletionHook, ()> {
        let comp = BufferShortNamesCompletion { servers };

        // WeeChat's /buffer completion uses this item after adding its regular
        // buffer names. Adding Matrix short names here keeps those core
        // candidates intact while making the command-run hook reachable.
        CompletionHook::new(
            "buffers_names",
            "Completion for Matrix buffer short names",
            comp,
        )
    }
}

impl CompletionCallback for BufferShortNamesCompletion {
    fn callback(
        &mut self,
        _: &Weechat,
        _: &Buffer,
        _: Cow<str>,
        completion: &Completion,
    ) -> Result<(), ()> {
        for short_name in buffer_completion_candidates(
            self.servers
                .borrow()
                .values()
                .flat_map(|server| server.rooms())
                .flat_map(|room| room.buffer_short_names()),
        ) {
            completion.add_with_options(
                &short_name,
                false,
                CompletionPosition::Sorted,
            );
        }

        Ok(())
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
                    .block_on(connection.client().encryption().tracked_users())
                    .unwrap_or_else(|e| {
                        tracing::warn!("Error getting tracked users: {e}");
                        Default::default()
                    });

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

/// Feed room member nicks into WeeChat's built-in "nicks" completion.
///
/// Thread buffers keep an empty nicklist, so the built-in fallback of
/// completing from the current buffer's nicklist finds no candidates
/// there. Adding the parent room's members here makes nick completion
/// work in thread buffers, while room buffers keep using the built-in
/// nicklist completion (this hook adds nothing for them, so WeeChat
/// falls back to it).
struct NicksCompletion {
    servers: Servers,
}

impl NicksCompletion {
    fn create(servers: Servers) -> Result<CompletionHook, ()> {
        let comp = NicksCompletion { servers };

        CompletionHook::new(
            "nick",
            "Completion for Matrix room member nicks",
            comp,
        )
    }
}

impl CompletionCallback for NicksCompletion {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
        completion: &Completion,
    ) -> Result<(), ()> {
        if buffer.get_localvar("thread_root").is_none() {
            return Ok(());
        }

        if let Some(room) = self.servers.find_room(buffer) {
            for nick in room.names() {
                completion.add_with_options(
                    &nick,
                    true,
                    CompletionPosition::Sorted,
                )
            }
        }

        Ok(())
    }
}

struct MediaCompletion {
    servers: Servers,
}

impl MediaCompletion {
    fn create(servers: Servers) -> Result<CompletionHook, ()> {
        let comp = MediaCompletion { servers };

        CompletionHook::new(
            "matrix-media",
            "Completion for Matrix media URIs in the current buffer",
            comp,
        )
    }
}

impl CompletionCallback for MediaCompletion {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
        completion: &Completion,
    ) -> Result<(), ()> {
        if self.servers.find_server(buffer).is_none() {
            return Ok(());
        }

        let mut media = BTreeSet::new();

        for line in buffer.lines().rev().take(200) {
            let message = Weechat::remove_color(&line.message());
            media.extend(extract_mxc_uris(&message));
        }

        for uri in media {
            completion.add_with_options(
                &uri,
                false,
                CompletionPosition::Sorted,
            );
        }

        Ok(())
    }
}

fn extract_mxc_uris(message: &str) -> Vec<String> {
    message
        .match_indices("mxc://")
        .filter_map(|(start, _)| {
            let uri = message[start..]
                .split(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            '<' | '>' | '[' | ']' | '(' | ')' | '"' | '\''
                        )
                })
                .next()
                .unwrap_or_default();
            let uri = Box::<MxcUri>::from(uri);

            if uri.is_valid() {
                Some(uri.as_str().to_owned())
            } else {
                None
            }
        })
        .collect()
}

fn buffer_completion_candidates<I>(short_names: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = String>,
{
    short_names
        .into_iter()
        .filter(|short_name| is_buffer_target(short_name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_valid_mxc_uris() {
        assert_eq!(
            vec!["mxc://matrix.org/some-media-id".to_owned()],
            extract_mxc_uris(
                "authenticated download: /matrix media download \
                 mxc://matrix.org/some-media-id [file]"
            )
        );
    }

    #[test]
    fn ignores_invalid_mxc_uris() {
        assert!(extract_mxc_uris("download mxc://").is_empty());
    }

    #[test]
    fn completes_only_resolvable_matrix_short_names() {
        assert_eq!(
            buffer_completion_candidates([
                "#matrix:example.org".to_owned(),
                "#matrix:example.org".to_owned(),
                "".to_owned(),
                "matrix room".to_owned(),
                "list".to_owned(),
            ]),
            BTreeSet::from(["#matrix:example.org".to_owned()]),
        );
    }
}
