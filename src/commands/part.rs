use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
};

use crate::Servers;

pub struct PartCommand {
    servers: Servers,
}

/// `/part -yes` confirms leaving; a bare `/part` only warns, since leaving a
/// private room cannot be undone without a new invite.
fn part_confirmed(command_line: &str) -> bool {
    command_line
        .split_whitespace()
        .skip(1)
        .any(|arg| arg == "-yes")
}

impl PartCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/part",
            PartCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for PartCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        command_line: Cow<str>,
    ) -> ReturnCode {
        if let Some(room) = self.servers.find_room(buffer) {
            if room.close_thread_buffer(buffer) {
                return ReturnCode::OkEat;
            }

            if !part_confirmed(&command_line) {
                buffer.print(&format!(
                    "{}Leaving a room cannot always be undone: a private room \
                     needs a new invite. Run /part -yes to leave, or close \
                     the buffer to hide the room until it has activity.",
                    Weechat::prefix(Prefix::Error)
                ));
                return ReturnCode::OkEat;
            }

            Weechat::spawn(async move { room.leave().await }).detach();

            ReturnCode::OkEat
        } else {
            Weechat::print(
                "The /part command needs to be run in a Matrix room buffer.",
            );
            ReturnCode::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::part_confirmed;

    #[test]
    fn part_needs_an_explicit_yes() {
        assert!(!part_confirmed("/part"));
        assert!(!part_confirmed("/part yes"));
        assert!(part_confirmed("/part -yes"));
        assert!(part_confirmed("/part -yes some reason"));
    }
}
