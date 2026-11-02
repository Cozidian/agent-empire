# AGENT EMPIRE

A persistent galaxy for your coding projects. Projects are **planets**, agents are **ships**,
and ordinary terminals live alongside them. Launch Claude, Codex or Grok, give it work,
run the project yourself, and return whenever it needs you.

```sh
cargo build --release
./target/release/empire
```

## Home and your projects

Press **H** to visit **Home**. Choose **Configure project root** and enter the folder containing
your projects, for example `/home/code` or `~/code`. Empire discovers Git repositories beneath
that folder and makes them planets. Their parent folders determine the sectors. Discovery runs
in the background, stops inside repositories, and skips hidden directories, symlink directories,
`node_modules`, `target`, and `vendor`. Git worktrees with a `.git` file are recognized too.

If no root has been saved, Empire uses `/home/code` or `~/code` when one exists. **R on Home**
rescans the root. A scan reads directory entries; it does not pull, clone or modify repositories.

**n** adds a folder manually, including folders outside the root and folders without Git.
A missing folder is created only after a second Enter. Adding a planet does not launch an agent.
Manually added folders and the root are saved in `$XDG_STATE_HOME/empire/workspace.json`
(normally `~/.local/state/empire/workspace.json`). Existing projects from Empire's history and
running sessions remain visible. Discovered projects stay charted without requiring an agent.

Home puts projects needing you first, with clear input/check labels and their latest question or task.
Other projects use compact rows grouped by parent folder; only nonzero ship and terminal counts
are shown. The selected project's full path stays in the footer. Folder headings are not selectable.
Choose a project to open its workspace, or press **e** on its Home row to launch a ship.
**r on Home** edits the project root. New ships depart from Home and settle into the project's
orbit; they remain there between tasks until you dismiss them.

## Working on a planet

Enter or double-click a planet to open its workspace:

- **Give this planet a task** (`t`): write a short instruction. The next available agent on this
  planet takes it. If all agents are working, it waits; agents waiting for permission are not free.
  With no live agents, the composer defaults to launching a new ship. **Tab** switches between
  an available agent and a new ship; **↑↓** chooses Claude, Codex or Grok when launching.
- **Launch an agent from Home** (`e`): choose a species and open its native terminal.
- **Open another terminal** (`s`): start your normal shell in the project folder. Open as many
  as you need for `git pull`, development servers, logs and manual testing. Shells never take
  agent tasks or generate agent permission/completion alerts.
- **Agents** and **Terminals** have separate sections. Every terminal has a stable number and
  shows its current title. Select one and press **Enter** to reopen it; **F12** or **Ctrl-]**
  returns with that session selected. **s** opens another terminal. **x** closes the selected
  session after confirmation and keeps you on the project. Closing a shell stops its running
  commands too; cancelling leaves it running.
- **Anomalies** (`a`): inspect detected problems and reported discoveries on this planet.
- **Project resources**: browse `AGENTS.md`, `CLAUDE.md`, skills, subagents, commands, hooks, MCP
  configuration and other agents' instruction files; each kind found stands as a spire on the
  planet. **Enter** or **e** opens the selected file in your editor (`$VISUAL`, `$EDITOR`, else
  nvim, vim, vi or nano) as a full terminal; quit the editor to come straight back with the
  file re-read, or step out with **F12** and pick it up again later (it's listed as an Editor
  under Terminals). On an empty plot, **Enter** or **n** lays a short first draft (`AGENTS.md`,
  `CLAUDE.md`, `.claude/skills/new-skill/SKILL.md`, `.mcp.json`…) and opens it; **n** on a kind
  that holds many adds another. Existing files are never overwritten. **B** instead briefs a
  ship to build or renovate it, as an ordinary task you can inspect before sending.

The project header shows branch, changed files and ahead/behind counts. Agent rows show their
current task or question. On wider terminals, selecting an agent with a handoff shows its actual
recent terminal output, including testing instructions when the agent provided them. This is an
excerpt, not an independently verified summary. Open the native terminal for the full conversation.

## Native terminals and quick tasks

Every key in a native terminal, including **Esc** and **Ctrl+C**, goes to that session.
**Ctrl-]** or **F12** returns to the project or galaxy. Scrolling uses the application's own mouse
support when available, otherwise Empire's terminal history. Drag to select and copy text.

The galaxy and terminal status bar adapts to the window width. Smaller windows shorten the
fleet summary and usage display, keeping connection status visible. Compact usage shows each
provider's most-used allowance window; meters, reset times and world counts return when space
allows. At the narrowest sizes, usage and quiet ship tallies give way to agents needing you.

In the planet task composer, **Enter** sends; **Alt+Enter**, **Shift+Enter** or **Ctrl+J** adds a
line. Pasted line breaks are preserved. **Ctrl+V** pastes clipboard text or attaches a screenshot.
Drop image paths, or type an image path and use **Ctrl+O**. **Ctrl+X** removes the last attachment.
Up to eight images, 20 MiB each, are copied to Empire's private screenshot directory so queued
tasks still work if the originals move. Failed delivery keeps the draft. Cancelling removes
unsent image copies. Agents need their own image-viewing tools to inspect attachments.

Tasks go to one available agent in the exact project folder. **L** on the galaxy opens the
planet's task history. A finished agent turn means **ready to check**; it does not mean that
implementation or testing succeeded. You remain in charge of testing, committing and pushing.

## Attention and discoveries

Ships and planets highlight agents needing an answer or ready for you to check. **Tab** on the
galaxy selects the next agent needing you; **C** opens the attention list across projects.
Permission questions remain incoming transmissions; number keys answer the displayed choices.
When the galaxy is closed, desktop notifications report questions and completed longer turns.

**A** opens anomalies across the galaxy; a planet's **a** action filters to that project.
Anomalies include failing CI and unresolved Git conflicts, plus findings reported by you or an
agent: bugs, unclear code, missing documentation and improvement opportunities. Each preserves
its source and evidence. Reported findings are labelled as not independently verified.

Select a finding and press **t** to prepare an investigation as an ordinary planet task. Its
brief asks for read-only investigation; the agent retains its native permissions. Review and edit
before sending. **i** dismisses, **r** resolves, **o** reopens. Automatic findings resolve only
when a fresh survey confirms recovery. A finished investigation does not prove resolution.
Use **v** to include resolved/dismissed history, **f** to filter kinds, and **/** to search.

Record a discovery without starting the daemon:

```sh
empire anomaly /path/to/project 'Confusing login flow' \
  'The callback discards the original destination; see src/auth.rs.' 'agent report'
```

## GitHub station

Projects with GitHub remotes connect to the **GitHub station**. Open it to browse project PRs
and CI status. **Tab** switches between all PRs, reviews requested from you, and your PRs.
**Enter** opens a PR in your browser. **t** prepares a task on that PR's planet: review someone
else's PR, fix failing checks, or address review feedback. Review tasks ask the agent to report
findings without posting anything. Inspect the instruction before sending.

The station uses your installed `gh` and its existing authentication. It refreshes up to 100 of
your open PRs and 100 other open PRs per project every 150 seconds, and marks stale cached results
when GitHub is unavailable. Unpushed commits still appear as freighters travelling to the station.

## Galaxy controls and atmosphere

| Key | Action |
| --- | --- |
| Enter / double-click | Open planet workspace or native agent terminal |
| H / n | Home / add a project folder |
| e / t | Launch an agent / give the selected planet a task |
| Tab / C | Next agent needing you / attention list |
| A | Anomalies |
| L / S | Selected planet's task history / situation log |
| i / x on a ship | Ship information / dismiss with confirmation |
| ↑↓ / j k | Select in the outliner |
| wasd / drag | Pan |
| ± / wheel | Zoom between systems, sectors and planets |
| z / f | Fit galaxy / focus selection |
| o | Toggle outliner |
| m / M | Mute alerts / mute music |
| ? / q | Help / leave the galaxy |

The galaxy overview can zoom out to six times the previous visible span. Keep scrolling down
or pressing **-** to pull back; **z** fits the whole galaxy using the same expanded range.

Planets retain their seeded procedural appearance, with curved daylight shading, soft atmosphere,
ocean glints and drifting cloud banks. Fitted two-colour octant cells give surfaces and glows smoother
gradients; small worlds receive a little extra light to stay readable on the map.
Useful progress adds rings, city lights and
other development without a separate management workflow. Music and sounds are synthesized
locally; music starts when the galaxy opens and stops when you leave. The score is composed
on the fly and never repeats: slow, hopeful chord changes in a drifting key, with sections
that move between pads, strings and a wordless choir. Echoing arpeggios, soft chimes and an
improvised lead melody join once each section settles. Home and the project
workspace show larger planetary portraits when there is enough room.

Campaigns, expeditions, ship designs/refits, prescribed launch roles, fleet orders, manual
hyperlane/sector editors and worktree creation/merging are retired from the workspace. Existing
folders, worktrees and saved records are not deleted. The daemon retains legacy wire fields and
session outfits so existing agents can survive upgrades. Campaign automation no longer dispatches
stages. Worktrees you already have can still appear as moons.

## Persistence and commands

The daemon owns agent and shell PTYs. **Closing the galaxy leaves them running**, including
servers started in ordinary terminals. Reopening reconnects. A new build can hot-upgrade the
daemon in place, keeping its PID and terminal file descriptors.

After a reboot or daemon crash, supported agents resume their recorded conversations. Ordinary
shells reopen in their original project folders; commands and development servers cannot survive
a reboot and must be started again. A deliberate `empire stop` closes every session and clears
the resume registry—do not use it merely to close the galaxy.

```sh
empire status          # status-bar summary
empire status --json   # Waybar-compatible JSON
empire upgrade         # hot-upgrade the daemon
empire sounds          # preview sound cues
```

State lives under `$XDG_STATE_HOME/empire` (normally `~/.local/state/empire`): workspace settings,
session registry, tasks, discoveries, screenshots, progress and visited projects.

To make a local build available on PATH:

```sh
mkdir -p "$HOME/.local/bin"
ln -s "$PWD/target/release/empire" "$HOME/.local/bin/empire"
```

## Development

Single Rust binary crate. `game.rs` handles the model/input, `draw.rs` renders it, `workspace.rs`
discovers projects, `daemon.rs` owns terminals, `proto.rs`/`client.rs` connect them, `survey.rs`
reads Git/GitHub status, and `anomaly.rs` stores findings. Graphics and sound are procedural.

```sh
cargo test
cargo build --release
```

Do not run `cargo fmt`; follow the existing compact style. Runtime tests must use isolated socket
and state paths, for example:

```sh
EMPIRE_SOCK=/tmp/empire-dev.sock XDG_STATE_HOME=/tmp/empire-dev ./target/release/empire
```

Never stop or alter the user's real daemon or state during tests.

### Graphics comparison

`cargo run --release -- graphics` opens an offline, animated comparison with fixed example
worlds at portrait and map sizes. **Space** switches between the polished renderer and the classic look;
**p** freezes rotation for comparison, and **q / Esc** leaves. It never connects to the daemon,
starts agents or audio, or reads/writes workspace state.

The polished renderer is the default in the galaxy and planetary portraits. The comparison keeps
the same procedural world identities and terminal resolution for both looks.

For reproducible 140×48 terminal captures, use `empire graphics --snapshot` and
`empire graphics --snapshot --classic` (ANSI output, fixed animation time).
