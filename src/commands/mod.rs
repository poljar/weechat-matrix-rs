use clap::{App, ArgMatches};
use verification::VerificationCommand;
use weechat::{
    hooks::{Command, CommandRun},
    Args, Weechat,
};

use crate::{config::ConfigHandle, Servers};

mod buffer_clear;
mod devices;
mod keys;
mod matrix;
mod page_up;
mod verification;

use buffer_clear::BufferClearCommand;
use devices::DevicesCommand;
use keys::KeysCommand;
use matrix::MatrixCommand;
use page_up::PageUpCommand;

pub struct Commands {
    _matrix: Command,
    _keys: Command,
    _devices: Command,
    _page_up: CommandRun,
    _verification: Command,
    _buffer_clear: CommandRun,
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
            _page_up: PageUpCommand::create(servers)?,
            _verification: VerificationCommand::create(servers)?,
            _buffer_clear: BufferClearCommand::create(servers)?,
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
