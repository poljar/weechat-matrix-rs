use clap::{ArgMatches, Command as App};
use weechat::{
    hooks::{Command, CommandRun},
    Args, Weechat,
};

use crate::{config::ConfigHandle, Servers};

mod ban;
mod buffer_clear;
mod create;
mod devices;
mod invite;
mod keys;
mod matrix;
mod page_up;
mod part;
mod room;
mod unban;

use ban::BanCommand;
use buffer_clear::BufferClearCommand;
use create::CreateCommand;
use devices::DevicesCommand;
use invite::InviteCommand;
use keys::KeysCommand;
use matrix::MatrixCommand;
use page_up::PageUpCommand;
use part::PartCommand;
use room::RoomCommand;
use unban::UnbanCommand;

pub struct Commands {
    _ban: Command,
    _buffer_clear: CommandRun,
    _create: CommandRun,
    _devices: Command,
    _invite: Command,
    _keys: Command,
    _matrix: Command,
    _page_up: CommandRun,
    _part: CommandRun,
    _room: Command,
    _unban: Command,
}

impl Commands {
    pub fn hook_all(
        servers: &Servers,
        config: &ConfigHandle,
    ) -> Result<Commands, ()> {
        Ok(Commands {
            _ban: BanCommand::create(servers)?,
            _buffer_clear: BufferClearCommand::create(servers)?,
            _create: CreateCommand::create(servers)?,
            _devices: DevicesCommand::create(servers)?,
            _invite: InviteCommand::create(servers)?,
            _keys: KeysCommand::create(servers)?,
            _matrix: MatrixCommand::create(servers, config)?,
            _page_up: PageUpCommand::create(servers)?,
            _part: PartCommand::create(servers)?,
            _room: RoomCommand::create(servers)?,
            _unban: UnbanCommand::create(servers)?,
        })
    }
}

fn parse_and_run(
    parser: App,
    arguments: Args,
    command: impl FnOnce(&ArgMatches),
) {
    match parser.try_get_matches_from(arguments) {
        Ok(m) => command(&m),
        Err(e) => {
            Weechat::print(&format!("{e}"));
        }
    }
}
