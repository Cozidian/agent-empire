# AGENTS.md

Agent Empire (`empire`): a Rust terminal app that runs coding agents (claude, codex, grok, …) as a
4X space game. Folders are planets, parent folders star systems, agents ships. `README.md` is the
user manual: keep it in step with any behaviour, key or file you change.

## Layout

Single binary crate, no workspace. `src/main.rs` dispatches subcommands (`daemon`, `hook`,
`statusline`, `status`, `upgrade`, `stop`, `sounds`; no argument opens the galaxy).

- `daemon.rs`: owns every agent's PTY and `vt100` screen, status detection, the unix-socket
  server, Claude/Grok hooks, resume after restart.
- `proto.rs`: JSON-lines wire protocol (`Req`/`Resp`), `socket_path()`. `client.rs`: the game's
  background threads talking to the daemon.
- `game.rs`: the `App` model, `Mode` enum (one variant per dialog), input (`handle`), main loop,
  prestige, missions, needs. Largest file; its tests live in `mod *_tests` blocks at the bottom.
- `draw.rs`: all rendering — pixels for the galaxy, text cells for HUD and dialogs. Each `Mode`
  has a draw function dispatched from `frame()`.
- `survey.rs`: background thread surveying each world (git, `gh` PRs, usage limits).
- `structure.rs`: AI resources found in a world (AGENTS.md, skills, MCP, hooks…) and their
  construction briefs. `role.rs`: ship roles and their launch flags. `jev.rs`: TypeSafe Jev
  suggestions (needs `TYPESAFE_API_KEY`). `screenshots.rs`: mission image attachments.
- `gfx.rs` / `octant.rs`: framebuffer and octant renderer. `sound.rs`: synthesized cues and music.

## Build, run, test

```
cargo build --release          # binary: ./target/release/empire
cargo test                     # all unit tests; no daemon, network or audio needed
cargo test jev_live -- --ignored --nocapture   # hits the real Jev API, needs TYPESAFE_API_KEY
./target/release/empire        # open the galaxy (starts/hot-upgrades the daemon)
./target/release/empire status # quick check against a running daemon
```

The dev profile is `opt-level = 3`, so `cargo build` is already fast at runtime.

To try a build without touching the user's real daemon and agents, give it its own socket and
state: `EMPIRE_SOCK=/tmp/empire-dev.sock XDG_STATE_HOME=/tmp/empire-dev ./target/release/empire`
(and `empire stop` with the same variables afterwards).

## Conventions

- **Not rustfmt-formatted.** Long lines (up to ~200 chars) and one-line `match` arms are the
  house style. Never run `cargo fmt` on the repo; match the surrounding code by hand.
- Short names (`w`, `p`, `l`, `t`, `fb`, `app`), and short `///` doc comments that say what a
  thing *is* in the game's own terms ("A moon (git worktree) orbits…"). Comments are sparse,
  lowercase-first inline remarks explaining *why*.
- Game vocabulary maps to real things; keep it consistent: world/planet = folder, system = parent
  folder, ship = agent, moon = git worktree, mission = queued prompt, station = GitHub, prestige =
  real progress, structure = AI resource file.
- A new dialog = a `Mode` variant + a branch in `handle()` + a draw fn in `frame()` + a line in
  `help()` and, if it's a galaxy key, the footer hint and the README key table.
- Errors in the UI are toasts or situation-log entries (`app.say(Ev::…)`), not panics. Shelling out
  (`git`, `gh`) returns `Option`/`Result` and degrades quietly.
- Tests: plain `#[test]` in the same file. Game tests drive `handle()` with `Client::isolated()`
  and render with `draw::frame` into an `Fb`, asserting on the text cells. Filesystem tests use
  a temp dir under `std::env::temp_dir()` and clean up.
- Commit messages: one short, present-tense summary in the game's language
  ("Worktree moons: parallel branches orbiting their world, docked by merge"), body if needed.

## Don'ts

- Don't break the protocol for a running daemon: the galaxy hot-swaps the daemon in place
  (`exec`, same pid) and old/new builds must still talk. Add fields to `proto.rs` with
  `#[serde(default)]`; don't rename or remove them.
- Don't run `empire stop` or kill the daemon (`$XDG_RUNTIME_DIR/empire-<uid>.sock`, or `$EMPIRE_SOCK`): it recalls
  every agent the user has running, possibly including you. Tests must not touch the real daemon
  or real state in `~/.local/state/empire` (prestige, history, missions, screenshots).
- Don't pass trust/permission-bypass flags to agents (e.g. Grok's `--trust`); scouts must stay
  read-only where the species enforces it (`Role::args`).
- Don't commit `target/` or `demo/*.mp4`, and don't push.
- No new dependencies or external assets without a clear need; sounds and graphics are
  synthesized in code on purpose.
