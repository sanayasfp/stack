// Shared chrome for every page: theme toggle (the original deliberate
// exception to "no JavaScript" in this design system) plus copy-to-clipboard
// on terminal blocks, added for the same reason -- both are small, page-wide
// behaviors that don't belong duplicated inline on every page.

// Theme toggle. Defaults to the OS preference (handled entirely by CSS media
// queries; no attribute is set on first load) and only writes data-theme
// once the visitor actually clicks, so a stored choice can override the OS
// setting on return visits.
(() => {
  const root = document.documentElement;
  const btn = document.getElementById('theme-toggle');
  if (!btn) return;
  const stored = localStorage.getItem('stack-theme');
  if (stored === 'dark' || stored === 'light') root.setAttribute('data-theme', stored);
  btn.addEventListener('click', () => {
    const current = root.getAttribute('data-theme')
      || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
    const next = current === 'dark' ? 'light' : 'dark';
    root.setAttribute('data-theme', next);
    localStorage.setItem('stack-theme', next);
  });
})();

// Copy-to-clipboard on every .term block. Copies only the command lines --
// those starting with the "$ " prompt -- stripped of the prompt itself, so
// pasting into a real shell doesn't drag along stack's own output or a
// literal "$". Falls back to the whole block's text for blocks with no
// prompt line at all (a bare stack.toml snippet, for example).
(() => {
  document.querySelectorAll('.term').forEach((term) => {
    const body = term.querySelector('.term-body');
    const bar = term.querySelector('.term-bar');
    if (!body || !bar) return;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'term-copy';
    btn.setAttribute('aria-label', 'Copy command');
    btn.textContent = 'Copy';
    bar.appendChild(btn);
    btn.addEventListener('click', () => {
      const lines = body.textContent.split('\n');
      const commandLines = lines
        .filter((l) => l.trim().startsWith('$ '))
        .map((l) => l.trim().slice(2));
      const text = commandLines.length ? commandLines.join('\n') : body.textContent.trim();
      navigator.clipboard.writeText(text).then(() => {
        const original = btn.textContent;
        btn.textContent = 'Copied';
        btn.disabled = true;
        setTimeout(() => { btn.textContent = original; btn.disabled = false; }, 1400);
      });
    });
  });
})();

// Generic copy-input button: a [data-copy-target] button copies the value of
// the element with that id -- used by the quick-start command box, which is
// a readonly <input> rather than a .term block or .installline.
(() => {
  document.querySelectorAll('[data-copy-target]').forEach((btn) => {
    const target = document.getElementById(btn.getAttribute('data-copy-target'));
    if (!target) return;
    btn.addEventListener('click', () => {
      const text = 'value' in target ? target.value : target.textContent;
      navigator.clipboard.writeText(text).then(() => {
        const original = btn.textContent;
        btn.textContent = 'Copied';
        btn.disabled = true;
        setTimeout(() => { btn.textContent = original; btn.disabled = false; }, 1400);
      });
    });
  });
})();

// Same idea for the compact hero install line (.installline) -- not a full
// .term block, just a single command, but it's the very first command a
// visitor sees and it needs to be copyable too.
(() => {
  document.querySelectorAll('.installline').forEach((line) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'installline-copy';
    btn.setAttribute('aria-label', 'Copy command');
    btn.textContent = 'Copy';
    line.appendChild(btn);
    btn.addEventListener('click', () => {
      const text = line.textContent.trim().replace(/^\$\s*/, '');
      navigator.clipboard.writeText(text).then(() => {
        const original = btn.textContent;
        btn.textContent = 'Copied';
        btn.disabled = true;
        setTimeout(() => { btn.textContent = original; btn.disabled = false; }, 1400);
      });
    });
  });
})();
