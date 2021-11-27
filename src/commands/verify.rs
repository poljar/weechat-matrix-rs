use std::convert::TryFrom;

use anyhow::{bail, Result};
use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg, ArgMatches};

use matrix_sdk::ruma::{DeviceIdBox, DeviceKeyAlgorithm, UserId};
use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Weechat,
};

use super::{parse_and_run, validate_user_id};
use crate::{server::MatrixServer, Servers};

pub struct VerifyCommand {
    servers: Servers,
}

impl VerifyCommand {
    pub const DESCRIPTION: &'static str =
        "Control interactive verification flows";

    pub const COMPLETION: &'static str = "%(matrix-users) %(matrix-devices)";

    pub const SETTINGS: &'static [ArgParseSettings] = &[
        ArgParseSettings::DisableHelpFlags,
        ArgParseSettings::DisableVersion,
        ArgParseSettings::VersionlessSubcommands,
    ];

    pub fn create(servers: &Servers) -> Result<Command, ()> {
        let settings = CommandSettings::new("verify")
            .description(Self::DESCRIPTION)
            .add_argument("verify <user> [<device>] [<fingerprint>]")
            .arguments_description(
                "user: the user we wish to verify
                device: the device we wish to verify, this is optional and if \
                not given all devices will receive the verification request \
                but only one will answer
                fingerprint: if given, the user or device will manually be \
                marked as verified",
            )
            .add_completion(Self::COMPLETION);

        Command::new(
            settings,
            VerifyCommand {
                servers: servers.clone(),
            },
        )
    }

    async fn verify(
        server: &MatrixServer,
        user_id: UserId,
        device_id: Option<DeviceIdBox>,
        fingerprint: Option<String>,
    ) -> Result<()> {
        let no_identity = || {
            bail!(
                "User {} doesn't have a valid cross signing identity",
                user_id
            )
        };

        let no_device = |device_id| {
            bail!(
                "The user {} doesn't seem to have a device with the given ID {}",
                user_id, device_id)
        };

        if let Some(c) = server.connection() {
            match (device_id, fingerprint) {
                (None, None) => {
                    if let Some(identity) =
                        c.client().get_user_identity(&user_id).await?
                    {
                        let request = || async move {
                            identity.request_verification().await
                        };

                        server.create_or_update_verification_buffer(
                            c.spawn(request()).await?,
                        );
                    } else {
                        no_identity()?
                    }
                }
                (None, Some(fingerprint)) => {
                    if let Some(identity) =
                        c.client().get_user_identity(&user_id).await?
                    {
                        if Some(fingerprint.as_str())
                            == identity.master_key().get_first_key()
                        {
                            let request =
                                || async move { identity.verify().await };

                            c.spawn(request()).await?;
                        } else {
                            bail!("The given master key fingerprint doesn't match, expected {:?}, got {}",
                                  identity.master_key().get_first_key(), fingerprint)
                        }
                    } else {
                        no_identity()?
                    }
                }
                (Some(device_id), None) => {
                    if let Some(device) =
                        c.client().get_device(&user_id, &device_id).await?
                    {
                        let request = || async move {
                            device.request_verification().await
                        };

                        server.create_or_update_verification_buffer(
                            c.spawn(request()).await?,
                        )
                    } else {
                        no_device(device_id)?
                    }
                }
                (Some(device_id), Some(fingerprint)) => {
                    if let Some(device) =
                        c.client().get_device(&user_id, &device_id).await?
                    {
                        if device.get_key(DeviceKeyAlgorithm::Ed25519)
                            == Some(&fingerprint)
                        {
                            let verify =
                                || async move { device.verify().await };

                            c.spawn(verify()).await?;
                        } else {
                            bail!("The given device fingerprint doesn't match, expected {:?}, got {}",
                                  device.get_key(DeviceKeyAlgorithm::Ed25519), fingerprint)
                        }
                    } else {
                        no_device(device_id)?
                    }
                }
            }
        } else {
            bail!("You need to be connected for the verification to proceed")
        };

        Ok(())
    }

    pub fn run(buffer: &Buffer, servers: &Servers, args: &ArgMatches) {
        if let Some(server) = servers.find_server(buffer) {
            let user_id = args
                .value_of_lossy("user-id")
                .map(|u| {
                    UserId::try_from(u.as_ref())
                        .expect("Argument wasn't a valid user id")
                })
                .expect("Verify command didn't contain a user id");

            let device_id = args
                .value_of_lossy("device-id")
                .map(|a| DeviceIdBox::from(a.as_ref()));

            let fingerprint =
                args.value_of_lossy("fingerprint").map(|f| f.to_string());

            let verify = || async move {
                if let Err(e) =
                    Self::verify(&server, user_id, device_id, fingerprint).await
                {
                    server
                        .print_error(&format!("Error while verifying: {:?}", e))
                }
            };

            Weechat::spawn(verify()).detach();
        } else {
            todo!()
        }
    }

    pub fn args() -> [Arg<'static, 'static>; 3] {
        [
            Arg::with_name("user-id")
                .required(true)
                .validator(validate_user_id),
            Arg::with_name("device-id").required(false),
            Arg::with_name("fingerprint").required(false),
        ]
    }
}

impl CommandCallback for VerifyCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        let argparse = Argparse::new("verify")
            .about(Self::DESCRIPTION)
            .settings(Self::SETTINGS)
            .args(&Self::args());

        parse_and_run(argparse, arguments, |matches| {
            Self::run(buffer, &self.servers, &matches)
        });
    }
}
