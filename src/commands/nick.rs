use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::{Servers, PLUGIN_NAME};

pub struct NickCommand {
    servers: Servers,
}

impl NickCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/nick",
            NickCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for NickCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        cmd: Cow<str>,
    ) -> ReturnCode {
        let new_nick = cmd.strip_prefix("/nick ").map(|s| s.trim());

        let Some(server) = self.servers.find_server(&buffer) else {
            return ReturnCode::Ok;
        };

        match new_nick {
            Some(name) if !name.is_empty() => {
                let name = name.to_owned();
                Weechat::spawn(async move {
                    server.set_display_name(Some(&name)).await;
                })
                .detach();
            }
            None => {
                // /nick with no arguments: show current display name
                Weechat::spawn(async move {
                    match server.get_display_name().await {
                        Some(name) => {
                            Weechat::print(&format!(
                                "{}: Current display name: {}",
                                PLUGIN_NAME, name
                            ));
                        }
                        None => {
                            Weechat::print(&format!(
                                "{}: No display name set.",
                                PLUGIN_NAME
                            ));
                        }
                    }
                })
                .detach();
            }
            _ => {
                Weechat::print(&format!(
                    "{}Usage: /nick <new-display-name>",
                    Weechat::prefix(weechat::Prefix::Error),
                ));
            }
        }

        ReturnCode::OkEat
    }
}
