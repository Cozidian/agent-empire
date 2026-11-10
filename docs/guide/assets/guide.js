'use strict';
document.body.classList.add('js-enabled');
const root = document.documentElement;
const sidebar = document.querySelector('.sidebar');
const menu = document.querySelector('#menu-toggle');
const closeMenu = document.querySelector('#close-menu');
const scrim = document.querySelector('.scrim');
const narrow = window.matchMedia('(max-width: 680px)');
let menuOpen = false;
function setMenu(open, moveFocus = true) {
  menuOpen = open && narrow.matches;
  sidebar.classList.toggle('is-open', menuOpen);
  scrim.classList.toggle('is-open', menuOpen);
  menu.setAttribute('aria-expanded', String(menuOpen));
  sidebar.inert = narrow.matches && !menuOpen;
  if (moveFocus) (menuOpen ? closeMenu : menu).focus();
}
menu.addEventListener('click', () => setMenu(!menuOpen));
closeMenu.addEventListener('click', () => setMenu(false));
scrim.addEventListener('click', () => setMenu(false));
narrow.addEventListener('change', () => setMenu(false, false));
setMenu(false, false);

// theme: the system preference until the reader picks one (the head script applies a saved pick before paint)
const THEME_KEY = 'empire-guide-theme';
const themeButton = document.querySelector('#theme-toggle');
const prefersDark = window.matchMedia('(prefers-color-scheme: dark)');
const theme = () => root.dataset.theme || (prefersDark.matches ? 'dark' : 'light');
function paintThemeButton() {
  const dark = theme() === 'dark';
  themeButton.textContent = dark ? '☼' : '☾\uFE0E';
  themeButton.setAttribute('aria-label', dark ? 'Switch to light mode' : 'Switch to dark mode');
  themeButton.title = (dark ? 'Light mode' : 'Dark mode') + ' (t)';
}
function toggleTheme() {
  root.dataset.theme = theme() === 'dark' ? 'light' : 'dark';
  try { localStorage.setItem(THEME_KEY, root.dataset.theme); } catch (e) { /* private window: the choice lasts for this page */ }
  paintThemeButton();
}
themeButton.hidden = false;
themeButton.addEventListener('click', toggleTheme);
prefersDark.addEventListener('change', paintThemeButton);
paintThemeButton();

const search = document.querySelector('#guide-search');
const results = document.querySelector('#search-results');
document.querySelector('.search-wrap').hidden = false;
let pages = null;
let failed = false;
function renderSearch() {
  const query = search.value.trim().toLocaleLowerCase();
  results.replaceChildren();
  if (!query) { results.hidden = true; return; }
  results.hidden = false;
  if (!pages) {
    const status = document.createElement('p');
    status.textContent = failed ? 'Search is unavailable. Browse the chapters in the menu.' : 'Loading the guide…';
    results.append(status);
    return;
  }
  const words = query.split(/\s+/);
  const matches = pages.map(page => {
    const title = page.title.toLocaleLowerCase();
    const text = page.text.toLocaleLowerCase();
    return {page, score: words.every(word => (title + ' ' + text).includes(word)) ? words.reduce((n, word) => n + (title.includes(word) ? 10 : 1), 0) : 0};
  }).filter(hit => hit.score).sort((a, b) => b.score - a.score).slice(0, 7);
  if (!matches.length) {
    const status = document.createElement('p');
    status.textContent = 'No matching chapters. Try “tasks”, “worktree” or “keyboard”.';
    results.append(status);
  }
  for (const {page} of matches) {
    const link = document.createElement('a');
    link.href = page.url;
    const title = document.createElement('strong'); title.textContent = page.title;
    const summary = document.createElement('small'); summary.textContent = page.description;
    link.append(title, summary); results.append(link);
  }
}
search.addEventListener('input', renderSearch);
search.addEventListener('focus', renderSearch);
search.addEventListener('keydown', event => {
  const first = results.hidden ? null : results.querySelector('a');
  if (event.key === 'ArrowDown' && first) { event.preventDefault(); first.focus(); }
  if (event.key === 'Enter' && first) { event.preventDefault(); location.href = first.href; }
});
results.addEventListener('keydown', event => {
  if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
  const links = [...results.querySelectorAll('a')];
  const at = links.indexOf(document.activeElement);
  event.preventDefault();
  if (event.key === 'ArrowUp' && at <= 0) search.focus();
  else (links[at + (event.key === 'ArrowDown' ? 1 : -1)] || links[at]).focus();
});
document.addEventListener('click', event => {
  if (!event.target.closest('.search-wrap')) results.hidden = true;
});

// sections: j/k jump between h2s, and the star map lights the one being read
const headings = [...document.querySelectorAll('.article h2[id]')];
const tocLinks = [...document.querySelectorAll('.toc a')];
const topOffset = () => parseFloat(getComputedStyle(root).scrollPaddingTop) || 0;
function jumpSection(step) {
  const line = topOffset() + 30;
  const target = step > 0 ? headings.find(h => h.getBoundingClientRect().top > line + 2) : headings.findLast(h => h.getBoundingClientRect().top < line - 30);
  if (!target) { if (step < 0) window.scrollTo({top: 0}); return; }
  target.scrollIntoView({block: 'start'});
  history.replaceState(null, '', '#' + target.id);
}
let spyQueued = false;
function spy() {
  spyQueued = false;
  const line = topOffset() + 40;
  let current = null;
  for (const h of headings) if (h.getBoundingClientRect().top <= line) current = h;
  if (window.innerHeight + window.scrollY >= root.scrollHeight - 2 && headings.length) current = headings[headings.length - 1];
  for (const link of tocLinks) {
    if (current && link.hash === '#' + current.id) link.setAttribute('aria-current', 'location');
    else link.removeAttribute('aria-current');
  }
}
window.addEventListener('scroll', () => { if (!spyQueued) { spyQueued = true; requestAnimationFrame(spy); } }, {passive: true});
spy();

// the shortcuts panel, opened with ? or the topbar button
const SHORTCUTS = [
  [['/'], 'Search the guide'],
  [['n', 'p'], 'Next / previous chapter'],
  [['j', 'k'], 'Next / previous section'],
  [['g', 'G'], 'Top / bottom of the page'],
  [['h'], 'Back to the welcome chapter'],
  [['m'], 'Chapter menu'],
  [['t'], 'Switch light / dark'],
  [['?'], 'Show these shortcuts'],
  [['Esc'], 'Close search, menu or this panel'],
];
const shortcuts = document.createElement('dialog');
shortcuts.className = 'shortcuts';
shortcuts.setAttribute('aria-labelledby', 'shortcuts-title');
{
  const title = document.createElement('h2'); title.id = 'shortcuts-title'; title.textContent = 'KEYBOARD SHORTCUTS';
  const table = document.createElement('table');
  const body = table.createTBody();
  for (const [keys, action] of SHORTCUTS) {
    const row = body.insertRow();
    const cell = row.insertCell();
    keys.forEach((key, i) => { if (i) cell.append(' / '); const kbd = document.createElement('kbd'); kbd.textContent = key; cell.append(kbd); });
    row.insertCell().textContent = action;
  }
  const foot = document.createElement('p');
  const hint = document.createElement('span'); hint.textContent = 'Keys work anywhere outside the search box.';
  const form = document.createElement('form'); form.method = 'dialog';
  const close = document.createElement('button'); close.className = 'tool-button'; close.textContent = 'Close';
  form.append(close); foot.append(hint, form);
  shortcuts.append(title, table, foot);
}
shortcuts.addEventListener('click', event => {
  const r = shortcuts.getBoundingClientRect();
  if (event.clientX < r.left || event.clientX > r.right || event.clientY < r.top || event.clientY > r.bottom) shortcuts.close();
});
document.body.append(shortcuts);
const shortcutsButton = document.querySelector('#shortcuts-button');
shortcutsButton.hidden = false;
shortcutsButton.addEventListener('click', () => shortcuts.showModal());

const follow = link => { if (link) location.href = link.href; };
const KEYS = {
  '/': () => search.focus(),
  '?': () => shortcuts.showModal(),
  n: () => follow(document.querySelector('.pagination a[rel=next]')),
  p: () => follow(document.querySelector('.pagination a[rel=prev]')),
  j: () => jumpSection(1),
  k: () => jumpSection(-1),
  g: () => window.scrollTo({top: 0}),
  G: () => window.scrollTo({top: root.scrollHeight}),
  h: () => follow(document.querySelector('.brand')),
  m: () => narrow.matches ? setMenu(!menuOpen) : sidebar.querySelector('[aria-current=page]')?.focus(),
  t: toggleTheme,
};
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') {
    if (menuOpen) setMenu(false);
    else if (!results.hidden) { results.hidden = true; search.focus(); }
    else if (document.activeElement === search) search.blur();
  }
  if (event.key === 'Tab' && menuOpen) {
    const items = [...sidebar.querySelectorAll('a,button')].filter(el => el.getClientRects().length);
    const first = items[0], last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  }
  if (event.ctrlKey || event.metaKey || event.altKey || event.isComposing || event.defaultPrevented) return;
  if (event.target.closest('input,textarea,select,[contenteditable]')) return;
  if (shortcuts.open) { if (event.key === '?') { event.preventDefault(); shortcuts.close(); } return; }
  const action = KEYS[event.key];
  if (action) { event.preventDefault(); action(); }
});
fetch('search-index.json').then(response => {
  if (!response.ok) throw new Error('Search index unavailable');
  return response.json();
}).then(data => { pages = data; if (document.activeElement === search) renderSearch(); })
  .catch(() => { failed = true; if (document.activeElement === search) renderSearch(); });
