use std::{ffi::OsString, path::PathBuf};

use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use matrix_sdk::ruma::{events::room::MediaSource, MxcUri, OwnedMxcUri};
use weechat::{buffer::Buffer, Weechat};

use crate::{emxc::parse_emxc, BufferOwner, Servers};

pub struct MediaCommand;

impl MediaCommand {
    pub const DESCRIPTION: &'static str = "Download Matrix media.";
    pub const COMPLETION: &'static str =
        "download %(matrix-media) || publish %(matrix-media)";

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
            (subcommand @ ("download" | "publish"), Some(args)) => {
                let publish = subcommand == "publish";
                let uri = args
                    .value_of("mxc-uri")
                    .expect("MXC URI not set but was required");
                let (source, uri) = if uri.starts_with("emxc://") {
                    match parse_emxc(uri) {
                        Ok(file) => {
                            let uri = file.url.clone();
                            (MediaSource::Encrypted(Box::new(file)), uri)
                        }
                        Err(error) => {
                            Weechat::print(&error);
                            return;
                        }
                    }
                } else {
                    if !<&MxcUri>::from(uri).is_valid() {
                        Weechat::print("Invalid MXC URI");
                        return;
                    }
                    let uri = OwnedMxcUri::from(uri);
                    (MediaSource::Plain(uri.clone()), uri)
                };

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

                let media_id = uri.media_id().ok().map(ToOwned::to_owned);
                Weechat::spawn(async move {
                    if publish && file.exists() {
                        server
                            .publish_downloaded_media(
                                file,
                                output_buffer,
                                media_id,
                            )
                            .await;
                    } else {
                        server
                            .download_media(
                                source,
                                file,
                                output_buffer,
                                false,
                                media_id,
                            )
                            .await;
                    }
                })
                .detach();
            }
            _ => unreachable!(),
        }
    }

    pub fn subcommands() -> Vec<Argparse<'static, 'static>> {
        let uri_arg = || {
            Arg::with_name("mxc-uri").required(true).validator(|uri| {
                if uri.starts_with("emxc://") {
                    return parse_emxc(&uri).map(|_| ());
                }

                if <&MxcUri>::from(uri.as_str()).is_valid() {
                    Ok(())
                } else {
                    Err("The given URI is not a valid MXC URI".to_owned())
                }
            })
        };

        vec![
            SubCommand::with_name("download")
                .about(
                    "Download an mxc:// URI, or decrypt and download an \
                     emxc:// link, through the logged-in Matrix client",
                )
                .arg(uri_arg())
                .arg(Arg::with_name("file").required(false)),
            SubCommand::with_name("publish")
                .about(
                    "Download the file if needed and run \
                     media.publish_command on it again, for a new link",
                )
                .arg(uri_arg())
                .arg(Arg::with_name("file").required(false)),
        ]
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
    file.strip_prefix('"')
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

/// Where an automatically downloaded file goes: the media id keeps the name
/// unique, the posted file name keeps it readable.
pub(crate) fn auto_download_path(
    prefix: &str,
    media_id: &str,
    file_name: &str,
) -> PathBuf {
    let file_name = std::path::Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(|name| format!("{media_id}-{name}"))
        .unwrap_or_else(|| media_id.to_owned());

    prefixed_download_path(prefix, &file_name)
}

/// The file remembering which media ids were published and where:
/// `<prefix>links.tsv`, one `media id<TAB>unix time<TAB>link` per line.
fn published_links_path(prefix: &str) -> PathBuf {
    prefixed_download_path(prefix, "links.tsv")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The link `media_id` was published at within the last `max_age_days`
/// days (0: ever), if any.
pub(crate) fn published_link(
    prefix: &str,
    media_id: &str,
    max_age_days: u64,
) -> Option<String> {
    let links = std::fs::read_to_string(published_links_path(prefix)).ok()?;
    let oldest = if max_age_days == 0 {
        0
    } else {
        unix_now().saturating_sub(max_age_days * 24 * 60 * 60)
    };

    links
        .lines()
        .filter_map(|line| {
            let (id, rest) = line.split_once('\t')?;
            let (time, link) = rest.split_once('\t')?;
            (id == media_id && time.parse::<u64>().ok()? >= oldest)
                .then(|| link.trim().to_owned())
        })
        .next_back()
}

/// Remember that `media_id` was published at `link` just now.
pub(crate) fn remember_published_link(
    prefix: &str,
    media_id: &str,
    link: &str,
) -> std::io::Result<()> {
    use std::io::Write;

    let path = published_links_path(prefix);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    writeln!(file, "{media_id}\t{}\t{link}", unix_now())
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
        let uri = <&MxcUri>::from("mxc://matrix.org/some-media-id");

        assert_eq!(
            Some(PathBuf::from("matrix-media-some-media-id")),
            default_download_file(uri, "matrix-media-")
        );
    }

    #[test]
    fn auto_download_names_keep_media_id_and_file_name() {
        assert_eq!(
            PathBuf::from("matrix-media-abc123-report.pdf"),
            auto_download_path("matrix-media-", "abc123", "report.pdf")
        );
        assert_eq!(
            PathBuf::from("matrix-media-abc123-passwd"),
            auto_download_path("matrix-media-", "abc123", "../../etc/passwd")
        );
        assert_eq!(
            PathBuf::from("matrix-media-abc123"),
            auto_download_path("matrix-media-", "abc123", "..")
        );
    }

    #[test]
    fn published_links_are_remembered_per_media_id() {
        let dir = std::env::temp_dir().join(format!(
            "wmrs-links-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let prefix = format!("{}/matrix-media-", dir.display());

        assert_eq!(None, published_link(&prefix, "abc", 14));
        remember_published_link(&prefix, "abc", "https://x/1").unwrap();
        remember_published_link(&prefix, "def", "https://x/2").unwrap();
        assert_eq!(
            Some("https://x/1".to_owned()),
            published_link(&prefix, "abc", 14)
        );
        assert_eq!(
            Some("https://x/2".to_owned()),
            published_link(&prefix, "def", 0)
        );
        assert_eq!(None, published_link(&prefix, "ghi", 14));

        // An entry older than the limit is ignored; 0 means forever.
        std::fs::write(
            published_links_path(&prefix),
            "old\t1000\thttps://x/old\n",
        )
        .unwrap();
        assert_eq!(None, published_link(&prefix, "old", 14));
        assert_eq!(
            Some("https://x/old".to_owned()),
            published_link(&prefix, "old", 0)
        );

        std::fs::remove_dir_all(dir).unwrap();
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
        let uri = <&MxcUri>::from("mxc://matrix.org/some-media-id");

        assert_eq!(
            Some(PathBuf::from("/tmp/matrix/matrix-media-some-media-id")),
            default_download_file(uri, "/tmp/matrix/matrix-media-")
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
