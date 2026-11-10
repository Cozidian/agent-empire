# Agent Empire user guide

Read the published guide at https://cozidian.github.io/agent-empire/.

The guide is a static site for GitHub Pages. Its eleven chapters cover setup, projects and
worktrees, native agents and shells, tasks and screenshots, attention and findings, GitHub,
research, controls, persistence/privacy, and troubleshooting.

## Build and preview

No package installation is needed: the builder uses Python 3's standard library.

```sh
python3 tools/build-docs.py
python3 -m http.server 8765 --bind 127.0.0.1 --directory target/docs
```

Open `http://127.0.0.1:8765/`. Output stays in ignored `target/docs`. All URLs are relative, so the
same files work at a GitHub project-site path such as `/agent-empire/` without a base-path rewrite.
The builder checks local links, heading anchors, image alt text and search-index targets.

Content is in the chapter HTML fragments beside this file. Navigation, ordering and search
summaries live in `navigation.json`; the shared layout is in `tools/build-docs.py`. CSS and the
small search/navigation enhancement live under `assets/`. The complete chapters and navigation
remain readable without JavaScript. There are no remote fonts, analytics or third-party scripts.

The theme follows the reader's system light/dark preference; the topbar toggle (or <kbd>t</kbd>)
overrides it and is remembered in `localStorage`. The starfield is CSS gradients, not images.
Keyboard shortcuts (press <kbd>?</kbd> on any page): `/` search, `n`/`p` chapters, `j`/`k`
sections, `g`/`G` top/bottom, `h` home, `m` menu, `t` theme, `Esc` close. Keep the `SHORTCUTS`
table in `guide.js` in step with the `KEYS` map.

When changing behavior, update the relevant chapter and README.md together. The README remains
the detailed repository manual; this guide provides a task-oriented introduction and reference.
Do not publish generated source listings, audit reports, local state or private screenshots.

## Publish with GitHub Pages

The `User guide` workflow builds and checks documentation on pull requests. Deployment only runs
from `main` after the repository variable `PAGES_ENABLED` is set to `true`.

After the repository's publication audit and history cleanup:

1. In repository **Settings → Pages**, select **GitHub Actions** as the build source.
2. Under **Settings → Secrets and variables → Actions → Variables**, create `PAGES_ENABLED=true`.
3. Push the reviewed changes, then run the **User guide** workflow or change a guide source file.
4. Check the workflow and `github-pages` deployment. The expected project URL is
   `https://cozidian.github.io/agent-empire/`.

The workflow publishes only `target/docs`. It does not change repository visibility. Public
repositories can use Pages on GitHub Free; hosting from a private repository depends on the
account's plan. This repository uses GitHub Actions deployment with `PAGES_ENABLED=true`.
