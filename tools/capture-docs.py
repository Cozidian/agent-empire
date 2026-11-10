#!/usr/bin/env python3
"""Convert Empire's fixed ANSI captures to SVG (Python standard library only)."""
import argparse
import html
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[1]
OCTANTS = {chr(int(h, 16)): i for i, h in enumerate(re.findall(r"\\u\{([0-9A-Fa-f]+)\}", (ROOT / 'src/octant.rs').read_text()))}
CSI = re.compile(r'\x1b\[([0-9;?]*)([A-Za-z])')


def convert(source, target, cols=140, rows=44):
    cells = [[(' ', '#cbd5e1', '#080d17') for _ in range(cols)] for _ in range(rows)]
    data = source.read_text()
    x = y = pos = 0
    fg, bg = '#cbd5e1', '#080d17'
    while pos < len(data):
        match = CSI.match(data, pos)
        if match:
            args, cmd = match.groups()
            if cmd == 'H':
                y, x = [int(n) - 1 for n in args.split(';')]
            elif cmd == 'm':
                values = [int(n) for n in args.split(';') if n]
                i = 0
                while i < len(values):
                    if values[i] in (38, 48) and values[i + 1:i + 2] == [2]:
                        color = '#%02x%02x%02x' % tuple(values[i + 2:i + 5])
                        if values[i] == 38: fg = color
                        else: bg = color
                        i += 5
                    else:
                        i += 1
            pos = match.end()
            continue
        ch = data[pos]
        if 0 <= x < cols and 0 <= y < rows:
            cells[y][x] = ch, fg, bg
        x += 1
        pos += 1
    # Octants are geometry, so no font with Unicode 16 coverage is needed.
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{cols * 10}" height="{rows * 20}" viewBox="0 0 {cols * 10} {rows * 20}">',
           '<title>Agent Empire — fictional demo projects, rendered by the application</title>',
           '<g shape-rendering="crispEdges">']
    labels = []
    for y, row in enumerate(cells):
        for x, (ch, fg, bg) in enumerate(row):
            px, py = x * 10, y * 20
            out.append(f'<path fill="{bg}" d="M{px} {py}h10v20h-10z"/>')
            if ch in OCTANTS:
                bits = OCTANTS[ch]
                for n in range(8):
                    if bits & (1 << n):
                        out.append(f'<path fill="{fg}" d="M{px + n % 2 * 5} {py + n // 2 * 5}h5v5h-5z"/>')
            elif ch != ' ':
                labels.append(f'<text x="{px}" y="{py + 16}" fill="{fg}">{html.escape(ch)}</text>')
    out.extend(['</g>', '<g font-family="DejaVu Sans Mono,monospace" font-size="16">', *labels, '</g></svg>'])
    target.write_text('\n'.join(out))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=pathlib.Path)
    parser.add_argument('target', type=pathlib.Path)
    parser.add_argument('--rows', type=int, default=44)
    args = parser.parse_args()
    convert(args.source, args.target, rows=args.rows)
