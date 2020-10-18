use clap::{App, ArgMatches};
use weechat::{hooks::Command, Args, Weechat};

use crate::{config::ConfigHandle, Servers};

mod devices;
mod keys;
mod matrix;

use devices::DevicesCommand;
use keys::KeysCommand;
use matrix::MatrixCommand;

pub struct Commands {
    _matrix: Command,
    _keys: Command,
    _devices: Command,
}

impl Commands {
    pub fn hook_all(
        servers: &Servers,
        config: &ConfigHandle,
    ) -> Result<Commands, ()> {
        Ok(Commands {
            _matrix: MatrixCommand::create(servers, config)?,
            _devices: DevicesCommand::create(servers)?,
            _keys: KeysCommand::create(servers)?,
        })
    }
}

fn parse_and_run(
    parser: App,
    arguments: Args,
    command: impl FnOnce(&ArgMatches),
) {
    match parser.get_matches_from_safe(arguments) {
        Ok(m) => command(&m),
        Err(e) => {
            let error = Weechat::execute_modifier(
                "color_decode_ansi",
                "1",
                &e.to_string(),
            )
            .expect("Can't color decode ansi string");
            Weechat::print(&error);
        }
    }
}
