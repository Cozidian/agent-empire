# Preparing a public release

Public visibility exposes more than the current branch. Review reachable history, old branches
and tags, pull requests, issue bodies and comments, release attachments, Actions logs/artifacts,
wikis and discussion threads before changing visibility. Deleting a file in a new commit does
not remove earlier copies. Closing an issue does not hide its contents.

## Before the first public push

- Choose whether to publish cleaned history or a fresh source-only repository. Keep the original
  private backup until the release is verified. Rewriting history changes commit IDs and requires
  coordination with everyone using the old branches; a mailmap alone does not erase raw metadata.
- Audit author/committer identities, commit messages, old integrations, fixture names, URLs and
  screenshots as well as credentials. Rotate any real credential found before removing it.
- Run Gitleaks against all fetched refs and the release tree, with redacted reports. Review results
  manually; a scanner passing is evidence, not a guarantee. Do not commit audit reports containing
  private history or secrets.
- Confirm the MIT license covers the code being released, including any employer-owned work.
- Run the Rust and research tests in [CONTRIBUTING.md](../CONTRIBUTING.md), build with `--locked`,
  and inspect the documentation images. CI must pass on GitHub after the prepared changes are pushed.
- Enable private vulnerability reporting, available secret scanning/push protection, and branch
  protection in the repository settings. Check that the reporting link in SECURITY.md works.
- Review the final diff and public-facing metadata before changing visibility. The release workflow
  publishes binary archives when a version tag is pushed; no workflow changes repository visibility
  or publishes to crates.io.

Example local scan with Gitleaks installed:

```sh
gitleaks git --redact --log-opts=--all .
```

For a committed source-only scan, export `git archive HEAD` to an empty temporary directory and
run `gitleaks dir --redact` there. Also scan any new or modified files included in the proposed
release; an archive of HEAD excludes uncommitted work. GitHub-only surfaces need their own review.

## Documentation hosting

The static [user guide](guide/README.md) is built by `python3 tools/build-docs.py`. It contains
no account data or live fleet access. The GitHub Pages workflow deploys only the generated
`target/docs` directory, with deployment disabled until `PAGES_ENABLED=true` is set.

After history and GitHub metadata cleanup, enable Pages with GitHub Actions in repository
settings, set the variable, and run the User guide workflow. Verify its deployment before
advertising `https://cozidian.github.io/agent-empire/`. The source repository's visibility is a
separate setting; enabling the documentation workflow does not change it.

Old pull-request refs and cached commit views can survive a history rewrite. Follow
[GitHub's sensitive-data removal guidance](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)
and resolve those retained surfaces before making the existing repository public. An edited
issue may also retain its prior body in edit history; replacing visible text alone is not a
complete deletion of that history.

## Version tags and binary releases

Use stable tags of the form `vMAJOR.MINOR.PATCH`, matching `Cargo.toml` and the root package in
`Cargo.lock`. Do not move a released tag or replace its assets: bump the version for a correction.
Update `docs/release-notes.md` for each release, including its pinned install command.

1. Update the package version and lockfile, release notes, README and guide as needed.
2. Run tests, `python3 tools/test-install.py` and `python3 tools/build-docs.py`.
3. Commit and push the release preparation to `main`; check Fleet checks passes.
4. Tag that reviewed commit and push just the tag:

   ```sh
   git tag -a v0.1.0 -m 'Launch Empire v0.1.0'
   git push origin v0.1.0
   ```

`Release fleet` runs Fleet checks, builds and tests natively on Ubuntu 22.04 for x86-64 and
ARM64, strips the binaries, packages the license and manual, and exercises the installer.
Only after both builds succeed does it create the GitHub Release with two archives,
`install.sh` and `SHA256SUMS`. The tag must match the package version. Repository write permission
is confined to the publishing job. Published assets are not overwritten on a rerun.

A manual workflow run on `main` tests packaging and saves Actions artifacts without publishing
or creating a tag. Run on an existing tag to retry a failed release. If publication left a draft,
inspect its assets before completing or deleting that draft; do not retag a published version.

The binaries require glibc 2.35+ and the ALSA shared library. They embed the Research Station
runner, so no source checkout is needed. Agent CLIs, Git, optional `gh`, and the opt-in research
runtime are installed separately. The installer uses an atomic rename and does not start,
stop or upgrade the running daemon. Private releases require authenticated access; making a
release does not make the repository public.
