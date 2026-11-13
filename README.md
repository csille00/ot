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
- T3 Code, if installed (`~/.t3`, or `T3_BIN`): its threads in this checkout's T3 project show as the board's agents, matched to MRs by the ones they opened. `s` sends a thread a message, enter opens it in T3. It takes herdr's place when both are here. `ctrl-k` (or `/`), on the board or in a conversation, finds any T3 thread, in every project and settled ones too, as T3's own ctrl+k does: titles match fuzzily, and so do the words of their messages. j/k don't move there, since you're typing: `ctrl-j`/`ctrl-k`, `ctrl-n`/`ctrl-p` or the arrows do; enter opens the thread, `ctrl-u` clears, esc closes. Reading a conversation, your T3 threads are listed beside it (from 100 columns; `b` hides it): `tab` moves into the list, `j`/`k` and enter switch, tab or esc come back, and `[`/`]` go to the thread before or after without leaving the conversation. `ctrl-k` also finds your open MRs, by `!number`, title or branch; enter opens the review. A thread with an MR (the one it opened, or the one its branch is) shows that MR beside it from 140 columns: how it stands, then its diff, drawn by [delta](https://github.com/dandavison/delta) when it is installed (else git's colours), which `ctrl-f`/`ctrl-b` scroll; `o` opens its review and `m` hides it. `ctrl-l` focuses the MR (`j`/`k`, `ctrl-d`/`ctrl-u`, `g`/`G` scroll its diff) and `ctrl-h` comes back, then on to the thread list. The MR's merge status (ready, waiting on its pipeline, blocked by open threads, conflicts, needs approval…) heads that panel, ctrl-k's preview and the review's list; a Claude session on an MR's branch gets the panel too. `e` renames a T3 thread (on its row, reading it, or in the list), and `n` starts a new one in this checkout's T3 project, then opens it. `l` on an MR's row reads its title and description over the board (`j`/`k` scroll, `w` web, `y` link, esc closes); `i` opens an MR's Linear ticket. `s` replies in your own nvim, over ot in a floating zellij pane inside zellij, else over the conversation: `:w` sends, `:q` keeps the draft for the next `s`. Text pastes as nvim pastes. An image goes in from the Mac: copy it and run the Raycast script `send-image-to-box.sh` (nixos-config's dotfiles/raycast), which puts it in `~/.cache/ot/paste`; the editor attaches what lands there, and `:w` sends it with the message. `v`, reading a session or thread, opens all of it in your nvim, read-only and at its end: visual mode, search, and yanks (over ssh through OSC 52 to your clipboard), in a floating zellij pane inside zellij. `t`, reading a session or thread, opens a shell where it works (a thread's own worktree, else its project's folder): a new zellij pane inside zellij, else this terminal until you exit. In nvim's review, `}` and `{` (`<leader>m}`/`<leader>m{` in the code) go to the change's next and previous comment.
- Images a conversation links by path draw inline: sharp through kitty's protocol in Ghostty or kitty, in half blocks inside zellij (which drops kitty's escapes) or elsewhere. Videos show as a line.
- Over ssh or in zellij, `y` copies through OSC 52, `w` copies the link, and notifications go out as OSC 777, so they reach the terminal you look at.
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
