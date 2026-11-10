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
- Review the final diff and public-facing metadata before changing visibility. No workflow here
  changes visibility, publishes a package, or uploads release binaries automatically.

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
