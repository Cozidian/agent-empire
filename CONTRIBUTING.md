# Contributing to Agent Empire

Start with an issue describing the workflow you want to improve, or send a focused pull request.
Screenshots and clear reproduction steps are especially useful for terminal bugs. Use fictional
projects and redact prompts, paths and account information before sharing a capture.

Read [AGENTS.md](AGENTS.md) for the code map and conventions. The project deliberately does not
use rustfmt's default layout; match the surrounding code and avoid repository-wide formatting.
Keep [README.md](README.md), the user manual, and the relevant [user guide chapter](docs/guide/README.md) in step with behavior and shortcut changes.
Run `python3 tools/build-docs.py` to validate the static guide after documentation changes.

## Local checks

Use the Linux build prerequisites in the README, then run:

```sh
cargo test --locked
cargo build --release --locked
```

The research integration tests additionally need Python 3.12 or 3.13 and the pinned runtime:

```sh
python3 -m venv /tmp/empire-research-tests
/tmp/empire-research-tests/bin/pip install 'git+https://github.com/google-research/harness2.git@6868817518876de8fd170c6a8354daca273b6191'
/tmp/empire-research-tests/bin/python -m unittest discover -s research -v
```

These tests use a model test double and make no paid model calls. CI runs both suites and scans
history and the committed source with Gitleaks. The Python runtime pin matches `src/research.rs`
and `research/runner.py`; update them together.

For a manual run, isolate both the socket and state from your real fleet:

```sh
EMPIRE_SOCK=/tmp/empire-dev.sock XDG_STATE_HOME=/tmp/empire-dev ./target/release/empire
```

Do not stop the real daemon during development. Its sessions may include another person's work
or the agent helping you. Only stop a test daemon using the same isolated variables.

For footer changes, follow the [terminal footer contract](.agents/skills/terminal-footer-contract/SKILL.md).
For documentation images, see [the capture recipe](docs/images/README.md).

Keep credentials, local agent state, model transcripts and real project screenshots out of commits.
Report security problems using [SECURITY.md](SECURITY.md).
