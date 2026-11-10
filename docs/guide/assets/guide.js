'use strict';
document.body.classList.add('js-enabled');
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
  if (event.key === 'ArrowDown' && !results.hidden) {
    const first = results.querySelector('a');
    if (first) { event.preventDefault(); first.focus(); }
  }
});
document.addEventListener('click', event => {
  if (!event.target.closest('.search-wrap')) results.hidden = true;
});
document.addEventListener('keydown', event => {
  if (event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey && !event.target.closest('input,textarea,[contenteditable]')) {
    event.preventDefault(); search.focus();
  }
  if (event.key === 'Escape') {
    if (menuOpen) setMenu(false);
    if (!results.hidden) { results.hidden = true; search.focus(); }
  }
  if (event.key === 'Tab' && menuOpen) {
    const items = [...sidebar.querySelectorAll('a,button')].filter(el => el.getClientRects().length);
    const first = items[0], last = items[items.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  }
});
fetch('search-index.json').then(response => {
  if (!response.ok) throw new Error('Search index unavailable');
  return response.json();
}).then(data => { pages = data; if (document.activeElement === search) renderSearch(); })
  .catch(() => { failed = true; if (document.activeElement === search) renderSearch(); });
