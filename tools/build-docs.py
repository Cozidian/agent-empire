#!/usr/bin/env python3
"""Build the static user guide with Python's standard library; no model or network calls."""
import argparse
import html
from html.parser import HTMLParser
import json
from pathlib import Path
import shutil
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'docs/guide'
REPO = 'https://github.com/Cozidian/agent-empire'


class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()
        self.links = []
        self.headings = []
        self.text = []
        self.heading = None

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'id' in attrs:
            if attrs['id'] in self.ids:
                raise ValueError(f"Duplicate id: {attrs['id']}")
            self.ids.add(attrs['id'])
        if tag == 'h2':
            if 'id' not in attrs:
                raise ValueError('Every h2 needs an id for chapter navigation')
            self.heading = [attrs['id'], '']
        if tag in ('a', 'img', 'script', 'link'):
            url = attrs.get('href', attrs.get('src', ''))
            if url:
                self.links.append(url)
        if tag == 'img' and not attrs.get('alt'):
            raise ValueError('Documentation images need meaningful alt text')

    def handle_data(self, text):
        self.text.append(text)
        if self.heading is not None:
            self.heading[1] += text

    def handle_endtag(self, tag):
        if tag == 'h2' and self.heading is not None:
            self.headings.append(self.heading)
            self.heading = None


def parse(content):
    doc = Document()
    doc.feed(content)
    return doc


def build(output):
    pages = json.loads((SOURCE / 'navigation.json').read_text())
    slugs = [page['slug'] for page in pages]
    if len(slugs) != len(set(slugs)) or 'index' not in slugs:
        raise ValueError('Guide slugs must be unique and include index')
    output.mkdir(parents=True, exist_ok=True)
    shutil.copytree(SOURCE / 'assets', output / 'assets', dirs_exist_ok=True)
    (output / 'images').mkdir(exist_ok=True)
    for name in ('galaxy.png', 'workspace.png'):
        shutil.copyfile(ROOT / 'docs/images' / name, output / 'images' / name)
    (output / '.nojekyll').write_text('')
    search = []
    for index, page in enumerate(pages):
        content = (SOURCE / f"{page['slug']}.html").read_text()
        doc = parse(content)
        search.append({**page, 'url': f"{page['slug']}.html", 'text': ' '.join(' '.join(doc.text).split())})
        nav = []
        group = None
        for number, item in enumerate(pages):
            if item['group'] != group:
                group = item['group']
                nav.append(f'<p class="nav-group">{html.escape(group)}</p>')
            current = ' aria-current="page"' if item['slug'] == page['slug'] else ''
            nav.append(f'<a href="{item["slug"]}.html"{current}><span class="chapter-number">{number:02}</span>{html.escape(item["title"])}</a>')
        toc = ''.join(f'<a href="#{anchor}">{html.escape(title)}</a>' for anchor, title in doc.headings)
        previous = pages[index - 1] if index else None
        following = pages[index + 1] if index + 1 < len(pages) else None
        pagination = ''.join(f'<a href="{item["slug"]}.html"><span>{label}</span>{html.escape(item["title"])}</a>' for label, item in [('Previous chapter', previous), ('Next chapter', following)] if item)
        title = html.escape(page['title'])
        description = html.escape(page['description'])
        document = f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · Agent Empire</title><meta name="description" content="{description}">
<meta name="color-scheme" content="light"><meta name="theme-color" content="#0c1720">
<link rel="stylesheet" href="assets/guide.css"><script src="assets/guide.js" defer></script></head>
<body><a class="skip" href="#main">Skip to content</a>
<aside class="sidebar" id="chapter-menu"><a class="brand" href="index.html"><span class="brand-mark" aria-hidden="true">✦</span><span>AGENT<br>EMPIRE</span></a>
<button class="menu-toggle" id="close-menu" type="button">Close menu</button>
<p class="edition">FLIGHT MANUAL / 0.1</p><nav class="chapter-nav" aria-label="User guide chapters">{''.join(nav)}</nav>
<div class="sidebar-footer"><a href="{REPO}">GitHub ↗</a><a href="{REPO}/blob/main/LICENSE">MIT license</a></div></aside>
<button class="scrim" type="button" tabindex="-1" aria-label="Close chapter menu"></button>
<div class="shell"><header class="topbar"><button class="menu-toggle" id="menu-toggle" type="button" aria-expanded="false" aria-controls="chapter-menu">☰ Menu</button>
<div class="breadcrumb">USER GUIDE <span>/</span> {html.escape(page['group'])}</div>
<div class="search-wrap" hidden><div class="search-box" role="search"><span aria-hidden="true">⌕</span><input id="guide-search" type="search" placeholder="Search the guide" aria-label="Search the user guide" aria-controls="search-results" autocomplete="off"><kbd aria-hidden="true">/</kbd></div><div class="search-results" id="search-results" aria-label="Matching chapters" hidden></div></div></header>
<div class="layout"><main class="article" id="main"><p class="eyebrow">{index:02} / {html.escape(page['group'])}</p><h1>{title}</h1>
{content}
<nav class="pagination" aria-label="Adjacent chapters">{pagination}</nav>
<footer class="page-footer"><span>AGENT EMPIRE · BUILT FOR YOUR TERMINAL</span><a href="{REPO}/blob/main/docs/guide/{page['slug']}.html">View chapter source ↗</a></footer></main>
<nav class="toc" aria-label="On this page"><p>ON THIS PAGE</p>{toc}</nav></div></div></body></html>
'''
        (output / f"{page['slug']}.html").write_text(document)
    (output / 'search-index.json').write_text(json.dumps(search, ensure_ascii=False))
    validate(output)
    print(f'Built and checked {len(pages)} chapters in {output}')


def validate(output):
    documents = {path.resolve(): parse(path.read_text()) for path in output.glob('*.html')}
    for path, doc in documents.items():
        for link in doc.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                continue
            target = (path.parent / unquote(url.path)).resolve() if url.path else path
            if not target.is_relative_to(output.resolve()) or not target.is_file():
                raise ValueError(f'{path.name}: missing or out-of-site link {link}')
            if url.fragment and target in documents and unquote(url.fragment) not in documents[target].ids:
                raise ValueError(f'{path.name}: missing anchor {link}')
    index = json.loads((output / 'search-index.json').read_text())
    for page in index:
        if not (output / page['url']).is_file() or not page['text']:
            raise ValueError('Search index has an empty or missing chapter')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/docs')
    args = parser.parse_args()
    build(args.output.resolve())
