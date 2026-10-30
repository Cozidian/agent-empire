# AGENT EMPIRE

Run your coding agents like a 4X space empire. Every **folder is a planet**, its parent folder
is a **star system**, and every agent (claude, codex, grok, …) is a **ship in orbit** around
the folder it works in.

```
cargo build --release
./target/release/empire
```

## How it works

```
empire (the galaxy UI)  ──unix socket──▶  empire daemon
                                           owns every agent's terminal (PTY + emulator),
                                           detects who's working / blocked / done
```

The daemon starts automatically the first time you open the galaxy. Agents live in the daemon,
so **quitting the UI doesn't kill them**: reopen `empire` and your empire is still there.
`empire stop` shuts the daemon down and recalls every agent.

Agents also survive the daemon itself:

- **New build?** Opening the galaxy (or `empire upgrade`) swaps the running daemon for the new
  binary *in place*: it `exec`s itself with the same pid, so every agent stays its child, their
  terminals carry over, and their screens are replayed. Nothing restarts.
- **Reboot or crash?** The daemon keeps a list of running agents and their session ids, and on its
  next start brings them back with `claude --resume`, `codex resume` or `grok --resume`.
  `empire stop` clears that list, so a deliberate stop stays stopped.

## Playing

Worlds, systems and ships have seeded procedural appearances: varied planetary palettes,
surface patterns and axial tilts, tilted colony rings, stellar coronas and occasional asteroid
belts, and four ship hull styles. Each folder or ship keeps its look across redraws and reopening
the galaxy. Ship status colours and development indicators still show the same information.

| key              | action                                                              |
| ---------------- | ------------------------------------------------------------------- |
| `n`              | **colonize**: launch an agent in a folder (pick one, or type a new path and it's created) |
| `e`              | **deploy** another agent to the selected world                      |
| `⏎` / click twice | **comms**: a chat popup with the agent's live terminal; `⇥` goes full screen |
| `Ctrl-]` / `F12` | leave comms, back to the galaxy                                     |
| `⇥`              | jump to the next agent that needs you                               |
| `C`              | **commander**: your attention queue across all star systems         |
| `t`              | **mission**: write a task for the selected world (launch a ship with it, or queue it) |
| `b`              | **fleet order**: one order to every agent in the selected system or world |
| `u`              | **moon**: found a git worktree off the selected world, with its own agent |
| `D`              | **dock** the selected moon: merge its branch back in and retire it   |
| `P`              | give the selected agent **priority** in a border conflict            |
| `X`              | forget the selected dormant world                                   |
| `r`              | what the selected world **needs**, as quick actions                |
| `L`              | the selected world's **mission log**: under way, queued, done       |
| `T`              | strike finished missions on the selected world from its log         |
| `S`              | the **situation log** in full: filter it, read an entry's details   |
| `x`              | **decommission** the selected agent (asks first)                    |
| `↑↓` / `j k`     | move through the outliner                                           |
| `wasd` / drag    | pan · `±` / wheel zoom · `z` fit galaxy · `f` focus selection        |
| `o` `m` `?` `q`  | outliner · mute · fleet manual · leave                              |

Comms opens as a **chat popup** over the galaxy: the agent's terminal, fitted to the popup, and a
line to write in underneath. `⏎` sends what you wrote (or, with nothing written, presses Enter for
the agent); when it's waiting on a question, `1`–`9` and `↑↓` answer it. `PgUp/PgDn` or the wheel
scroll back, `Esc` or a click outside closes it, and `⇥` takes you to the full-screen terminal.

In the full-screen terminal every key, including Esc and Ctrl-C, goes straight to the agent. The mouse wheel
(or `Shift-PgUp/PgDn`) scrolls back through the agent's history; agents that handle the mouse
themselves get the wheel instead. Typing snaps back to live.
Drag to select text, double-click for a word, triple-click for a line; letting go copies it
(wl-copy, xclip or pbcopy, else the terminal's own clipboard via OSC 52).

Sounds stay out of your way: one cue at a time, nothing for the agent you're already talking to,
no "done" for turns shorter than a few seconds, and a blocked agent re-pings every 2 minutes.

| status      | in the galaxy                                   | sound                 |
| ----------- | ----------------------------------------------- | --------------------- |
| working     | cyan ship with an orbiting satellite            | —                     |
| **blocked** | flashing red ship, shockwaves, red planet ring  | robot "hm–hm?"        |
| done        | green burst, green planet ring until you look   | "bee-boop"            |
| launching   | blinking amber                                  | data burst on arrival |

## Missions

Every world keeps a quest log. Press `t` on a world and write what should happen there: either a new
ship launches with it as its opening prompt, or it waits in the queue and the next agent there that
is free gets it. A mission is complete when its agent finishes the turn; the world card lists what's
under way (◆), waiting (○) and done (★). Missions survive restarts.

A mission can span several lines: `Shift+⏎` or `Alt+⏎` starts a new one (`Ctrl+J` in terminals
that send the same thing for `Shift+⏎` as for `⏎`), and pasted text keeps its line breaks. The text
wraps and grows to eight lines, then scrolls; `↑↓` move between its lines, and past the first or
last they pick the species.

A mission goes to **one** agent: the first free one on that world. To give every agent there the
same order, use a fleet order.

In the mission composer `⇥` switches between launching a new ship and queueing the mission (or
click either). A new ship can have a **role**, its standing order for life (`⇧⇥` cycles, or click
one):

| role        | what it does                                                                 |
| ----------- | ---------------------------------------------------------------------------- |
| none        | a generalist: does whatever the mission asks                                 |
| `scout`     | investigates and reports a plan, read-only: Claude runs in plan mode and Codex in its read-only sandbox (other species are only asked) |
| `builder`   | implements, runs the tests and commits on its world; never pushes            |
| `inspector` | runs the tests and reviews recent changes; fixes only what's broken          |

Claude gets the standing order as a system prompt, so your mission stays as you wrote it; other
species get it in front of the mission. A ship keeps its role across upgrades and restarts, and it
shows in the outliner, on its cards and in its hull: scouts, frigates (builders) and inspectors
each have their own silhouette.

The composer also lists the ships already on the world: **give it to** one of them and it takes
the mission when it's next free (`⇥` steps through launch, each ship, and queue-for-anyone). If
`PROVIDER_API_KEY` is set, **Advisor** (Provider's Advice model) suggests where the mission should go
once you stop typing for a second: the ship whose work it continues, or a new ship and its role.
The suggestion shows under the text and is marked `✦`; when Advisor is sure (60% or more) it's
preselected, until you choose for yourself. Nothing is sent until you press `⏎`. Advisor sees the
mission text and each ship's species, role, status, current task, turns and context use.

In the mission composer, **drop screenshot files** onto the terminal or press **Ctrl+V** to
attach a PNG image from the clipboard (`wl-paste` on Wayland, `xclip` on X11). File drops support
PNG, JPEG, WebP and GIF, up to 8 images of 20 MiB each. If your terminal types a dropped path
instead of pasting it, press **Ctrl+O** to attach the path from the prompt field. **Ctrl+X** removes
the last attachment; Esc discards the draft and its image copies.

Empire saves private copies under `$XDG_STATE_HOME/empire/screenshots` (normally
`~/.local/state/empire/screenshots`), so queued missions can use them after the originals move or
disappear. The agent receives local image paths with instructions to inspect them; it needs image
viewing tools to use them. Submitted images are retained, and their names appear in the expanded
mission log.

`L` opens the selected world's **mission log** (its moons' missions included): what's under way
(and who's on it), what's queued and in what order, and what's done. `⏎` opens comms with a mission's agent, `x` calls a queued one off. `␣` expands a
mission to its full text, its world, fleet order, agent and times (`e` expands them all), and `s`
groups the log by status, world or moon, or fleet order.

`S` (or a click on the side panel's **situation log**) opens it in full: the last 500 entries,
newest first. `0`–`7` or `f` show one kind of news (alerts, missions, agents, trade, prestige,
worlds, errors), `w` narrows to the selected entry's world and its moons, and `/` searches the text;
`x` clears the filters. The pane below the list spells out the selected entry: its full text, when it
happened, its world and the agent involved, if that agent is still around. `⏎` goes to that agent or world,
and `c` opens comms with the agent.

## Commander

Press `C` or click **commander** in the footer to see what needs your attention across all
charted star systems, regardless of the selected system. The footer shows the number waiting.
Questions and approvals come first, then file conflicts, your failing PR checks, PR reviews
requested from you, missions queued without an agent, and finished agent work to inspect.
Working agents and ordinary uncommitted files do not fill the queue.

Use `↑↓` / `j k` or the wheel to select an item. Its world, question, context and available
choices appear below; `PgUp/PgDn` scroll the details. `⏎` (or a second click) opens the relevant
agent, world actions, PR, or deployment dialog. For PRs, `t` prepares a review or fix mission.
Answer questions and approve actions in the agent's comms; closing comms returns to Commander.
Items update as agents and PR surveys change. Reading the list alone does not dismiss anything;
finished work leaves the queue when opened in comms, and questions remain until resolved.
`Esc` returns to the galaxy.

## Needs and quick actions

Every world shows what it needs from you right under its name on the map: `✖#42` failing CI, `±7`
uncommitted files, `↑2` unpushed, `↓3` behind upstream, `⚔` a border conflict, plus `★` missions
under way and `☆` queued. The world card lists them in full. `r` opens them as **quick actions**:
one key sends the orders (as a mission for a free agent there, or a new ship if there's no one),
opens comms with an agent that's waiting, or settles a conflict.

A **fleet order** (`b` on a system or world) goes to every ship there at once (except ones waiting
on an answer). The cards show its progress, and the situation log reports when the whole fleet is done.

## Moons

A moon is a git worktree on its own branch with its own agent: parallel work that can't step on the
world's toes. `u` on a world founds one (in `~/.local/share/empire/moons/`); it orbits its world
with its own ships. Worktrees you made yourself show up as moons too. `D` docks a moon: its branch is
merged back with `--no-ff`, its agents are recalled and the worktree removed. Both sides must be
committed; a merge conflict is a **collision**: the merge is called off and nothing changes.

## Trade routes

Worlds with a GitHub remote trade with the **GitHub station** at the edge of the galaxy. Unpushed
commits are freighters on their way to it; your open pull requests (`gh pr list --author @me`, every
couple of minutes) are docked around it, coloured by CI: green, amber while running, blinking red when
failing. The station glows the colour of its worst PR, CI turning red or green is reported in the
situation log, and world cards list unpushed commits and PRs under **trade**.

Click the **GitHub station or its label** to browse projects in the current galaxy and their open
PRs. It also appears in the outliner for keyboard selection. `⇥` switches between **all open PRs**,
those **waiting on your review**, and **your PRs**. Projects with failing checks of yours come
first, then those with reviews waiting on you; each lists your PRs, then reviews asked of you
(marked ◆, with the author), then everyone else's, with CI status. `t` on a PR opens the mission
composer for its project with a brief ready to send: review someone else's PR (read-only, nothing
posted without asking), fix your PR's failing checks, or address the review feedback on it. On a
project row, `t` starts a blank mission there. Use arrows or
the mouse wheel to scroll, and Enter (or a second click on a selected row) to focus a project or
open a PR in your browser. Esc returns to the galaxy.

The station uses `gh` to refresh up to 100 of your open PRs, and up to 100 of everyone else's,
per project every 150 seconds; only your own PRs dock at the station and count toward needs and
prestige.
It shows when results were last checked and keeps cached PRs visible if GitHub is unavailable.
Projects outside the current galaxy are not included.

## War

Two agents on the same world editing the same file is a **border conflict**: the world flashes, the
rivals trade laser fire and the card names the contested files. Claude reports every file it edits
through a hook; for other agents the daemon reads "Edited src/a.rs" style lines off their screens.
Select one of them and press `P` to give it **priority**: the others are told to leave those files
alone.

## Fog of war

The galaxy remembers every world you've had agents on (the 40 most recent). Worlds with no one there
stay on the map as **dormant** planets (◌ in the outliner), and the longer they're left alone (no
agents, no commits) the more they grey out, until after a few weeks they're lost in a violet nebula.
The map shows where your attention has been. `e` sends a ship back; `X` forgets a world.

## Victory and development

Worlds earn **prestige** for real progress: +2 for every completed mission, +1 when the work tree
goes from dirty to all committed (with agents there), +1 when a PR's CI turns green, +5 for a merged
PR. Prestige develops a world, with a fanfare on every level: **outpost → colony** (rings, at 4)
**→ city world** (city lights on its night side, 12) **→ ecumenopolis** (25), whose star gets a
Dyson swarm. The world card shows its development and how far the next level is.

## Answering from orbit

When an agent asks something (a permission prompt, "trust this folder?", a y/n), an
**incoming transmission** pops up over the galaxy with what it's about and the choices.
Press `1`–`9` to answer, `⏎` to open comms instead, `⇥` for the next one waiting.

## When the galaxy is closed

- **Desktop notifications** (`notify-send`) when an agent is blocked, or finishes a turn longer than
  15 seconds. A question's choices become buttons on the notification: answer without opening
  anything. They pause while a galaxy is open, which does its own alerting.
- **`empire status`** prints one line for a status bar (`▲1 ✔2 ◆3`); `empire status --json` gives
  waybar-style `{text, tooltip, class}` with every agent in the tooltip.

## Stats

- **Resources** (top bar): how much of your Claude 5-hour / weekly and Codex allowance is used,
  and when the tightest window refills. Claude's come from the statusline the daemon gives every
  Claude it launches (your own statusline still shows inside it); Codex's from its session logs.
  Claude's only appear after it has talked to the API at least once.
- **Select a world** for its card: branch, ahead/behind, uncommitted files ("stability"),
  commits, files and main language, population, turns finished, lines changed, spend, and how
  full each Claude's context is.
- **Select a system** for the totals across its worlds: capital, unrest (uncommitted work) and
  trade (commits not pushed yet).

## Status detection

- **Claude Code** is launched with injected hooks (`--settings`) that call `empire hook …`, so
  it reports working, done and needs-permission itself.
- **Everything else** is read from the screen: a live `esc to interrupt` hint means working,
  approval phrasing near the bottom ("Do you want to proceed?", "Would you like to run…",
  "trust this folder") means blocked, and quiet after work means done. Agents whose idle
  screen animates (codex) are judged by the hint only.

## Files

- `daemon.rs`: PTYs (`portable-pty`), terminal emulation (`vt100`) including answers to the
  terminal queries TUIs block on (cursor position, device attributes, colours), status
  detection, socket server, Claude hooks.
- `proto.rs` / `client.rs`: JSON-lines protocol, plus background threads for the agent list,
  live screen streaming and an ordered command queue.
- `game.rs`: galaxy model, camera, input, colonize/deploy/decommission flows.
- `draw.rs`: procedural dithered planets, ships, outliner, situation log, dialogs, comms.
- `survey.rs`: background git survey of every world (status, size, worktrees, PRs via `gh`) and
  Claude/Codex usage.
- `gfx.rs` / `octant.rs`: the octant renderer from mission-control. `sound.rs`: robot cues.
