use std::{convert::TryFrom, rc::Rc};

use clap::{App, ArgMatches};
use matrix_sdk::ruma::UserId;
use tokio::runtime::Runtime;
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
mod verify;

use buffer_clear::BufferClearCommand;
use devices::DevicesCommand;
use keys::KeysCommand;
use matrix::MatrixCommand;
use page_up::PageUpCommand;
use verification::VerificationCommand;
use verify::VerifyCommand;

pub struct Commands {
    _matrix: Command,
    _keys: Command,
    _devices: Command,
    _page_up: CommandRun,
    _verification: Command,
    _verify: Command,
    _buffer_clear: CommandRun,
}

impl Commands {
    pub fn hook_all(
        servers: &Servers,
        config: &ConfigHandle,
        runtime: Rc<Runtime>,
    ) -> Result<Commands, ()> {
        Ok(Commands {
            _matrix: MatrixCommand::create(servers, config, runtime)?,
            _devices: DevicesCommand::create(servers)?,
            _keys: KeysCommand::create(servers)?,
            _page_up: PageUpCommand::create(servers)?,
            _verification: VerificationCommand::create(servers)?,
            _verify: VerifyCommand::create(servers)?,
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

fn validate_user_id(user_id: String) -> Result<(), String> {
    Box::<UserId>::try_from(user_id)
        .map_err(|_| "The given user isn't a valid user ID".to_owned())
        .map(|_| ())
}
