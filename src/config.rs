//! Where ot is and what this repo means by its branches and CI, all from git
//! config: the GitLab project from origin's URL, and the ot.* settings,
//! which git config already layers (global, then the repo).
//!
//!   git config --add ot.prefix you/        taken off your branch names; repeat for more
//!   git config ot.staleJob stale-check     the CI job that fails an old base
//!   git config ot.staleDays 4              how old that job lets a base get
//!   git config ot.deployJob deploy-prod    the job that deploys to production: your
//!                                          merged MRs stay on the board until it has them
//!   git config ot.slackToken xoxb-…        a Slack token with users:read.email: c's cc
//!                                          input completes from that workspace's people
//!   git config ot.linearKey lin_api_…      a Linear personal API key: your tickets show
//!                                          among your MRs, by project
//!   git config ot.linearSince 2026-11-01   only tickets in projects made since then
use crate::model::Rules;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub host: String,
    pub project: String,
    pub rules: Rules,
    pub slack_token: Option<String>,
    pub linear_key: Option<String>,
    pub linear_since: Option<jiff::civil::Date>,
}

pub fn load() -> Result<Config, String> {
    let out = Command::new("git")
        .args(["config", "--get-regexp", r"^(remote\.origin\.url|ot\.)"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    parse(&String::from_utf8_lossy(&out.stdout))
}

fn parse(config: &str) -> Result<Config, String> {
    let mut origin = None;
    let mut rules = Rules::default();
    let mut slack_token = None;
    let mut linear_key = None;
    let mut linear_since = None;
    for line in config.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        // git lowercases section and key names, so ot.staleJob reads as ot.stalejob.
        match key {
            "remote.origin.url" => origin = Some(value.to_string()),
            "ot.prefix" => rules.prefixes.push(value.to_string()),
            "ot.stalejob" => rules.stale_job = Some(value.to_string()),
            "ot.deployjob" => rules.deploy_job = Some(value.to_string()),
            "ot.slacktoken" => slack_token = Some(value.to_string()),
            "ot.linearkey" => linear_key = Some(value.to_string()),
            "ot.linearsince" => {
                linear_since = Some(
                    value
                        .parse()
                        .map_err(|_| format!("ot.linearSince is not a date like 2026-11-01: {value}"))?,
                )
            }
            "ot.staledays" => {
                rules.stale_days = Some(
                    value
                        .parse()
                        .map_err(|_| format!("ot.staleDays is not a number: {value}"))?,
                )
            }
            _ => {}
        }
    }
    let origin = origin.ok_or("ot runs in a git checkout whose origin is a GitLab project")?;
    let (host, project) = remote(&origin)
        .ok_or_else(|| format!("can't read a GitLab project from origin {origin}"))?;
    Ok(Config {
        host,
        project,
        rules,
        slack_token,
        linear_key,
        linear_since,
    })
}

/// git@host:group/app.git, ssh://git@host:22/group/app.git and
/// https://host/group/app.git all name host and group/app.
fn remote(url: &str) -> Option<(String, String)> {
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let (host, path) = rest.split_once('/')?;
            (host.rsplit('@').next()?.split(':').next()?, path)
        }
        None => {
            let (host, path) = url.split_once(':')?;
            (host.rsplit('@').next()?, path)
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| (host.to_string(), path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_urls_name_the_host_and_project() {
        let want = Some(("git.example".to_string(), "team/sub/app".to_string()));
        assert_eq!(remote("git@git.example:team/sub/app.git"), want);
        assert_eq!(remote("ssh://git@git.example:2222/team/sub/app.git"), want);
        assert_eq!(remote("https://git.example/team/sub/app.git"), want);
        assert_eq!(remote("https://user@git.example/team/sub/app"), want);
        assert_eq!(remote("not a url"), None);
    }

    #[test]
    fn settings_come_from_git_config() {
        let c = parse("remote.origin.url git@git.example:team/app.git\not.prefix sam/\not.prefix old/\not.stalejob stale-check\not.staledays 4\not.deployjob deploy-prod\n").unwrap();
        assert_eq!(c.project, "team/app");
        assert_eq!(
            c.rules,
            Rules {
                prefixes: vec!["sam/".into(), "old/".into()],
                stale_job: Some("stale-check".into()),
                stale_days: Some(4),
                deploy_job: Some("deploy-prod".into()),
            }
        );
        assert!(parse("ot.prefix sam/\n").is_err());
        let origin = "remote.origin.url git@git.example:team/app.git\n";
        let since = |v: &str| parse(&format!("{origin}ot.linearsince {v}\n")).map(|c| c.linear_since);
        assert_eq!(since("2026-11-01"), Ok(Some(jiff::civil::date(2026, 11, 1))));
        assert!(since("last week").is_err());
    }
}
