# AGENT EMPIRE

Run your coding agents like a 4X space empire. Every **folder is a planet**, its parent folder
is a **star system**, and every agent (claude, codex, grok, …) is a **ship in orbit** around
the folder it works in.

```
cargo build --release
./target/release/empire
```

To run `empire` from any folder, link the release build into a directory on your `PATH`
(run these commands from the repository):

```sh
mkdir -p "$HOME/.local/bin"
ln -s "$PWD/target/release/empire" "$HOME/.local/bin/empire"
empire
```

This assumes `~/.local/bin` is on your `PATH`. Rebuild with `cargo build --release` after
source changes; the link uses the updated build automatically. If you move the repository,
update the link to its new location.

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

Zooming far out switches to a **system overview**: one small star per system, quiet connections,
and world/ship counts with waiting or finished work (including agents on moons). The GitHub
station becomes a simple diamond with its PR count and failing checks. Labels move or shorten
to avoid overlapping. When you have sectors, zoom passes through a **sector map** before full
planetary detail: softly shaded regions show their names and small member worlds, with ships
and orbit effects hidden. The title bar says **SECTORS** at this distance. Further zoom brings
back full planets, ships and effects. With no sectors, the existing two views remain.

Click a system or sector region/name to select it, then press **Enter** or click it again to
zoom in. Enter on a distant system first visits the sector layer; Enter on a sector zooms into
its worlds in that system. A sector spanning systems uses the same name and colour in each,
with a count of its represented systems. The outliner adds selectable sector rows at middle
zoom and still lets you select individual worlds and agents at any distance.

| key              | action                                                              |
| ---------------- | ------------------------------------------------------------------- |
| `n`              | **colonize**: launch an agent in a folder (pick one, or type a new path and it's created) |
| `e`              | **deploy** another agent to the selected world                      |
| `⏎` / click twice | **land** on a planet to browse buildings; on a ship, open its card |
| `Ctrl-]` / `F12` | leave comms, back to the galaxy                                     |
| `⇥`              | jump to the next agent that needs you                               |
| `C`              | **commander**: your attention queue across all star systems         |
| `A`              | **anomalies**: discoveries, investigations and history across worlds |
| `V`              | **sectors**: group worlds and inspect their combined activity       |
| `H` / `h`        | **hyperlanes**: manage named connections / toggle dependency overlay |
| `O`              | **campaigns**: objectives, linked missions and reviewed handoffs    |
| `Y`              | **shipyard**: saved ship designs: rename (`r`) or scrap (`x`) them  |
| `t`              | **mission**: write a task for the selected world (launch a ship with it, or queue it) |
| `b`              | **fleet order**: one order to every agent in the selected system or world |
| `u`              | **moon**: found a git worktree off the selected world, with its own agent |
| `D`              | **dock** the selected moon: merge its branch back in and retire it   |
| `P`              | give the selected agent **priority** in a border conflict            |
| `X`              | forget the selected dormant world                                   |
| `r`              | what the selected world **needs**, as quick actions                |
| `B`              | the selected world's **structures**: build or renovate its AI resources |
| `L`              | the selected world's **mission log**: under way, queued, done       |
| `T`              | strike finished missions on the selected world from its log         |
| `S`              | the **situation log** in full: filter it, read an entry's details   |
| `x`              | **decommission** the selected agent (asks first)                    |
| `r` on a ship card | **refit** a deployed ship from a saved design: preview, `⏎` then `y` confirms, `c` cancels |
| `↑↓` / `j k`     | move through the outliner (the wheel scrolls it and the log beneath) |
| `wasd` / drag    | pan · `±` / wheel zoom · `z` fit galaxy · `f` focus selection        |
| `m` / `M`        | mute alerts / mute background music (Shift+m)                       |
| `o` `?` `q`      | outliner · fleet manual · leave                                     |

In **colonize** (`n`), folder suggestions use case-insensitive fuzzy matching: `doc` can find
`Documents`, and `dwn` can find `Downloads`. Exact names and prefixes rank ahead of substrings
and scattered letters. The parent path stays literal; only its last segment is searched.
Hidden folders appear when that segment begins with `.`. **Tab / Shift+Tab** cycle matches,
scrolling the list as needed and preserving the folder's real spelling. Enter still uses the
path in the field, so new folders can be created as before.

**Ctrl+Backspace**, **Alt+Backspace**, or **Ctrl+W** erase the path segment before the cursor
(`code/somethi` → `code/`). Use Alt+Backspace or Ctrl+W if the terminal does not distinguish
Ctrl+Backspace. Ordinary Backspace removes one character; **Ctrl+U** clears the field.

The galaxy plays a quiet, original space-themed score: slow synth pads, low swells and
sparse bells in a continuous 64-second loop. Press **`M` (Shift+m)** on the galaxy
to mute or resume the music; **`m`** controls alert sounds independently. The footer
shows the music state. Music starts on each time you open the galaxy and stops when
you leave. It is synthesized locally, with no downloads or audio files needed.

Comms opens as a **chat popup** over the galaxy: the agent's terminal, fitted to the popup, and a
line to write in underneath. `⏎` sends what you wrote (or, with nothing written, presses Enter for
the agent); when it's waiting on a question, `1`–`9` and `↑↓` answer it. `PgUp/PgDn` or the wheel
scroll back, `Esc` or a click outside closes it, and `⇥` takes you to the full-screen terminal.

In the popup, drag across the agent's output to **copy** it (double-click selects a word;
`Ctrl+C` copies the selection again). **Ctrl+V** pastes clipboard text or attaches a screenshot;
terminal paste and dropped image paths work too. Pasted line breaks are preserved, and
`Shift/Alt+Enter` or `Ctrl+J` adds a line without sending. Images appear above the reply:
`Ctrl+X` removes the last, and `Ctrl+O` attaches an image path typed into the reply. Press Enter
to send the text and images together. Failed delivery keeps the draft; Esc discards unsent
image copies. Send or clear a draft before switching to full screen.

The popup's right-hand scroll track can be clicked to jump through terminal history.
`Ctrl+Home` goes to the oldest available output, `Ctrl+End` returns to live. For agents that
manage their own scrolling, wheel and page events are forwarded using popup-relative coordinates.

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
| `scout`     | investigates and reports a plan, read-only: Claude runs in plan mode, Codex in its read-only sandbox, and Grok in plan mode inside the read-only sandbox (other species are only asked) |
| `builder`   | implements, runs the tests and commits on its world; never pushes            |
| `inspector` | runs the tests and reviews recent changes; fixes only what's broken          |

Claude gets the standing order as a system prompt, and Grok as `--rules`, so your mission stays as
you wrote it; other species get it in front of the mission. A ship keeps its role across upgrades and restarts, and it
shows in the outliner, on its cards and in its hull: scouts, frigates (builders) and inspectors
each have their own silhouette.

### Ship designs

A **design** is a saved outfit for new ships: a species, a role, standing orders of its own and
the skills it should lean on. In the mission composer `Ctrl+D` picks the next design (and then none
again); it sets the species and role, switches to launching, and shows the design under the role
row: its orders, and each skill marked **✓** if the chosen species can find it from this world or
**✗** if it can't. Everything stays adjustable for this launch: `↑↓` species, `⇧⇥` role, and
`Ctrl+R` **refits** the ship: write its standing orders (`Alt+⏎` new line), `⇥` to the skills and
`␣` to give or take one, `⏎` to keep the changes or `Esc` to drop them. `Ctrl+S` in the refit saves
the ship as a design (saving under an existing name updates that design). With no design picked,
the composer launches exactly as before.

The ship gets the design's orders after its role's, then a line naming its skills, the same way
as a role: Claude as a system prompt, Grok as `--rules`, Codex as `developer_instructions`, and
other species in front of the mission. Skills aren't installed or copied: a ship can use only
skills its species finds on its own, so ✗ ones are named but unavailable (the ship still launches,
with a warning).

| species | finds skills in (in the world, up to its repo root · in your home)             |
| ------- | ------------------------------------------------------------------------------- |
| claude  | `.claude/skills` · `~/.claude/skills`                                           |
| codex   | `.agents/skills` · `~/.agents/skills`, `~/.codex/skills`                        |
| grok    | `.grok/skills`, `.agents/skills`, `.claude/skills` · the same under `~`          |
| others  | none: skills are only named in their orders                                     |

A design never holds a world's files, and a world's structures (its skills, AGENTS.md, MCP
servers…) stay with the world. `Y` opens the **shipyard**, the list of designs: `r` renames one
and `x` scraps it. Designs are kept in `$XDG_STATE_HOME/empire/designs.json` (normally
`~/.local/state/empire/designs.json`) and survive restarts. A ship keeps its own copy of its
orders and skills, through upgrades and restarts, so renaming or scrapping a design never touches
ships already launched from it; the ship card shows which design a ship came from.

### Refitting a deployed ship

A ship already flying can take a saved design without losing its work. (`Ctrl+R` in the
mission composer is different: it outfits a ship that hasn't launched yet.) Open the ship's card
(`⏎` or a second click on the ship). It lists **open comms**, **give its world a mission**,
**refit from a saved design** and **decommission**, plus **priority** during a border conflict.
`r`, or the refit entry, opens the refit view. The top line shows what's **aboard** now. Below it,
`↑↓` steps through the shipyard's designs, and a preview shows what the selected design would
change for this ship:

- **role**: `builder → inspector`, or `(unchanged)`
- **orders**: the old standing orders as `−` and the new ones as `+` (first line each), or `(dropped)` / `(unchanged)`
- **skills**: `+` gained, `−` dropped, `=` kept. Each new skill is marked `✓` or `✗` for whether
  *this ship's* species finds it from its world (see the table above). `✗` skills are still named
  in its orders.
- **access**: whether it gains or loses a scout's read-only launch (Claude, Codex, Grok)

Nothing changes yet. `⏎` asks **"Refit as …? y refit when idle"**, and only `y` orders the refit:
any other key goes back to the preview. Picking the outfit the ship already carries only says so.
The preview warns when the species can't resume a session (anything but claude, codex and grok).

A refit means a **session handoff**: the agent is stood down and launched again as the same
ship (same id) with the new launch flags. It resumes its own session through its harness: `claude
--resume <session>`, `codex resume <session>`, `grok --resume <session>`. Its conversation and
work carry over, and the harness keeps its own permissions; no bypass flags are added. This waits
until the ship is **idle**, between turns and with no mission sent or under way. A ship waiting for
or under refit takes no queued missions; missions given to it wait for it.

The card keeps the outfit that's actually running (the **outfit** row: role, design, own orders,
skills) apart from the refit (the **refit** row):

| refit row | meaning |
| --------- | ------- |
| **◌ PENDING** | ordered; waits until the ship is idle. The old outfit is still the one running |
| **⟳ APPLYING** | standing down and relaunching on its session (a few seconds) |
| **✓ ACTIVE** | relaunched with the new outfit; the outfit row now shows it |
| **✖ FAILED** | the new launch failed, or exited within a few seconds, so the ship was relaunched with its old outfit; the note says why |
| **⊘ UNSUPPORTED** | it can't be resumed: the species has no resume by session id, or the ship has no session yet (a Claude that hasn't finished a turn). Nothing was restarted |

The situation log reports when a refit becomes active, fails or is unsupported. `c` in the refit
view calls off a pending refit or clears a finished one's notice (not while it's applying).

A refit belongs to that one ship. It copies the design's role, orders and skills into the ship's
own outfit, as a launch does. Changing or scrapping the design later doesn't touch the ship, and
nothing is written to the world: its skills, AGENTS.md and other structures stay as they are.
Refit state is saved with the ship (`agents.json` in the state directory, and the in-place upgrade
handoff), so it survives restarts of both the galaxy and the daemon. A refit cut short by a daemon
restart goes back to **pending**, and the ship comes back as it was until it's next idle.

The composer also lists the ships already on the world: **give it to** one of them and it takes
the mission when it's next free (`⇥` steps through launch, each ship, and queue-for-anyone). If
`TYPESAFE_API_KEY` is set, **Jev** (TypeSafe's System One model) suggests where the mission should go
once you stop typing for a second: the ship whose work it continues, or a new ship and its role.
The suggestion shows under the text and is marked `✦`; when Jev is sure (60% or more) it's
preselected, until you choose for yourself. Nothing is sent until you press `⏎`. Jev sees the
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

## Campaigns

Press **`O`** to give missions a shared purpose, such as “Ship Kiplat authentication.” A campaign
keeps an objective, acceptance criteria, ordered stages, linked missions, handoff notes and review
history. It stays available after ships leave or missions are removed from the daemon's log.

Start with **`n`**, enter the objective, then the acceptance criteria. **`s`** plans a stage: choose
its world or worktree moon and describe the task. Plan as many stages as useful; only a stage
whose predecessors have been accepted can receive a campaign mission or an accepted outcome.
A sequence might be scout → accepted plan → builder on a moon → inspector → reviewed docking.
It is optional: one ship can carry the whole campaign, and a stage can link several missions.

**`m`** opens the existing mission composer with the objective, criteria, current task, and
accepted handoffs and verification notes from preceding stages. You can edit the draft, attach
screenshots, choose a ship, or choose a species and role for a new one. It defaults to queueing;
nothing is sent until you submit the composer. **Esc** cancels and returns to the campaign.
New campaign missions link back to their stage through saved metadata. **`l`** links an existing
mission explicitly, including work on another world or moon, without sending a new order.
The selected stage and its linked ship appear first, followed by the objective, criteria, handoff
and verification. **`d`** expands or collapses saved mission prompts and history.

**`o`** opens the stage's linked ship, prioritizing one waiting for input; **Esc** returns to the
same campaign. Agent questions and replies stay in the ship's native terminal. When a waiting
state and question are detected, the campaign shows them above the stage; questions written as
ordinary agent output may require opening the ship to read them. A live linked ship remains
accessible after its mission reports finished. **Enter** opens the stage world's mission log.

The review sequence is deliberately manual:

1. A finished agent turn means **reported finished**. It does not establish correctness.
2. **`h`** records findings, changes, evidence and unresolved questions for the handoff.
3. **`v`** records what you checked and the results, marking the stage **verified by the player**.
   Linked missions still queued or under way must finish first.
4. **`a`** accepts that stage. It makes the next stage available; it does not launch it.
5. Once every stage is accepted, **`c`**, then **`y`**, accepts the campaign against its objective
   and criteria. No test result or agent status completes a campaign automatically.

| key in campaigns | action |
| ---------------- | ------ |
| `Tab` / `Shift+Tab` | switch between campaigns and stages |
| `↑↓` / `j k` / wheel / click | select; `Home/End` reach the first/last item |
| `PgUp/Dn` / wheel over details | scroll the selected campaign |
| `o` | open the linked ship (waiting ship first); `Esc` returns to the campaign |
| `d` | show or hide history and saved mission prompts |
| `n` / `e` | create a campaign / edit its objective and criteria |
| `s` / `t` | add a stage / edit its world and task |
| `m` / `l` | draft a mission / link an existing mission |
| `h` / `v` / `a` | record a handoff / record verification / accept the stage |
| `r` | reopen the selected stage and subsequent reviews |
| `c` | complete the campaign after `y` confirmation |
| `x` | archive or restore the campaign; missions and agents keep running |
| `Enter` / `Esc` | open the stage world's mission log / return to the galaxy |

Text fields support pasted paragraphs and **Shift+Enter**, **Alt+Enter**, or **Ctrl+J** for a
newline. **Enter** saves or advances; **Esc** discards the edit. Objectives are single lines of
up to 120 characters; criteria, tasks and review notes allow up to 8,000 characters each.

Changing the objective or criteria reopens all reviews. Editing a stage, linking new mission
evidence, or explicitly reopening a stage clears its acceptance and subsequent reviews; previous
verification and handoff notes remain in history. If the campaign changes while its mission
composer is open, submission asks you to cancel and reopen it with fresh context.

Missing missions retain their saved prompt, world, agent, times and last observed status, and
are labelled as snapshots. Their absence does not mean success: you can record a manual review
using evidence you still have. Missing folders keep their stages but cannot receive a new draft.
Unarchived campaigns keep available stage worlds charted; archive them with `x` before forgetting
those dormant worlds with `X` on the galaxy.

Campaigns are saved in `$XDG_STATE_HOME/empire/campaigns.json` (normally
`~/.local/state/empire/campaigns.json`). Galaxy windows share locked, atomic updates; unreadable
records are preserved and save failures keep the edit open. Campaigns prepare prompts and record
outcomes; Claude, Codex and the other harnesses keep their own reasoning, tools, context management
and execution. Empire does not start the next stage, merge a branch, or dock a moon automatically.

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

## Sectors

Press **`V`** to group worlds around a product or initiative, independently of their folder layout.
For example, create **Kiplat** and add its runtime, frontend and Bifrost repositories. A world can
belong to several sectors. Its moons follow it automatically, so work on a worktree counts toward
the parent world's sector.

The sector editor shows groups on the left and worlds on the right. **`n`** creates a named sector;
**Tab** switches panes, arrows or the wheel select, and **Space** adds or removes the selected world.
Members are marked **`[+]`**. **`p`** adds an existing folder by path (including `~/…`) if it is not
charted yet. Adding a worktree path uses its host world. Membership never creates or moves folders,
launches ships, or sends orders.

| key | action |
| --- | --- |
| `n` / `r` | create / rename a sector; Enter saves, Esc cancels |
| `Tab` / `←→` | switch between sectors and worlds |
| `↑↓` / `j k` / wheel | select; `Home/End` and `PgUp/PgDn` navigate long lists |
| `Space` | toggle the selected world's membership in the worlds pane |
| `p` | add an existing folder by path |
| `⏎` | focus the selected sector or locate the selected world, depending on pane |
| `f` | focus the sector on the galaxy |
| `a` | open anomalies scoped to the sector, including its moons |
| `x` | dissolve the selected sector after `y` confirmation; other keys cancel |
| `Esc` | return to the galaxy |

The summary combines the sector's ships (working, waiting and finished), missions under way,
queued missions and active anomalies. Moons contribute to activity without increasing the parent
world count; unrelated worlds are excluded. Missing member folders remain listed so their
membership can be removed.

**`f`** fits the camera around the sector, stopping at the sector layer or farther out for widely
spread groups. It highlights member worlds; at a distance, their star systems are marked **▱**. A small map banner shows the sector's activity. The rest of the galaxy
and its alerts remain available. **Esc on the galaxy** clears the sector highlight and selection.
At middle zoom, **V** opens the editor on the selected sector. Region outlines show group membership,
not exclusive borders: a world can belong to several sectors. This grouping does not declare
dependency relationships; use **hyperlanes** to name those connections explicitly.

Available sector members stay charted even without ships. Remove a dormant world's memberships
in `V` before forgetting it with `X`. Dissolving a sector removes only the group: folders, agents,
missions and anomalies remain intact.

Sectors are saved in `$XDG_STATE_HOME/empire/sectors.json` (normally
`~/.local/state/empire/sectors.json`). Galaxy windows refresh the shared record and use locked,
atomic updates. A corrupt or unreadable record is preserved and reported in the editor and
situation log. Sector names are unique ignoring case and contain 1–48 characters.

## Hyperlanes

Press **`H`** to chart named, directional relationships between worlds. A sector says which
worlds belong together; a hyperlane says how they relate. For example, you could chart
`frontend — calls → Bifrost` and `Bifrost — depends on → runtime`. These are your descriptions,
not automatically discovered dependencies, and do not send orders to agents.

Press **`n`** in the editor, choose the source world, press **Enter**, choose the destination,
press **Enter**, then type the relationship and press **Enter** to save. The selected planet
is the suggested source; a selected moon uses its parent world. Choose from charted worlds
and saved endpoints. To chart another existing folder without a ship, add it through `V`.
Labels need 1–48 characters. Identical links are rejected, while reverse directions and different
relationships between the same worlds are allowed. **Esc** cancels an unfinished draft.

| key in hyperlanes | action |
| ----------------- | ------ |
| `↑↓` / wheel / click | select a link or world |
| `PgUp/Dn` / `Home/End` | move through longer lists |
| `n` | create a connection: source → destination → label |
| `e` / `r` | edit endpoints and label / rename the relationship |
| `Backspace` | return from destination selection to source selection |
| `⏎` / `f` | show the selected connection on the map |
| `1` / `2` | locate its source / destination world |
| `h` | toggle the dependency overlay |
| `x` | remove the connection after `y` confirmation |
| `Esc` | cancel the draft, or return to the galaxy |

**`h` on the galaxy** toggles connections without opening the editor. The title changes to
**DEPENDENCIES**. This view retains small world markers even at distant zoom, so links always
join their actual endpoints. Sector boundaries remain visible, and decorative system lanes
are hidden. Curved arrows run from source to destination; opposite directions and parallel
relationships get separate curves. Labels move or disappear when space is tight; select a
curve to read its relationship and paths in the card. **Enter** or a second click opens that
connection in the editor; **f** fits its endpoints. At close zoom, full planets and ships return
with the dependency labels. Toggle **h** again for the ordinary map. Overlay visibility is
local to the current galaxy window and starts off.

Hyperlane endpoints stay charted while their folders exist. A missing endpoint remains in the
editor as **unavailable**, and its connection is kept but not drawn. Remove a dormant world's
links in `H` before forgetting it with `X`. Removing a link leaves folders, agents, sectors and
missions intact.

Connections are saved in `$XDG_STATE_HOME/empire/hyperlanes.json` (normally
`~/.local/state/empire/hyperlanes.json`). Galaxy windows refresh the shared record; edits use
locked, atomic writes. Corrupt or unreadable data is preserved and reported, and a failed save
keeps your draft open.

## Anomalies

Press **`A`** in the galaxy to browse persistent discoveries across worlds. A **⟡ count** beside
planet and system names marks active findings; a planet's count includes its moons, and the
zoomed-out system overview counts each finding once. World and system cards also show the count.
Commander includes open Git, moon and reported findings; failing PR checks retain their existing
Commander entry.

The first survey sources are **your PRs with failing checks**, **unresolved Git merge conflicts**,
and **charted worktree moons with no running agent**. An uncrewed moon is an observation, not a
claim that its work is abandoned. Ordinary dirty files and unpushed commits are not anomalies.
Surveys run while the galaxy is open; findings and their history survive closing it.

The browser lists each finding's status, source and world. The detail pane includes its full world
and system paths, evidence, discovery/observation ages, and lifecycle history.

| key | action |
| --- | --- |
| `↑↓` / `j k` / wheel | select a finding (`Home/End` jump to first/last) |
| `v` | cycle active, all, open, investigating, deferred, dismissed and resolved |
| `f` | cycle all kinds, CI, Git, Moon and Report |
| `w` / `s` | toggle the selected finding's world family / star system filter |
| `/` | search title, evidence, location or finding number; Enter/Esc ends editing |
| `x` | reset filters to all active findings |
| `PgUp/PgDn` | scroll the selected finding's evidence and history |
| `⏎` / click selected row | locate its world on the galaxy |
| `t` | draft a read-only scout investigation; review and edit before sending |
| `d` / `i` | defer / dismiss |
| `r` / `o` | mark resolved / reopen |
| `Esc` | return to the galaxy |

An investigation defaults to a new **scout**, preserving the species' supported read-only launch
settings. You can choose another destination in the ordinary mission composer. Keep the opening
`[anomaly #N]` tag to link the submitted mission to its finding. Cancelling the draft changes
nothing; a submitted mission is linked when the daemon reports it. A finished agent turn is
recorded in the history and **does not resolve the finding**. Review the evidence before resolving
it or commissioning a fix through an ordinary mission.

Repeated surveys update the same finding. A confirmed clear Git status, green PR checks, or a ship
returning to a moon resolves the corresponding observation. Recurrence reopens it; **dismissals
remain dismissed until you reopen them**. Deferred findings stay out of active counts until
reopened or a confirmed recovery/recurrence changes them. Missing PRs, unavailable folders,
offline GitHub and stale evidence do not prove recovery.

Explicit findings can be recorded without starting or contacting the daemon:

```sh
empire anomaly /path/to/world 'Instructions disagree with the code' \
  'AGENTS.md says npm; package.json declares pnpm. Needs investigation.' 'scout report'
```

Quote each argument; the final source label is optional. Reports are labelled **not independently
verified**. The same world and title update one reported finding. A read-only scout can provide its
findings in comms for you to record; this command does not bypass its filesystem permissions.
Reports appear on the next galaxy refresh. Use Enter to chart and locate a reported folder that
is not on the map yet.

Records live in `$XDG_STATE_HOME/empire/anomalies.json` (normally
`~/.local/state/empire/anomalies.json`). Updates are locked and atomically replaced, so simultaneous
reports and galaxy windows preserve each other's changes. An unreadable or corrupt store is left
untouched and reported in the browser and situation log.

## Needs and quick actions

Every world shows what it needs from you right under its name on the map: `✖#42` failing CI, `±7`
uncommitted files, `↑2` unpushed, `↓3` behind upstream, `⚔` a border conflict, plus `★` missions
under way and `☆` queued. The world card lists them in full. `r` opens them as **quick actions**:
one key sends the orders (as a mission for a free agent there, or a new ship if there's no one),
opens comms with an agent that's waiting, or settles a conflict.

A **fleet order** (`b` on a system or world) goes to every ship there at once (except ones waiting
on an answer). The cards show its progress, and the situation log reports when the whole fleet is done.

## Structures

Select a planet and press **Enter** or **f**, click it again, or zoom closely into it to land.
The planet has its own screen: buildings on the left, their files on the right, and a read-only
file preview below. Empty plots explain what can be built. Skills open their `SKILL.md`, and
all discovered files are available, including worlds with many skills.

Use **↑↓ / j k** or the wheel to choose, **1–8** to jump to a building, and **Tab / Shift+Tab**
or **←→** to move between buildings, files and the preview. Click a building or file to select it;
**PgUp/PgDn** scroll the preview. **Home/End** jump within the focused pane. **R** refreshes
the files and preview. **B** drafts a construction or renovation order for the selected building;
cancelling or submitting it returns here. **c** opens the world's lead ship's comms, returning
here when closed. **Esc** or the top-left galaxy link returns to the map.

Previews show up to 64 KiB, with a notice for larger files. An unreadable or removed file shows
an explanation in the preview; it does not close the planet screen.

A world's buildings are the AI resources in its folder: what it has built for the agents that work
there. The survey finds them every few seconds, and each one stands as a spire on the planet's upper
limb, in its own colour.

| structure        | what it is                                                       |
| ---------------- | ---------------------------------------------------------------- |
| ⌂ charter hall   | `AGENTS.md`: instructions every species reads                    |
| ▤ archive        | `CLAUDE.md` (also `.claude/CLAUDE.md`, `CLAUDE.local.md`)        |
| ✧ academy        | skills: `SKILL.md` folders in `.claude/skills`, `.agents/skills`, `.codex/skills`, `.grok/skills` |
| ⚑ barracks       | subagents in `.claude/agents`                                     |
| ⌘ command post   | slash commands in `.claude/commands` and `.codex/prompts`        |
| ⌁ relay          | MCP servers in `.mcp.json`, `.cursor/mcp.json` or `.vscode/mcp.json` |
| ⚲ watchtower     | hooks in `.claude/settings.json` / `settings.local.json`         |
| ⚐ embassy        | other agents' rules: `GEMINI.md`, Copilot, Cursor, Windsurf, Cline |

The world card lists what stands and how many plots are free. `B` opens the world's structures:
pick one and the mission composer opens with a construction order for a **builder** (or, for one
that already stands, an order to renovate it against the code as it is now). Read it, change it if
you like, and send it. When a structure goes up the situation log reports it and the world earns
+1 prestige (once per kind of structure); one that's removed is reported torn down.

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
PR, +1 for each new kind of structure. Prestige develops a world, with a fanfare on every level: **outpost → colony** (rings, at 4)
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
  full each Claude or Grok's context is. Grok's context and spend come from its session files;
  it has no rate-limit window in the resource bar.
- **Select a system** for the totals across its worlds: capital, unrest (uncommitted work) and
  trade (commits not pushed yet).

## Status detection

- **Claude Code** is launched with injected hooks (`--settings`) that call `empire hook …`, so
  it reports working, done and needs-permission itself.
- **Grok** gets the same reports from `~/.grok/hooks/empire.json`, written when a Grok ship
  launches. The file does nothing unless empire started that Grok. Until the folder is trusted,
  the screen is used instead: a live `send a message to interrupt` hint means working. Context
  and spend are read from the session directory. Empire does not pass `--trust`.
- **Everything else** is read from the screen: a live `esc to interrupt` hint means working,
  approval phrasing near the bottom ("Do you want to proceed?", "Would you like to run…",
  "trust this folder") means blocked, and quiet after work means done. Agents whose idle
  screen animates (codex) are judged by the hint only.

## Files

- `daemon.rs`: PTYs (`portable-pty`), terminal emulation (`vt100`) including answers to the
  terminal queries TUIs block on (cursor position, device attributes, colours), status
  detection, socket server, Claude and Grok hooks.
- `proto.rs` / `client.rs`: JSON-lines protocol, plus background threads for the agent list,
  live screen streaming and an ordered command queue.
- `game.rs`: galaxy model, camera, input, colonize/deploy/decommission flows.
- `draw.rs`: procedural dithered planets, ships, outliner, situation log, dialogs, comms.
- `structure.rs`: structures (AI resources) a world has built, and their construction orders.
- `anomaly.rs`: persistent findings, lifecycle history, filters and explicit reports.
- `campaign.rs`: objectives, ordered stages, mission snapshots and manual review history.
- `hyperlane.rs`: named, directional relationships between worlds.
- `design.rs`: saved ship designs, and which skill folders each species reads.
- `sector.rs`: named groups of worlds; `state.rs`: shared locked, atomic local storage.
- `survey.rs`: background git survey of every world (status, size, worktrees, PRs via `gh`) and
  Claude/Codex usage.
- `gfx.rs` / `octant.rs`: the octant renderer from mission-control. `sound.rs`: robot cues.

## Development backlog

[BACKLOG.md](BACKLOG.md) keeps the agreed direction for anomalies, sectors, campaigns, ship refits,
the chronicle and soundtracks.
