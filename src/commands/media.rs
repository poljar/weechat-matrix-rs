use std::path::PathBuf;

use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use matrix_sdk::ruma::MxcUri;
use weechat::{buffer::Buffer, Weechat};

use crate::Servers;

pub struct MediaCommand;

impl MediaCommand {
    pub const DESCRIPTION: &'static str = "Download Matrix media.";
    pub const COMPLETION: &'static str = "download";

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        let server = match servers.find_server(buffer) {
            Some(server) => server,
            None => {
                Weechat::print("Must be executed on Matrix buffer");
                return;
            }
        };

        match args.subcommand() {
            ("download", Some(args)) => {
                let uri = args
                    .value_of("mxc-uri")
                    .expect("MXC URI not set but was required");
                let uri = Box::<MxcUri>::from(uri);

                if !uri.is_valid() {
                    Weechat::print("Invalid MXC URI");
                    return;
                }

                let file = args
                    .value_of("file")
                    .expect("File not set but was required");
                let file = PathBuf::from(Weechat::expand_home(file));

                Weechat::spawn(async move {
                    server.download_media(uri.into(), file).await;
                })
                .detach();
            }
            _ => unreachable!(),
        }
    }

    pub fn subcommands() -> Vec<Argparse<'static, 'static>> {
        vec![SubCommand::with_name("download")
            .about("Download the MXC URI through the logged-in Matrix client")
            .arg(Arg::with_name("mxc-uri").required(true).validator(|uri| {
                let uri = Box::<MxcUri>::from(uri.as_str());

                if uri.is_valid() {
                    Ok(())
                } else {
                    Err("The given URI is not a valid MXC URI".to_owned())
                }
            }))
            .arg(Arg::with_name("file").required(true))]
    }

    pub const SETTINGS: &'static [ArgParseSettings] = &[
        ArgParseSettings::DisableHelpFlags,
        ArgParseSettings::DisableVersion,
        ArgParseSettings::VersionlessSubcommands,
        ArgParseSettings::SubcommandRequiredElseHelp,
    ];
}
