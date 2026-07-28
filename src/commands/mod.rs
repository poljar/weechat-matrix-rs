use clap::{App, ArgMatches};
use verification::VerificationCommand;
use weechat::{
    hooks::{Command, CommandRun},
    Args, Weechat,
};

use crate::{config::ConfigHandle, Servers};

mod buffer_clear;
mod buffer_switch;
mod devices;
mod dm;
mod ignore;
mod invite;
mod join;
mod keys;
mod matrix;
mod me;
mod media;
mod moderation;
mod names;
mod nick;
mod page_up;
mod part;
mod redact;
mod reply;
mod topic;
mod upload;
mod verification;

use buffer_clear::BufferClearCommand;
use buffer_switch::BufferSwitchCommand;

pub(crate) use buffer_switch::is_buffer_target;
use devices::DevicesCommand;
use dm::DirectMessageCommand;
use ignore::IgnoreCommand;
use invite::InviteCommand;
use join::JoinCommand;
use keys::KeysCommand;
use matrix::MatrixCommand;
use me::MeCommand;
use media::MediaCommand;
use moderation::ModerationCommand;
use names::NamesCommand;
use nick::NickCommand;
use page_up::PageUpCommand;
use part::PartCommand;
use redact::RedactCommand;
use reply::ReplyCommand;
use topic::TopicCommand;
use upload::UploadCommand;

pub struct Commands {
    _matrix: Command,
    _keys: Command,
    _devices: Command,
    _invite: Command,
    _ignore: [CommandRun; 2],
    _ban: Command,
    _kick: Command,
    _page_up: CommandRun,
    _redact: Command,
    _reply: Command,
    _topic: Command,
    _verification: Command,
    _buffer_clear: CommandRun,
    _buffer_switch: CommandRun,
    _join: CommandRun,
    _me: CommandRun,
    _upload: CommandRun,
    _part: CommandRun,
    _query: CommandRun,
    _msg: CommandRun,
    _names: CommandRun,
    _unban: Command,
    _nick: CommandRun,
}

impl Commands {
    pub fn hook_all(
        servers: &Servers,
        config: &ConfigHandle,
    ) -> Result<Commands, ()> {
        Ok(Commands {
            _matrix: MatrixCommand::create(servers, config)?,
            _devices: DevicesCommand::create(servers)?,
            _invite: InviteCommand::create(servers)?,
            _ignore: IgnoreCommand::create()?,
            _ban: ModerationCommand::ban(servers)?,
            _kick: ModerationCommand::kick(servers)?,
            _keys: KeysCommand::create(servers)?,
            _page_up: PageUpCommand::create(servers)?,
            _redact: RedactCommand::create(servers)?,
            _reply: ReplyCommand::create(servers)?,
            _topic: TopicCommand::create(servers)?,
            _verification: VerificationCommand::create(servers)?,
            _buffer_clear: BufferClearCommand::create(servers)?,
            _buffer_switch: BufferSwitchCommand::create(servers)?,
            _join: JoinCommand::create(servers)?,
            _me: MeCommand::create(servers)?,
            _upload: UploadCommand::create(servers)?,
            _part: PartCommand::create(servers)?,
            _query: DirectMessageCommand::query(servers)?,
            _msg: DirectMessageCommand::msg(servers)?,
            _names: NamesCommand::create(servers)?,
            _unban: ModerationCommand::unban(servers)?,
            _nick: NickCommand::create(servers)?,
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
