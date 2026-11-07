---
name: terminal-footer-contract
description: Verify terminal footer changes for clipped hints and missing essential actions at 40, 80, and 140 columns.
---

# Terminal footer verification

Derive expected key/action pairs from the request and input behavior, independently of the new layout. In this project, reconcile `src/game.rs::handle` and `App::activate` with `draw.rs::help` and the README. Preserve existing bindings when hiding secondary hints.

Exercise the production renderer at **40, 80, and 140 columns**, at a fixed adequate height. Cover the footer's distinct selection cases: project, HOME, agent, Research, GitHub, and no contextual action. Their existing Enter labels are project, home, terminal, research, and pull requests respectively; the default has none. Include optional sound suffix states that affect fitting when feasible; identify unchecked states.

Check two properties separately:

- **Integrity:** every visible hint has a complete key and understandable action, with no partial words, orphan separators, or overlap, including at the right edge.
- **Coverage:** explicitly require `? help`, `q leave`, and the selected item's opening action. Clear abbreviations are acceptable. Check `Tab` next-alert visibility ahead of secondary navigation when space permits. Complete remaining hints do not excuse a missing essential action.

`draw.rs::seg` truncates character by character; `gfx.rs::Fb::put_bg` silently drops out-of-bounds writes. A width bound or no-panic assertion proves neither property. Render into `Fb`, build `Lay` from it, and inspect the actual footer row at `l.rows - 1`: `fb.txt[row * fb.cols + col]`, using `Tc.ch` or a space for `None`. Existing game tests use `handle()`, `Client::isolated()`, and `draw::frame` without the real daemon.

Associate assertions or captured rows with width and selection; delimit printed rows to expose their edges. Confirm a focused regression catches the original symptom, then passes on the final change. Inspect the final diff for shortcut preservation. Keep evidence in test output and the requested report; preserve the requested deliverable format.

Report commands actually run, observed results, and uncovered widths/states. Distinguish rendered-cell verification from compilation, interactive terminal use, and audio checks. If rendering cannot execute, report it as unverified; source inspection or test counts alone cannot establish a visual pass.
