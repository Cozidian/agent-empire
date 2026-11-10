# Documentation images

`galaxy.png` and `workspace.png` are captures of the production renderer with fictional projects,
not captures of a real user's fleet. No daemon, agents, audio or saved workspace are used.
The capture helper runs only as an explicitly ignored test.

Recreate them from the repository root with Rust, Python 3 and `rsvg-convert` (librsvg):

```sh
EMPIRE_CAPTURE_DIR=/tmp/empire-doc-captures cargo test capture_release_images -- --ignored
python3 tools/capture-docs.py /tmp/empire-doc-captures/galaxy.ansi /tmp/empire-galaxy.svg
python3 tools/capture-docs.py /tmp/empire-doc-captures/workspace.ansi /tmp/empire-workspace.svg
rsvg-convert /tmp/empire-galaxy.svg -o docs/images/galaxy.png
rsvg-convert /tmp/empire-workspace.svg -o docs/images/workspace.png
```

The helper preserves the ANSI output's terminal cells and colors. Octant glyphs are rendered as
geometry so captures do not depend on a Unicode 16 font. Text uses DejaVu Sans Mono or a monospace
fallback. The scene and animation time are fixed; the UI clock reflects capture time.
Review the images at README size after regenerating. Keep generated ANSI/SVG intermediates out
of the repository; only the PNGs are used in the README.
