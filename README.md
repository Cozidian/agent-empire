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

| key              | action                                                              |
| ---------------- | ------------------------------------------------------------------- |
| `n`              | **colonize**: launch an agent in a folder (pick one, or type a new path and it's created) |
| `e`              | **deploy** another agent to the selected world                      |
| `⏎` / click twice | **comms**: a live, interactive terminal with the agent             |
| `Ctrl-]` / `F12` | leave comms, back to the galaxy                                     |
| `⇥`              | jump to the next agent that needs you                               |
| `t`              | **mission**: write a task for the selected world (launch a ship with it, or queue it) |
| `b`              | **fleet order**: one order to every agent in the selected system or world |
| `u`              | **moon**: found a git worktree off the selected world, with its own agent |
| `D`              | **dock** the selected moon: merge its branch back in and retire it   |
| `P`              | give the selected agent **priority** in a border conflict            |
| `X`              | forget the selected dormant world                                   |
| `T`              | strike finished missions on the selected world from its log         |
| `x`              | **decommission** the selected agent (asks first)                    |
| `↑↓` / `j k`     | move through the outliner                                           |
| `wasd` / drag    | pan · `±` / wheel zoom · `z` fit galaxy · `f` focus selection        |
| `o` `m` `?` `q`  | outliner · mute · fleet manual · leave                              |

Inside comms every key, including Esc and Ctrl-C, goes straight to the agent. The mouse wheel
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
- `gfx.rs` / `octant.rs`: the octant renderer from mission-control. `sound.rs`: robot cues.
