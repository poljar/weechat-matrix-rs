use std::{ffi::OsString, path::PathBuf};

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

                let download_prefix =
                    server.config().borrow().media().download_prefix();

                let file = args
                    .value_of("file")
                    .map(normalize_download_file)
                    .or_else(|| default_download_file(&uri, &download_prefix));
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

fn normalize_download_file(file: &str) -> PathBuf {
    PathBuf::from(Weechat::expand_home(strip_download_file_quotes(file)))
}

fn strip_download_file_quotes(file: &str) -> &str {
    let file = file.trim();
    file
        .strip_prefix('"')
        .and_then(|file| file.strip_suffix('"'))
        .or_else(|| {
            file.strip_prefix('\'')
                .and_then(|file| file.strip_suffix('\''))
        })
        .unwrap_or(file)
}

fn default_download_file(uri: &MxcUri, prefix: &str) -> Option<PathBuf> {
    uri.as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(|name| prefixed_download_path(prefix, name))
}

fn prefixed_download_path(prefix: &str, name: &str) -> PathBuf {
    let prefix = expand_download_prefix(
        prefix,
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    );
    PathBuf::from(format!("{}{}", prefix, name))
}

fn expand_download_prefix(
    prefix: &str,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> String {
    let prefix = expand_home_prefix(prefix, home.clone());

    if prefix.starts_with("${XDG_STATE_HOME}") {
        return format!(
            "{}{}",
            xdg_state_home_path(xdg_state_home, home).display(),
            &prefix["${XDG_STATE_HOME}".len()..]
        );
    }

    if prefix == "$XDG_STATE_HOME" || prefix.starts_with("$XDG_STATE_HOME/") {
        return format!(
            "{}{}",
            xdg_state_home_path(xdg_state_home, home).display(),
            &prefix["$XDG_STATE_HOME".len()..]
        );
    }

    prefix
}

fn expand_home_prefix(prefix: &str, home: Option<OsString>) -> String {
    if prefix == "~" || prefix.starts_with("~/") {
        if let Some(home) = home.filter(|home| !home.is_empty()) {
            return format!(
                "{}{}",
                PathBuf::from(home).display(),
                &prefix["~".len()..]
            );
        }
    }

    prefix.to_owned()
}

fn xdg_state_home_path(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> PathBuf {
    xdg_state_home
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_download_file_to_media_id() {
        let uri = Box::<MxcUri>::from("mxc://matrix.org/some-media-id");

        assert_eq!(
            Some(PathBuf::from("matrix-media-some-media-id")),
            default_download_file(&uri, "matrix-media-")
        );
    }

    #[test]
    fn strips_weechat_preserved_quotes_from_download_file() {
        assert_eq!(
            "/tmp/a path/image.png",
            strip_download_file_quotes("\"/tmp/a path/image.png\"")
        );
        assert_eq!(
            "/tmp/a path/image.png",
            strip_download_file_quotes("'/tmp/a path/image.png'")
        );
    }

    #[test]
    fn default_download_prefix_can_include_directory() {
        let uri = Box::<MxcUri>::from("mxc://matrix.org/some-media-id");

        assert_eq!(
            Some(PathBuf::from("/tmp/matrix/matrix-media-some-media-id")),
            default_download_file(&uri, "/tmp/matrix/matrix-media-")
        );
    }

    #[test]
    fn expands_xdg_state_home_in_download_prefix() {
        assert_eq!(
            "/tmp/state/weechat-matrix/matrix-media-",
            expand_download_prefix(
                "$XDG_STATE_HOME/weechat-matrix/matrix-media-",
                Some("/tmp/state".into()),
                Some("/home/user".into())
            )
        );

        assert_eq!(
            "/tmp/state/weechat-matrix/matrix-media-",
            expand_download_prefix(
                "${XDG_STATE_HOME}/weechat-matrix/matrix-media-",
                Some("/tmp/state".into()),
                Some("/home/user".into())
            )
        );
    }

    #[test]
    fn expands_xdg_state_home_fallback() {
        assert_eq!(
            "/home/user/.local/state/weechat-matrix/matrix-media-",
            expand_download_prefix(
                "$XDG_STATE_HOME/weechat-matrix/matrix-media-",
                None,
                Some("/home/user".into())
            )
        );
    }

    #[test]
    fn expands_home_in_download_prefix() {
        assert_eq!(
            "/home/user/media/matrix-media-",
            expand_download_prefix(
                "~/media/matrix-media-",
                Some("/tmp/state".into()),
                Some("/home/user".into())
            )
        );
    }
}
