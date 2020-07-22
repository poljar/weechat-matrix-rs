use crate::config::ConfigHandle;
use crate::Servers;
use weechat::hooks::Command;

mod matrix;

use matrix::MatrixCommand;

pub struct Commands {
    _matrix: Command,
}

impl Commands {
    pub fn hook_all(
        servers: &Servers,
        config: &ConfigHandle,
    ) -> Result<Commands, ()> {
        Ok(Commands {
            _matrix: MatrixCommand::create(servers, config)?,
        })
    }
}
