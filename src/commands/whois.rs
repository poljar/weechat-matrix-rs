use clap::{App as Argparse, AppSettings as ArgParseSettings, Arg};
use serde::Deserialize;

use weechat::{
    buffer::Buffer,
    hooks::{Command, CommandCallback, CommandSettings},
    Args, Prefix, Weechat,
};

use super::parse_and_run;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct MatrixMemberProfile {
    pub(crate) user_id: String,
    pub(crate) display_name: String,
    pub(crate) nick: String,
    pub(crate) membership: String,
    pub(crate) role: String,
    pub(crate) power_level: Option<i64>,
    pub(crate) avatar_mxc: Option<String>,
}

pub struct WhoisCommand;

impl WhoisCommand {
    pub const DESCRIPTION: &'static str =
        "Show Matrix room member identity information.";

    pub fn create() -> Result<Command, ()> {
        let settings = CommandSettings::new("whois")
            .description(Self::DESCRIPTION)
            .add_argument("<user>")
            .arguments_description(
                "user: Matrix user ID, display name, or current room nick.",
            )
            .add_completion("%(matrix-room-members)");

        Command::new(settings, WhoisCommand)
    }

    fn parser() -> Argparse<'static, 'static> {
        Argparse::new("whois")
            .settings(&[
                ArgParseSettings::DisableHelpFlags,
                ArgParseSettings::DisableVersion,
            ])
            .arg(Arg::with_name("user").required(true))
    }

    fn print_whois(buffer: &Buffer, query: &str) {
        match whois_reply(&matrix_member_profiles(buffer), query) {
            Ok(reply) => buffer.print(&reply),
            Err(error) => buffer.print(&format!(
                "{}matrix: {}",
                Weechat::prefix(Prefix::Error),
                error,
            )),
        }
    }
}

impl CommandCallback for WhoisCommand {
    fn callback(&mut self, _: &Weechat, buffer: &Buffer, arguments: Args) {
        parse_and_run(Self::parser(), arguments, |args| {
            let query = args
                .value_of("user")
                .expect("required whois user argument missing");
            Self::print_whois(buffer, query);
        });
    }
}

pub(crate) fn matrix_member_profiles(
    buffer: &Buffer,
) -> Vec<MatrixMemberProfile> {
    buffer
        .get_localvar("matrix_members_v1")
        .and_then(|json| serde_json::from_str(json.as_ref()).ok())
        .unwrap_or_default()
}

pub(crate) fn matrix_member_completion_candidates(
    profiles: &[MatrixMemberProfile],
) -> Vec<String> {
    let mut candidates = profiles
        .iter()
        .flat_map(|profile| {
            [
                profile.nick.as_str(),
                profile.display_name.as_str(),
                profile.user_id.as_str(),
            ]
        })
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| candidate.to_lowercase());
    candidates.dedup();
    candidates
}

fn whois_reply(
    profiles: &[MatrixMemberProfile],
    query: &str,
) -> Result<String, String> {
    let query = query.trim();

    if query.is_empty() {
        return Err("Usage: /whois <user>".to_owned());
    }

    if profiles.is_empty() {
        return Err("No Matrix room members are known yet.".to_owned());
    }

    let matches = matching_profiles(profiles, query);

    match matches.as_slice() {
        [] => Err(format!("No Matrix room member matches '{}'.", query)),
        [profile] => Ok(format_profile(profile)),
        matches => {
            let names = matches
                .iter()
                .take(8)
                .map(|profile| {
                    format!("{} ({})", profile.nick, profile.user_id)
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!("'{}' is ambiguous: {}.", query, names))
        }
    }
}

fn matching_profiles<'a>(
    profiles: &'a [MatrixMemberProfile],
    query: &str,
) -> Vec<&'a MatrixMemberProfile> {
    let query_folded = query.to_lowercase();

    let mut matches = profiles
        .iter()
        .filter(|profile| {
            [profile.user_id.as_str(), profile.nick.as_str()]
                .iter()
                .any(|candidate| *candidate == query)
                || profile.display_name.eq_ignore_ascii_case(query)
        })
        .collect::<Vec<_>>();

    if matches.is_empty() {
        matches = profiles
            .iter()
            .filter(|profile| {
                [
                    profile.user_id.as_str(),
                    profile.nick.as_str(),
                    profile.display_name.as_str(),
                ]
                .iter()
                .any(|candidate| candidate.to_lowercase() == query_folded)
            })
            .collect();
    }

    matches
}

fn format_profile(profile: &MatrixMemberProfile) -> String {
    let power_level = profile
        .power_level
        .map(|level| level.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let avatar = profile.avatar_mxc.as_deref().unwrap_or("none");

    format!(
        "Matrix user: {nick}\n  user_id: {user_id}\n  display name: {display_name}\n  membership: {membership}\n  role: {role}\n  power level: {power_level}\n  avatar: {avatar}",
        nick = profile.nick,
        user_id = profile.user_id,
        display_name = profile.display_name,
        membership = profile.membership,
        role = profile.role,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        matrix_member_completion_candidates, whois_reply, MatrixMemberProfile,
    };

    fn profile(
        user_id: &str,
        display_name: &str,
        nick: &str,
    ) -> MatrixMemberProfile {
        MatrixMemberProfile {
            user_id: user_id.to_owned(),
            display_name: display_name.to_owned(),
            nick: nick.to_owned(),
            membership: "join".to_owned(),
            role: "member".to_owned(),
            power_level: Some(0),
            avatar_mxc: Some("mxc://example.org/avatar".to_owned()),
        }
    }

    #[test]
    fn whois_finds_member_by_mxid() {
        let profiles = vec![profile("@ada:example.org", "Ada", "Ada")];

        let reply = whois_reply(&profiles, "@ada:example.org").unwrap();

        assert!(reply.contains("user_id: @ada:example.org"));
        assert!(reply.contains("display name: Ada"));
        assert!(reply.contains("membership: join"));
    }

    #[test]
    fn whois_finds_member_by_display_name_case_insensitively() {
        let profiles = vec![profile("@ada:example.org", "Ada Lovelace", "Ada")];

        assert!(whois_reply(&profiles, "ada lovelace")
            .unwrap()
            .contains("@ada:example.org"));
    }

    #[test]
    fn whois_reports_ambiguous_display_names() {
        let profiles = vec![
            profile("@one:example.org", "Sam", "Sam"),
            profile("@two:example.org", "Sam", "Sam (@two:example.org)"),
        ];

        let error = whois_reply(&profiles, "Sam").unwrap_err();

        assert!(error.contains("ambiguous"));
        assert!(error.contains("@one:example.org"));
        assert!(error.contains("@two:example.org"));
    }

    #[test]
    fn completion_uses_nicks_display_names_and_mxids() {
        let profiles = vec![profile(
            "@ada:example.org",
            "Ada Lovelace",
            "Ada (@ada:example.org)",
        )];

        assert_eq!(
            matrix_member_completion_candidates(&profiles),
            vec![
                "@ada:example.org".to_owned(),
                "Ada (@ada:example.org)".to_owned(),
                "Ada Lovelace".to_owned(),
            ],
        );
    }
}
