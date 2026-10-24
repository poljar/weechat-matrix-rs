[![build-test-release](https://github.com/poljar/weechat-matrix-rs/actions/workflows/release.yml/badge.svg?event=push)](https://github.com/poljar/weechat-matrix-rs/actions/workflows/release.yml)
[![#weechat-matrix](https://img.shields.io/badge/matrix-%23weechat--matrix:termina.org.uk-blue.svg?style=flat-square)](https://matrix.to/#/!twcBhHVdZlQWuuxBhN:termina.org.uk?via=termina.org.uk&via=matrix.org)
[![license](https://img.shields.io/badge/license-ISC-blue.svg?style=flat-square)](https://github.com/poljar/weechat-matrix-rs/blob/master/LICENSE)

# What is weechat-matrix?

[Weechat](https://weechat.org/) is an extensible chat client.

[Matrix](https://matrix.org/blog/home) is an open network for secure,
decentralized communication.

weechat-matrix-rs is a Rust plugin for Weechat that lets Weechat communicate
over the Matrix protocol. This is a Rust rewrite of the
[weechat-matrix](https://github.com/poljar/weechat-matrix) Python script.

# Project status

weechat-matrix-rs connects to one or more Matrix homeservers and maps rooms,
direct messages, Spaces, and threads to WeeChat buffers. It supports:

- password and SSO login, with session restore from a saved access token;
- sending text (Markdown by default), emotes, replies, edits, redactions, and
  file uploads, with local echo;
- thread buffers, thread listing, and paging through older room history;
- typing notices, read markers and read receipts, and a smart filter for
  membership noise;
- end-to-end encryption, device listing and deletion, device trust,
  interactive (SAS) verification, and room-key import and export;
- room management: join, leave, invite, kick, ban, unban, topic, join rule,
  member list, `/whois`, and display-name changes;
- media download through the logged-in client, including authenticated media;
- room metadata exposed as WeeChat local variables for scripts and relay
  front-ends.

It is not an IRC relay or bridge: IRC clients connected through WeeChat's relay
plugin cannot use Matrix rooms as IRC channels. Remote WeeChat interfaces using
WeeChat relay protocols can access plugin buffers, but Matrix features that
rewrite already-printed lines, such as message edits and redactions, depend on
the remote client supporting WeeChat relay line-update events.

If you are interested in helping out take a look at the issue tracker.

# Build

Install Rust and the native build dependencies first. On Debian or Ubuntu a
typical build environment needs:

    sudo apt install \
      build-essential \
      clang \
      libclang-dev \
      libsqlite3-dev \
      pkg-config \
      weechat-dev

Other distributions use different package names, but the important native
pieces are a C/C++ compiler, `clang`/`libclang`, and SQLite development headers.
The `weechat-sys` dependency has a bundled WeeChat plugin header
(`weechat-plugin.h`) fallback. If you want to build against the exact WeeChat
API installed on your system, install the matching development package
(`weechat-dev` on Debian/Ubuntu) or point `WEECHAT_PLUGIN_FILE` at the full
path to that header:

    export WEECHAT_PLUGIN_FILE=/usr/include/weechat/weechat-plugin.h

Use the same WeeChat API headers as the WeeChat binary that will load the
plugin. If WeeChat reports an API mismatch while loading `matrix.so`, rebuild
after installing the matching development package or setting
`WEECHAT_PLUGIN_FILE`. Set `WEECHAT_BUNDLED=1` only when you deliberately want
to build against the bundled fallback header, for example in a controlled test
or package build.

If `bindgen` reports that `stddef.h`, `libclang`, or `libLLVM` is missing, check
that the `clang` command-line tools, C/C++ compiler headers, and matching
`libclang` package are installed. On minimal distributions the library package
alone may not provide the compiler include paths that `bindgen` needs.
Use a coherent `clang`/`libclang`/`libLLVM` set from the same toolchain; copying
only one shared library from another package or SDK can leave `bindgen` loading
an incompatible LLVM library.

After the dependencies are installed the plugin can be compiled with:

    cargo build --release

You do not need to clean the `target/` directory before each build. Reusing it
lets Cargo keep compiled dependencies and makes rebuilds faster. Use
`cargo clean` or `make clean` only when you deliberately want to discard build
artifacts.

If you are developing on weechat-matrix-rs, use debug builds which are faster at
the expense of plugin performance:

    cargo build

On Linux this creates a `libmatrix.so` file in the `target/release/`
(`target/debug` for dev builds) folder. Rename it to `matrix.so` and copy it to
your WeeChat plugin directory.

`make install` installs the plugin systemwide:

    sudo make install

On Debian-like multiarch systems this installs the release build to:

    /usr/lib/${DEB_HOST_MULTIARCH}/weechat/plugins/matrix.so

On other systems it defaults to:

    /usr/lib/weechat/plugins/matrix.so

Set `WEECHAT_PLUGIN_DIR` if your distribution uses another system plugin
directory:

    sudo make install WEECHAT_PLUGIN_DIR=/path/to/weechat/plugins

To install only for the current user, use:

    make install-user

This installs the release build to:

    ${XDG_DATA_HOME:-$HOME/.local/share}/weechat/plugins/matrix.so

Use `PROFILE=debug` for a debug build:

    make install-user PROFILE=debug

The matching uninstall targets are:

    sudo make uninstall
    make uninstall-user

On older WeeChat setups the plugin directory may instead live under
`$WEECHAT_HOME/plugins`, commonly `~/.weechat/plugins/`. Create the directory if
needed and copy the renamed plugin there.

On macOS the built library extension is usually `.dylib`. WeeChat may need this
extension enabled before it loads the plugin:

    /set weechat.plugin.extension ".so,.dll,.dylib"

Rename `target/release/libmatrix.dylib` to `matrix.dylib` before copying it to
your WeeChat plugin directory.

# Debian / Ubuntu package

A `.deb` package can be built using `cargo-deb`. This requires
[Rust installed](https://rustup.rs/) (via rustup, the recommended method).

First ensure `cargo-deb` is available:

    cargo install cargo-deb

Then build the package with:

    make deb

The resulting `.deb` package is placed in `target/debian/`. Install it with:

    sudo dpkg -i target/debian/weechat-matrix-rs_*.deb

# Loading the plugin

Restart WeeChat after installing the plugin, or load it manually:

    /plugin load matrix

Check that it is loaded with:

    /plugin list

If WeeChat cannot find the plugin, check that the file is named `matrix.so`
(`matrix.dylib` on macOS), that it is in a directory from WeeChat's plugin
search path, and that `.so`/`.dylib` is enabled in
`weechat.plugin.extension`.

# Configuration

Configuration is completed primarily through the Weechat interface. First start
WeeChat, make sure the plugin is loaded, and then issue the following commands
_(replace the placeholders in brackets [] with your own details)_:

1. Add a server _(make sure the url includes the scheme e.g. 'https://matrix.org')_:

       /matrix server add [server-name] [server-url]

2. Set your username and password:

       /set matrix-rust.server.[server-name].username [username]
       /set matrix-rust.server.[server-name].password [password]

   If the homeserver advertises single sign-on, leave the password empty to use
   it. WeeChat will print a login URL when you connect.

   The SSO callback listens on `127.0.0.1:29325` on the WeeChat host. When
   WeeChat runs on another machine, create an SSH tunnel before connecting so
   the browser's localhost callback reaches it:

       ssh -L 29325:127.0.0.1:29325 user@weechat-host

   If the callback cannot reach WeeChat, copy the `loginToken` from the
   redirect URL instead and finish the login by hand:

       /matrix sso-complete [server-name] [login-token]

3. Now try to connect. The first connection can take a few minutes while the
   client syncs the account:

       /matrix connect [server-name]

4. Automatically connect to the server:

       /set matrix-rust.server.[server-name].autoconnect on

5. If everything works, save the configuration:

       /save

After a password or SSO login the device id is saved under the server's
storage directory and reused on later logins. If `access_token` is set (which
also requires `device_id`), the plugin restores that session instead of logging
in. When the homeserver refreshes the session tokens, the plugin stores them
with `/secure set` and points `access_token` and `refresh_token` at the
`${sec.data.*}` entries.

## Server options

Each server has its own `matrix-rust.server.[server-name].*` options:

| Option          | Description                                                   |
|-----------------|---------------------------------------------------------------|
| `autoconnect`   | Connect to this server when the plugin loads.                 |
| `homeserver`    | Homeserver URL, including the scheme.                         |
| `proxy`         | Proxy URL used for the connection.                            |
| `username`      | Matrix username.                                              |
| `password`      | Password. Leave empty to use SSO.                             |
| `access_token`  | Access token of a saved session.                              |
| `refresh_token` | Refresh token of a saved session.                             |
| `device_id`     | Device id of a saved session.                                 |
| `ssl_verify`    | Verify the server's TLS certificate (default `on`).           |

`homeserver`, `username`, `password`, and the token and device id options are
evaluated, so they may reference `${sec.data.*}` entries.

## Plugin options

| Option                                      | Default            | Description |
|---------------------------------------------|--------------------|-------------|
| `look.encrypted_room_sign`                  | `🔒`               | Shown in `buffer_modes` for encrypted rooms. |
| `look.encryption_warning_sign`              | `❗`               | Shown for encrypted rooms that contain unverified devices. |
| `look.public_room_sign`                     | `🌍`               | Shown for public rooms. |
| `look.busy_sign`                            | `⏳`               | Shown while the client is busy, e.g. fetching history. |
| `look.local_echo`                           | `on`               | Print a sent message before the server confirms it. |
| `look.redaction_style`                      | `strike-through`   | How redacted messages are shown: `strike-through`, `delete`, or `notice`. |
| `look.smart_filter_delay`                   | `300000`           | Inactivity, in milliseconds, after which a member's membership events get the `matrix_smart_filter` tag (0 to 86400000). |
| `look.reply_full_quote_threshold`           | `0`                | Render replies to recently printed events on one line when positive; `0` keeps the Matrix reply fallback quote. |
| `look.nick_format`                          | `%d`               | Member nick format: `%d` display name, `%m` full MXID, `%u` MXID localpart, `%h` homeserver. |
| `look.server_buffer`                        | `merge-with-core`  | Server buffer placement: `merge-with-core`, `merge-without-core`, or `independent`. |
| `color.nick_prefix_admin`                   | `lightgreen`       | Color of the admin nick prefix (`&`). |
| `color.nick_prefix_moderator`               | `lightmagenta`     | Color of the moderator nick prefix (`@`). |
| `color.nick_prefix_power`                   | `yellow`           | Color of the `+` prefix for non-default power levels below moderator. |
| `network.debug_buffer`                      | `off`              | Write debug logs to a separate buffer. |
| `network.send_read_receipts`                | `on`               | Send public read receipts when marking a room as read. |
| `input.markdown_input`                      | `on`               | Parse the input line as Markdown. |
| `media.download_prefix`                     | `matrix-media-`    | Prefix for default media download filenames; may include a directory and supports `~`, `$XDG_STATE_HOME`, and `${XDG_STATE_HOME}`. |

All of these live under `matrix-rust.`, for example:

       /set matrix-rust.input.markdown_input off

To hide join/leave noise from users who have not been active recently, add a WeeChat
filter for Matrix smart-filter tags:

       /filter add matrix_smart * matrix_smart_filter *

The inactivity delay defaults to five minutes and can be changed with:

       /set matrix-rust.look.smart_filter_delay 300000

Recent replies can be rendered on one line in the form `> Alice: message`. By
default, weechat-matrix-rs keeps the Matrix reply fallback quote when one is
available. Set a positive threshold to render replies to recently printed lines
in the compact one-line form:

       /set matrix-rust.look.reply_full_quote_threshold 20

## Completions

The plugin adds the completion items `matrix_servers`, `matrix-users`,
`matrix-room-members`, and `matrix-media`, and adds Matrix buffer short names
and room member nicks to WeeChat's `buffers_names` and `nick` completions.

# Room Status And Verification

The `buffer_modes` bar item shows Matrix room state. Encrypted rooms show
`matrix-rust.look.encrypted_room_sign`; encrypted rooms that still contain
unverified devices also show `matrix-rust.look.encryption_warning_sign`. Public
rooms show `matrix-rust.look.public_room_sign`, and busy rooms show
`matrix-rust.look.busy_sign`. The `matrix_typing_notice` bar item shows who is
typing in the current room.

Start an interactive verification with a contact, or inspect verification
state:

       /matrix verification start [user-id]
       /matrix verification info [user-id]

Interactive verification requests can be accepted, confirmed, or cancelled from
the room or verification buffer:

       /verification accept
       /verification confirm
       /verification cancel

The plugin renders in-room SAS verification requests and shows the emoji or
decimal short authentication string when it is ready. `/verification` and
`/matrix verification` take the same subcommands.

To mark one device as verified without the interactive flow:

       /matrix verify [user-id] [device-id]

Devices can be listed and deleted (`/devices` works the same as
`/matrix devices`):

       /matrix devices list [user-id]
       /matrix devices delete [device-id]

`set-name` is accepted by the parser but not implemented yet.

Room keys can be exported to and imported from a passphrase-protected file
(`/keys` works the same as `/matrix keys`):

       /matrix keys export [file] [passphrase]
       /matrix keys export --room-id [room-id] [file] [passphrase]
       /matrix keys import [file] [passphrase]

After an import the plugin retries decryption of messages that could not be
decrypted earlier in this session.

# Matrix Spaces

Space buffers use a `+` prefix in their short name, while ordinary room buffers
keep the `#` prefix and direct-message buffers stay unprefixed.

Room buffers expose Matrix parent Spaces as WeeChat local variables. Switch to a
room buffer that belongs to a Space and run:

       /buffer listvar

Look for `space`, `space_id`, `spaces`, and `space_ids`. The singular variables
hold the first parent Space name and room id; the plural variables contain all
known parent Spaces as comma-separated lists.

The buflist plugin will not reorganize Matrix rooms under Spaces by itself.
These variables are available to scripts and custom buflist formats. For
example, to prefix Matrix room names with their first parent Space:

       /set buflist.format.name "${if:${space}?${space}/${name}:${name}}"
       /buflist refresh

Remove that prefix again with:

       /unset buflist.format.name

Matrix rooms can belong to more than one Space. WeeChat buffers cannot be shown
as the same room under multiple server-like parents at the same time, so the
plugin exposes all known parent Spaces as metadata instead of moving room
buffers into a fake hierarchy. Matrix Space buffers use a `+` short-name prefix;
ordinary rooms keep the usual room/channel naming rules.

# Storage

The plugin keeps Matrix state and encryption data in
`$WEECHAT_HOME/matrix-rust/<server>/`. It keeps the Matrix SDK event cache in
`$WEECHAT_HOME/matrix-rust/<server>-cache/`.

If the cache grows or causes too much disk I/O, disconnect WeeChat and remove
only the `-cache` directory. Do not remove the main server directory unless you
want to reset stored Matrix state.

# Troubleshooting

If startup or login fails with a Matrix SDK state-store decode error, first make
sure the plugin was built from current sources and try starting WeeChat again so
SDK migrations can run. If the same error still stops login, disconnect WeeChat
and move `$WEECHAT_HOME/matrix-rust/<server>/` aside before reconnecting. This
resets the stored Matrix state for that server, including encryption/session
data, so keep the old directory until the new login is verified.

For crashes, aborts, or a blocked WeeChat UI during startup or `/quit`, collect
a full thread backtrace from another shell while the process is still stopped or
blocked:

       gdb -p $(pidof weechat) -batch -ex 'thread apply all bt'

A short `bt` from the crashing thread is often not enough to tell whether the
Matrix plugin, WeeChat itself, another plugin, or shutdown cleanup owns the
block.

Nick prefix colors for Matrix power levels can be changed with:

       /set matrix-rust.color.nick_prefix_admin lightgreen
       /set matrix-rust.color.nick_prefix_moderator lightmagenta
       /set matrix-rust.color.nick_prefix_power yellow

Existing nicklist entries may need a member update or plugin restart before the new colors or nick format are visible.

Matrix member nicknames default to display names. To include MXID parts in chat,
the nicklist, `/names`, and nick completion, set `matrix-rust.look.nick_format`:

       /set matrix-rust.look.nick_format "%u (%d)"

Supported placeholders are `%d` for display name, `%m` for full MXID, `%u` for
MXID localpart, and `%h` for homeserver.

# Helpful Commands

`/help matrix` will print information about the `/matrix` command.

`/matrix help [command]` will print information for subcommands, such as `/matrix help server`.

`/matrix version` prints the plugin version.

## Servers and rooms

       /matrix server add [server-name] [server-url]
       /matrix server delete [server-name]
       /matrix server list
       /matrix server listfull
       /matrix connect [server-name]...
       /matrix disconnect [server-name]
       /matrix join [room-id-or-alias]

`/join [room-id-or-alias]` works in any Matrix buffer. A local alias such as
`#room` is completed with your homeserver's domain. `/matrix join` also works
outside Matrix buffers when exactly one server is configured.

`/part` leaves the current room. In a thread buffer it closes the thread buffer
instead.

`/query [user-id] [message]` and `/msg [user-id] [message]` open (or create)
the direct-message room with that user, switch to it, and send the optional
message. A bare localpart is completed with the current room's domain, or
your own.

## Messages

Room buffers accept normal message input after `/matrix connect [server-name]`
has completed. Input is sent as Markdown unless `matrix-rust.input.markdown_input`
is off. `/say [message]` and `/me [action]` send a text message or an emote.

       /reply [event-id|index|/pattern/] [message]
       /edit [event-id|index|/pattern/] [message]
       /redact [event-id|index|/pattern/] [reason...]

The target is a Matrix event id, a 1-based index into recent events (`1`, `0`,
or `-1` is the latest; `2` or `-2` the one before), or a case-sensitive
substring in slashes, which selects the most recent unredacted message
containing it. Without a target, the latest event is used. `/edit` also
accepts the shorthand `/pattern/replacement/`.

       /upload [file]

uploads a file as an attachment to the current room or thread.

       /matrix read

marks the current room as read. Switching to a room buffer also marks it as
read. Public read receipts are sent unless
`matrix-rust.network.send_read_receipts` is off.

## History and threads

Scrolling up (`/window page_up`) at the top of a room buffer fetches an older
page of history. `/matrix history` does the same explicitly and reports the
result. `/buffer clear` resets the paging state of a room buffer.

       /matrix thread [event-id]

opens a thread buffer for that event and loads its history. Thread buffers
are named `thread.<suffix>` and expose the root event id as the `thread_root`
local variable. Input in a thread buffer is sent to the thread.

`/matrix threads list [all|participated]` lists the thread roots of the current room, most recent activity first, using the `/_matrix/client/v1/rooms/{roomId}/threads` endpoint. `/matrix threads more` fetches the next page of that listing and `/matrix threads open <n>` opens one of the listed threads in a thread buffer.

## Room management

       /invite [user-id]
       /kick [user-id] [reason]
       /ban [user-id] [reason]
       /unban [user-id] [reason]
       /topic [topic]
       /topic --clear
       /room set_join_rule [public|invite|knock|private]
       /names
       /nick [new-display-name]
       /whois [user]

`/topic` without arguments shows the current topic. `/nick` without arguments
shows your display name. `/whois` accepts a user id, display name, or current
room nick. `/ignore` is not supported in Matrix buffers.

## Media

`/matrix media download <mxc-uri> [file]` downloads Matrix media through the logged-in client, including homeservers that require authenticated media. It refuses to overwrite an existing file. When `[file]` is omitted, the media id is saved with the configured prefix, which defaults to `matrix-media-`:

       /set matrix-rust.media.download_prefix matrix-media-

The prefix may include a directory and supports `~`, `$XDG_STATE_HOME`, and `${XDG_STATE_HOME}`. Parent directories are created automatically:

       /set matrix-rust.media.download_prefix "$XDG_STATE_HOME/weechat-matrix/media/matrix-media-"

## Buffer metadata for scripts and front-ends

Room buffers set WeeChat local variables, among them `server`, `room_id`,
`thread_root` (thread buffers), the Space variables described above,
`matrix_members_v1` (a JSON list of member profiles, used by `/whois`), and
`matrix_mentions` (a JSON list of `display_name`/`user_id` pairs). Room and
thread buffers also set `matrix_upload_v1`.

Two command-run hooks exist for front-ends that drive WeeChat over a relay:
`/matrix-send [payload]` sends a message with explicit `m.mentions`, where the
payload is base64url-encoded JSON of the form
`{"body": "...", "user_ids": ["@user:example.org"]}`, and
`/matrix-upload begin|chunk|commit|cancel` uploads a file in base64url chunks.
`/matrix-upload` prints its usage when called with wrong arguments.
