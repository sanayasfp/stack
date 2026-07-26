
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
