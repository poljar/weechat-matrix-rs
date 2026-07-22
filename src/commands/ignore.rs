use std::borrow::Cow;

use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
};

use crate::PLUGIN_NAME;

pub struct IgnoreCommand;

#[derive(Debug, Eq, PartialEq)]
enum IgnoreDisposition {
    EatWithUnsupportedMessage,
    PassThrough,
}

impl IgnoreCommand {
    pub fn create() -> Result<[CommandRun; 2], ()> {
        Ok([
            CommandRun::new("2000|/ignore", IgnoreCommand)?,
            CommandRun::new("2000|/ignore *", IgnoreCommand)?,
        ])
    }
}

fn disposition(plugin_name: &str) -> IgnoreDisposition {
    if plugin_name == PLUGIN_NAME {
        IgnoreDisposition::EatWithUnsupportedMessage
    } else {
        IgnoreDisposition::PassThrough
    }
}

impl CommandRunCallback for IgnoreCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        _: Cow<str>,
    ) -> ReturnCode {
        match disposition(&buffer.plugin_name()) {
            IgnoreDisposition::EatWithUnsupportedMessage => {
                buffer.print(&format!(
                    "{}{}: /ignore is not supported in Matrix buffers.",
                    Weechat::prefix(Prefix::Error),
                    PLUGIN_NAME,
                ));
                ReturnCode::OkEat
            }
            IgnoreDisposition::PassThrough => ReturnCode::Ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{disposition, IgnoreDisposition};

    #[test]
    fn eats_ignore_in_matrix_buffers() {
        assert_eq!(
            IgnoreDisposition::EatWithUnsupportedMessage,
            disposition("matrix")
        );
    }

    #[test]
    fn passes_ignore_through_in_other_buffers() {
        assert_eq!(IgnoreDisposition::PassThrough, disposition("irc"));
        assert_eq!(IgnoreDisposition::PassThrough, disposition("core"));
    }
}
