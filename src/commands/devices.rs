use std::{cmp::Reverse, convert::TryFrom};

use chrono::{DateTime, Utc};
use clap::{
    App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches,
    SubCommand,
};
use matrix_sdk::{
    ruma::{
        DeviceId, DeviceKeyAlgorithm,
        MilliSecondsSinceUnixEpoch, UserId,
    },
    Error,
};

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;
use crate::{
    commands::validate_user_id, connection::Connection, server::MatrixServer,
    Servers,
};

pub struct DevicesCommand {
    servers: Servers,
}

#[derive(Debug, Clone, Copy)]
enum DeviceTrust {
    Verified,
    Unverified,
    Unsupported,
}

impl DevicesCommand {
    pub const DESCRIPTION: &'static str =
        "List, delete or rename Matrix devices";

    pub const SETTINGS: &'static [ArgParseSettings] = &[
        ArgParseSettings::DisableHelpFlags,
        ArgParseSettings::DisableVersion,
        ArgParseSettings::VersionlessSubcommands,
        ArgParseSettings::SubcommandRequiredElseHelp,
    ];

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("devices")
            .description(Self::DESCRIPTION)
            .add_argument("list")
            .add_argument("delete <device-id>")
            .add_argument("set-name <device-id> <name>")
            .arguments_description(
                "device-id: The unique id of the device that should be deleted.
     name: The name that the device name should be set to.",
            )
            .add_completion("list %(matrix-users)")
            .add_completion("delete %(matrix-own-devices)")
            .add_completion("set-name %(matrix-own-devices)")
            .add_completion("help list|delete|set-name");

        Command::new(
            settings,
            DevicesCommand {
                servers: servers.clone(),
            },
        )
    }

    fn delete(servers: &Servers, buffer: &Buffer, devices: Vec<Box<DeviceId>>) {
        let server = servers.find_server(buffer);

        if let Some(s) = server {
            let devices = || async move {
                s.delete_devices(devices).await;
            };
            Weechat::spawn(devices()).detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    fn list(servers: &Servers, buffer: &Buffer, user_id: Option<Box<UserId>>) {
        let server = servers.find_server(buffer);

        if let Some(s) = server {
            let devices = || async move {
                s.devices(user_id).await;
            };
            Weechat::spawn(devices()).detach();
        } else {
            Weechat::print("Must be executed on Matrix buffer")
        }
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        match args.subcommand() {
            ("list", args) => {
                let user_id = args.and_then(|a| {
                    a.args.get("user-id").and_then(|a| {
                        a.vals.first().map(|u| {
                            Box::<UserId>::try_from(u.to_string_lossy().as_ref())
                                .expect("Argument wasn't a valid user id")
                        })
                    })
                });

                Self::list(servers, buffer, user_id);
            }
            ("delete", args) => {
                let devices = args
                    .and_then(|a| a.args.get("device-id"))
                    .expect("Args didn't contain any device ids");
                let devices: Vec<Box<DeviceId>> = devices
                    .vals
                    .iter()
                    .map(|d| d.clone().to_string_lossy().as_ref().into())
                    .collect();
                Self::delete(servers, buffer, devices);
            }
            _ => Weechat::print(&format!(
                "{}Subcommand isn't implemented",
                Weechat::prefix(Prefix::Error)
            )),
        }
    }

    pub fn subcommands() -> Vec<Argparse<'static, 'static>> {
        vec![
            SubCommand::with_name("list")
                .arg(
                    Arg::with_name("user-id")
                        .required(false)
                        .validator(validate_user_id),
                )
                .about("List your own Matrix devices on the server."),
            SubCommand::with_name("delete")
                .about("Delete the given device")
                .arg(
                    Arg::with_name("device-id")
                        .require_delimiter(true)
                        .multiple(true)
                        .required(true),
                ),
            SubCommand::with_name("set-name")
                .about("Set the human readable name of the given device")
                .arg(Arg::with_name("device-id").required(true))
                .arg(Arg::with_name("name").required(true)),
        ]
    }
}

impl CommandCallback for DevicesCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("devices")
            .about(Self::DESCRIPTION)
            .settings(Self::SETTINGS)
            .subcommands(Self::subcommands());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, &matches)
        });
    }
}

impl MatrixServer {
    async fn list_own_devices(
        &self,
        connection: Connection,
    ) -> Result<(), Error> {
        let mut response = connection.devices().await?;

        if response.devices.is_empty() {
            self.print_error("No devices were found for this server");
            return Ok(());
        }

        self.print_network(&format!(
            "Devices for server {}{}{}:",
            Weechat::color("chat_server"),
            self.name(),
            Weechat::color("reset")
        ));

        response.devices.sort_by_key(|d| Reverse(d.last_seen_ts));
        let own_device_id = connection.client().device_id().await;
        let own_user_id = connection
            .client()
            .user_id()
            .await
            .expect("Getting our own devices while not being logged in");

        let mut lines: Vec<String> = Vec::new();

        let devices =
            connection.client().get_user_devices(&own_user_id).await?;

        for device in devices.devices() {
            Weechat::print(&format!(
                "Found device {}",
                device.device_id().as_str()
            ));
        }

        for device_info in response.devices {
            let device = connection
                .client()
                .get_device(&own_user_id, &device_info.device_id)
                .await?;

            let own_device =
                own_device_id.as_ref() == Some(&device_info.device_id);

            let device_trust = device
                .as_ref()
                .map(|d| {
                    if d.verified() {
                        DeviceTrust::Verified
                    } else {
                        DeviceTrust::Unverified
                    }
                })
                .unwrap_or(DeviceTrust::Unsupported);

            let info = Self::format_device(
                &device_info.device_id,
                device
                    .and_then(|d| {
                        d.get_key(DeviceKeyAlgorithm::Ed25519)
                            .map(|f| f.to_string())
                    })
                    .as_deref(),
                device_info.display_name.as_deref(),
                own_device,
                device_trust,
                device_info.last_seen_ip,
                device_info.last_seen_ts,
            );

            lines.push(info);
        }

        let line = lines.join("\n");
        self.print(&line);

        Ok(())
    }

    async fn list_other_devices(
        &self,
        connection: Connection,
        user_id: &UserId,
    ) -> Result<(), Error> {
        let devices = connection.client().get_user_devices(&user_id).await?;

        let lines: Vec<_> = devices
            .devices()
            .map(|device| {
                let device_trust = if device.verified() {
                    DeviceTrust::Verified
                } else {
                    DeviceTrust::Unverified
                };

                Self::format_device(
                    device.device_id(),
                    device
                        .get_key(DeviceKeyAlgorithm::Ed25519)
                        .map(|f| f.as_str()),
                    device.display_name().as_deref(),
                    false,
                    device_trust,
                    None,
                    None,
                )
            })
            .collect();

        let user_color = Weechat::info_get("nick_color_name", user_id.as_str())
            .expect("Can't get user color");

        if lines.is_empty() {
            self.print_error(&format!(
                "No devices were found for user {}{}{} on this server",
                Weechat::color(&user_color),
                user_id.as_str(),
                Weechat::color("reset"),
            ));
        } else {
            self.print_network(&format!(
                "Devices for user {}{}{} on server {}{}{}:",
                Weechat::color(&user_color),
                user_id.as_str(),
                Weechat::color("reset"),
                Weechat::color("chat_server"),
                self.name(),
                Weechat::color("reset")
            ));

            let line = lines.join("\n");
            self.print(&line);
        }

        Ok(())
    }

    fn format_device(
        device_id: &DeviceId,
        fingerprint: Option<&str>,
        display_name: Option<&str>,
        is_own_device: bool,
        device_trust: DeviceTrust,
        last_seen_ip: Option<String>,
        last_seen_ts: Option<MilliSecondsSinceUnixEpoch>,
    ) -> String {
        let device_color =
            Weechat::info_get("nick_color_name", device_id.as_str())
                .expect("Can't get device color");

        let last_seen_date = last_seen_ts
            .and_then(|d| {
                d.to_system_time().map(|d| {
                    let date: DateTime<Utc> = d.into();
                    date.format("%Y/%m/%d %H:%M").to_string()
                })
            })
            .unwrap_or_else(|| "?".to_string());

        let last_seen = format!(
            "{} @ {}",
            last_seen_ip.as_deref().unwrap_or("-"),
            last_seen_date
        );

        let (bold, color) = if is_own_device {
            (Weechat::color("bold"), format!("*{}", device_color))
        } else {
            ("", device_color)
        };

        let verified = match device_trust {
            DeviceTrust::Verified => {
                format!(
                    "{}Trusted{}",
                    Weechat::color("green"),
                    Weechat::color("reset")
                )
            }
            DeviceTrust::Unverified => {
                format!(
                    "{}Not trusted{}",
                    Weechat::color("red"),
                    Weechat::color("reset")
                )
            }
            DeviceTrust::Unsupported => {
                format!(
                    "{}No encryption support{}",
                    Weechat::color("darkgray"),
                    Weechat::color("reset")
                )
            }
        };

        let fingerprint = if let Some(fingerprint) = fingerprint {
            let fingerprint = fingerprint
                .chars()
                .collect::<Vec<char>>()
                .chunks(4)
                .map(|c| c.iter().collect::<String>())
                .collect::<Vec<String>>()
                .join(" ");

            format!(
                "{}{}{}",
                Weechat::color("magenta"),
                fingerprint,
                Weechat::color("reset")
            )
        } else {
            format!(
                "{}-{}",
                Weechat::color("darkgray"),
                Weechat::color("reset")
            )
        };

        format!(
            "       \
                                    Name: {}{}\n  \
                               Device ID: {}{}{}\n   \
                                Security: {}\n\
                             Fingerprint: {}\n  \
                               Last seen: {}\n",
            bold,
            display_name.as_deref().unwrap_or(""),
            Weechat::color(&color),
            device_id.as_str(),
            Weechat::color("reset"),
            verified,
            fingerprint,
            last_seen,
        )
    }

    pub async fn devices(&self, user_id: Option<Box<UserId>>) {
        let connection = if let Some(c) = self.connection() {
            c
        } else {
            self.print_error("You must be connected to execute this command");
            return;
        };

        let ret = if let Some(user_id) = user_id {
            if Some(&user_id) == connection.client().user_id().await.as_ref() {
                self.list_own_devices(connection).await
            } else {
                self.list_other_devices(connection, &user_id).await
            }
        } else {
            self.list_own_devices(connection).await
        };

        if let Err(e) = ret {
            self.print_error(&format!("Error fetching devices {:?}", e));
        }
    }

    pub async fn delete_devices(&self, devices: Vec<Box<DeviceId>>) {
        let formatted = devices
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<String>>()
            .join(", ");

        let print_success = || {
            self.print_network(&format!(
                "Successfully deleted device(s) {}",
                formatted
            ));
        };

        let print_fail = |e| {
            self.print_error(&format!(
                "Error deleting device(s) {} {:#?}",
                formatted, e
            ));
        };

        if let Some(c) = self.connection() {
            match c.delete_devices(devices.clone(), None).await {
                Ok(_) => print_success(),
                Err(e) => {
                    if let Some(info) = e.uiaa_response() {
                        let auth_info = self.auth_info(info);

                        if let Err(e) = c
                            .delete_devices(devices.clone(), Some(auth_info))
                            .await
                        {
                            print_fail(e);
                        } else {
                            print_success();
                        }
                    } else {
                        print_fail(e)
                    }
                }
            }
        };
    }
}
