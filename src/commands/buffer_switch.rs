use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct BufferSwitchCommand {
    servers: Servers,
}

impl BufferSwitchCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "2000|/buffer *",
            BufferSwitchCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for BufferSwitchCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        _: &Buffer,
        command: Cow<str>,
    ) -> ReturnCode {
        let Some(short_name) = buffer_target(&command) else {
            return ReturnCode::Ok;
        };

        let Some(buffer_handle) =
            self.servers.find_buffer_by_short_name(short_name)
        else {
            return ReturnCode::Ok;
        };

        if let Ok(buffer) = buffer_handle.upgrade() {
            buffer.switch_to();
            ReturnCode::OkEat
        } else {
            ReturnCode::Ok
        }
    }
}

fn buffer_target(command: &str) -> Option<&str> {
    let target = command.strip_prefix("/buffer")?.trim();

    is_buffer_target(target).then_some(target)
}

pub(crate) fn is_buffer_target(target: &str) -> bool {
    !target.is_empty()
        && !target.contains(char::is_whitespace)
        && !target.starts_with('-')
        && !is_core_buffer_subcommand(target)
}

fn is_core_buffer_subcommand(target: &str) -> bool {
    matches!(
        target,
        "list"
            | "clear"
            | "move"
            | "swap"
            | "cycle"
            | "merge"
            | "unmerge"
            | "hide"
            | "unhide"
            | "renumber"
            | "close"
            | "notify"
            | "localvar"
            | "set"
            | "get"
    )
}

#[cfg(test)]
mod tests {
    use super::{buffer_target, is_buffer_target};

    #[test]
    fn extracts_single_buffer_argument() {
        assert_eq!(buffer_target("/buffer #OSGeo"), Some("#OSGeo"));
    }

    #[test]
    fn ignores_empty_buffer_command() {
        assert_eq!(buffer_target("/buffer"), None);
    }

    #[test]
    fn leaves_subcommands_to_weechat() {
        assert_eq!(buffer_target("/buffer move 1"), None);
        assert_eq!(buffer_target("/buffer clear"), None);
        assert_eq!(buffer_target("/buffer -merged"), None);
    }

    #[test]
    fn recognizes_resolvable_buffer_targets() {
        assert!(is_buffer_target("#OSGeo"));
        assert!(!is_buffer_target(""));
        assert!(!is_buffer_target("matrix room"));
        assert!(!is_buffer_target("list"));
    }
}
