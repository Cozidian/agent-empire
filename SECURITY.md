# Security

Agent Empire is early-stage software for a single Linux user. Only the current development
version receives fixes; there is no supported older release series yet.

## Reporting a vulnerability

Use [GitHub’s private vulnerability reporting](https://github.com/Cozidian/agent-empire/security/advisories/new). Include the affected commit, reproduction steps using a disposable project, and the
impact. Do not paste credentials or private agent transcripts into a public issue.

If private reporting is unavailable, open an issue asking the maintainer for a private reporting
channel, without exploit details or sensitive data. Never post a working secret; revoke it first.

## Trust boundary and local data

The Linux daemon authenticates Unix-socket peers by user ID, on both client and server connections.
Its socket is mode `0600`. On daemon startup, the Empire state directory is created or tightened
to `0700`; daemon logs, handoffs and registry writes use `0600`. Existing symlink state directories
are rejected. A custom `EMPIRE_SOCK` still requires a trustworthy parent directory.

Any process running as your user is within the trust boundary and can control your agents and
terminals. Empire is not a sandbox and does not replace agent permission controls. Do not expose
its socket through a network proxy or mount it into an untrusted container. A shared Unix account
is a shared fleet.

Saved state can include task text, working directories, agent output and image attachments.
Agent CLIs store additional data in their own locations. Old files outside Empire's state directory
are not migrated or permission-audited. Research studies live under `$XDG_STATE_HOME/empire/research` and may contain source snapshots
and model traces; treat them as private project data. The optional research runtime is installed
separately under `$XDG_DATA_HOME/empire/research`.

Agent CLIs contact their configured model providers. GitHub integration calls `gh`; Research
Station setup downloads its optional runtime. Review the relevant tool's permissions and data
handling before using it with confidential projects.

Optional advisor features are off by default. When configured, they send feature-specific task,
report or terminal excerpts to `PROVIDER_BASE_URL`. Common-pattern redaction is best effort;
review the README’s advisor sections before enabling them with confidential work.
