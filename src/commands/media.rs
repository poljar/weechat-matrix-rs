use std::path::PathBuf;

use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use matrix_sdk::ruma::MxcUri;
use weechat::{buffer::Buffer, Weechat};

use crate::{BufferOwner, Servers};

pub struct MediaCommand;

impl MediaCommand {
    pub const DESCRIPTION: &'static str = "Download Matrix media.";
    pub const COMPLETION: &'static str = "download %(matrix-media)";

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        let (server, output_buffer) = match servers.buffer_owner(buffer) {
            BufferOwner::Room(server, room) => {
                (server, Some(room.buffer_handle()))
            }
            BufferOwner::Server(server) => {
                let output_buffer = server.server_buffer().as_ref().cloned();
                (server, output_buffer)
            }
            BufferOwner::Verification(server, verification) => {
                (server, Some(verification.buffer()))
            }
            BufferOwner::None => {
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
                    .map(|file| PathBuf::from(Weechat::expand_home(file)))
                    .or_else(|| default_download_file(&uri));
                let file = match file {
                    Some(file) => file,
                    None => {
                        Weechat::print(
                            "Could not derive a filename from MXC URI",
                        );
                        return;
                    }
                };

                Weechat::spawn(async move {
                    server
                        .download_media(uri.into(), file, output_buffer)
                        .await;
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
            .arg(Arg::with_name("file").required(false))]
    }

    pub const SETTINGS: &'static [ArgParseSettings] = &[
        ArgParseSettings::DisableHelpFlags,
        ArgParseSettings::DisableVersion,
        ArgParseSettings::VersionlessSubcommands,
        ArgParseSettings::SubcommandRequiredElseHelp,
    ];
}

fn default_download_file(uri: &MxcUri) -> Option<PathBuf> {
    uri.as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_download_file_to_media_id() {
        let uri = Box::<MxcUri>::from("mxc://matrix.org/some-media-id");

        assert_eq!(
            Some(PathBuf::from("some-media-id")),
            default_download_file(&uri)
        );
    }
}
