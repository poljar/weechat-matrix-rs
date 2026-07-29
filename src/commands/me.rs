use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct MeCommand {
    servers: Servers,
}

const ME_COMMAND_PATTERN: &str = "/me *";

fn action_body(command: &str) -> Option<&str> {
    command.strip_prefix("/me ")
}

impl MeCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            ME_COMMAND_PATTERN,
            MeCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for MeCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        cmd: Cow<str>,
    ) -> ReturnCode {
        let Some(room) = self.servers.find_room(buffer) else {
            return ReturnCode::Ok;
        };

        let Some(body) = action_body(&cmd) else {
            return ReturnCode::Ok;
        };

        self.servers
            .runtime()
            .block_on(room.send_emote(buffer, body.to_owned()));

        ReturnCode::OkEat
    }
}

#[cfg(test)]
mod tests {
    use super::{action_body, ME_COMMAND_PATTERN};

    #[test]
    fn me_hook_matches_action_arguments_before_command_resolution() {
        assert_eq!(ME_COMMAND_PATTERN, "/me *");
    }

    #[test]
    fn me_action_body_excludes_other_commands() {
        assert_eq!(action_body("/me waves"), Some("waves"));
        assert_eq!(action_body("/message waves"), None);
    }
}
