# Agent Empire backlog

Agreed direction: Empire coordinates projects, discoveries and outcomes. Each agent keeps its own reasoning, tools, context management and execution.

## Anomalies — implemented (2026-11-01)

- Persistent findings with evidence, source, world/system, discovery and observation times, and lifecycle history.
- Planet markers and system totals; a separate browser with location, kind, status and text filters.
- Investigate through an editable scout mission; defer, dismiss, resolve and reopen findings.
- Reliable survey observations first: failing PR checks, merge conflicts and uncrewed worktree moons. Explicit scout reports are observations, not automatically verified facts.
- Repeated observations update the same finding; dismissals stick; unavailable data never proves recovery.
- Discovery and resolution history are the first foundation for the chronicle.
- Delivered: `A` browser, map/card counts, Commander entries, scout mission drafts and `empire anomaly` reports. See README for controls and persistence.
- Verified with lifecycle, concurrent storage, survey and UI tests, a release build and an isolated CLI smoke test.

## Sectors — implemented (2026-11-01)

- Group worlds into a product or initiative independently of filesystem layout.
- First case: a Kiplat sector containing runtime, frontend and Bifrost.
- `V` editor: create, rename and dissolve groups; toggle charted worlds or add an existing folder.
- Include member moons in combined ship, mission and anomaly totals.
- Focus/highlight a sector on the map and open its scoped anomalies; preserve the wider galaxy.
- Keep available members charted and persist groups across restarts.
- Normal-map zoom now steps through systems → sectors → planetary detail. Named, tinted regions reveal local member worlds; select a region to zoom into it. Cross-system groups share their name and colour.
- Verified: 97 tests passed (1 live API test ignored), release build, and an isolated terminal smoke test creating a three-world sector and reopening it.
- Sector zoom verified: 101 tests passed (1 live API test ignored), release build, and a rendered fixture preview; covers local/cross-system regions, picking and Enter transitions.

## Hyperlanes — implemented (2026-11-01)

- `H` editor: create, edit and remove named, directional relationships between worlds.
- `h` dependency overlay: curved arrows, clickable labels and connections, endpoint inspection and map focus.
- Connections can cross sectors and systems; moons use their host when creating a link.
- Locked, atomic persistence; unavailable endpoints stay recorded and available folders stay charted.
- Verified: 110 tests passed (1 live API test ignored), release build, rendered map/editor previews, and an isolated terminal test of creation, rename, overlay, restart persistence and removal.
- Future: automatic discovery, plus optional overlays for anomalies, campaigns and release readiness.

## Campaigns — next

- Shared objective, linked missions, acceptance criteria and handoff evidence across worlds.
- Begin with manual handoffs; allow one agent to carry an entire campaign.
- Optional scout → accepted plan → builder on a moon → inspector → reviewed docking sequence.
- Automate only chosen transitions later; do not add a competing agent reasoning loop.
- Distinguish reported-finished, verified and accepted outcomes before automatic progression.

## Ship designs and refits

- Save reusable species, role, standing instructions, skills and workflow configurations.
- Apply designs to deployed ships as refits; show pending versus effective equipment.
- Respect native harness support: instruction delivery, reload or deliberate session handoff as appropriate.
- Distinguish planet-wide resources from equipment assigned to one ship.

## Empire chronicle

- Durable milestones linked to missions, findings, decisions and evidence.
- Preserve why decisions were made, from explicit decisions or attributed reports.
- Grow the anomaly lifecycle history into campaign, release and project history.

## Soundtracks and swappable resources

- Separate compositions from the audio engine.
- Add several original synthesized tracks, gentle transitions, selection and shuffle.
- Consider optional user audio files; keep the built-in experience self-contained.
- Introduce a general plugin system only when concrete extension needs justify it.

## Parked

- Planet specialization/designations: revisit only when a useful workflow calls for them.
- Synthetic currencies, mandatory upkeep and random disasters.
