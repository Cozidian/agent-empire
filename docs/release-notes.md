Empire turns project folders and persistent coding-agent terminals into a playable galaxy.

- Launch Claude, Codex and Grok in their native terminals, with their own permission controls.
- Track tasks, worktree stations, GitHub activity and project findings in one place.
- Keep agents running when the galaxy closes; reconnect when you return.
- Optional advisors offer mission routing, report severity, approval hints, overlap detection,
  outcome tags and ship pulse. They are off by default; remote advice requires an explicit service URL.

## Install without Rust

Download `install.sh` from this release and run `bash install.sh --version v0.1.0`.
It chooses your architecture, verifies the archive against `SHA256SUMS` and installs to
`~/.local/bin`. Use `--bin-dir DIR` to choose another directory. It does not start or stop agents.

Linux x86-64 and ARM64 are supported. Binaries require glibc 2.35 or newer and the ALSA runtime
library (`libasound2` on Ubuntu 22.04, `libasound2t64` on Ubuntu 24.04, `alsa-lib` on Arch).
Git and a true-color terminal are needed; install and authenticate your coding agents separately.

For private-repository access, use an authenticated GitHub CLI:

```sh
mkdir empire-release
gh release download v0.1.0 --repo Cozidian/agent-empire --dir empire-release
bash empire-release/install.sh --version v0.1.0 --from-dir empire-release
```

You can also extract the matching archive and copy `empire` into a directory on your PATH.
Use `empire --version` to check the installed build. See the README and `docs/guide` for the user guide.
