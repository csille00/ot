# ot

Your GitLab merge requests as a board in the terminal, and a review of one in nvim that you read by the functions and types it changed instead of file by file.

The board groups your MRs into stacks and says the one thing each needs next: rebase it, ask for review, answer threads, merge it. It keeps itself current without you refreshing. It can rebase a whole stack here and land it one MR at a time, and it asks before it merges or pushes anything of yours. If you use Claude Code, your sessions show on the MR they're working on.

## Install

```
cargo install --git <this repo's url>
```

Run `ot` in a checkout whose `origin` is a GitLab project. It uses `GITLAB_TOKEN` if set, or else the token `glab auth login` saved. Press `?` anywhere for the keys.

## What it uses

- git, and nvim for reviews. ot brings its own nvim plugin, and your config and LSP load as usual.
- The `claude` CLI for the parts Claude does: settling rebase conflicts, deciding which MRs in a stack can ship in one deploy, the tour of a change (`T`), and a line on where a stopped agent left things.
- Optionally ffmpeg for video thumbnails in descriptions, herdr's `hc` to show agents running in herdr terminals, and cliamp with yt-dlp for the music screen.

Built and used on macOS. Notifications are macOS only, and Linux is untested.

## Settings

All in git config, so set them globally or per repo.

```
git config --add ot.prefix you/        taken off your branch names; repeat for more
git config ot.staleJob stale-check     the CI job that fails an old base
git config ot.staleDays 4              how old that job lets a base get
git config ot.deployJob deploy-prod    the job that deploys to production; landing waits on it
git config ot.lintCommand 'make lint'  run on what Claude settled in a rebase, with
                                       GIT_BASE_REF and OT_CHANGED_FILES set
git config ot.slackToken xoxb-…        a Slack token with users:read.email, so cc completes emails
git config ot.linearKey lin_api_…      a Linear personal API key, so your tickets show with your MRs
git config ot.linearSince 2026-11-01   only tickets in projects made since then
```

## Also here

`seam-core/` is a copy of the Rust core of [seam](https://github.com/booka66/seam), which reads a diff by the definitions it touches. `zen/` is a small Firefox and Zen extension that switches to the tab already showing a page when a link is opened from outside the browser, so `w` on the board doesn't stack up MR tabs.
