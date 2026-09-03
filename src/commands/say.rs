use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    ReturnCode, Weechat,
};

use crate::Servers;

pub struct SayCommand {
    servers: Servers,
}

fn say_body(command: &str) -> Option<&str> {
    command.strip_prefix("/say ").map(str::trim_start)
}

impl SayCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/say *",
            SayCommand {
                servers: servers.clone(),
            },
        )
    }
}

impl CommandRunCallback for SayCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        command: Cow<str>,
    ) -> ReturnCode {
        let Some(room) = self.servers.find_room(buffer) else {
            return ReturnCode::Ok;
        };

        let Some(body) = say_body(&command) else {
            return ReturnCode::Ok;
        };

        let content = room.text_message_content(buffer, body.to_owned());
        Weechat::spawn(async move { room.send_message(content).await })
            .detach();

        ReturnCode::OkEat
    }
}

#[cfg(test)]
mod tests {
    use super::say_body;

    #[test]
    fn say_body_preserves_message_text() {
        assert_eq!(say_body("/say hello Matrix"), Some("hello Matrix"));
        assert_eq!(say_body("/say   leading spaces"), Some("leading spaces"));
    }

    #[test]
    fn say_body_excludes_other_commands() {
        assert_eq!(say_body("/msg hello"), None);
        assert_eq!(say_body("hello"), None);
    }
}
