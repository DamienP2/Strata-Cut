/*
 * One kind of tooltip in the whole interface: the one that follows the pointer (FollowTip,
 * drawn from `data-tip`). An element written with `title={t('…')}` has its title moved into
 * `data-tip` as soon as it is in the page (or its title changes), so that the browser's own
 * tooltip never shows next to it.
 */

let installed = false;

/** Moves `title` into `data-tip` so the browser no longer shows its own. */
function adopt(el: Element) {
  const title = el.getAttribute('title');
  if (title === null) return;
  el.removeAttribute('title');
  const h = el as HTMLElement;
  if (title) h.dataset.tip = title;
  else delete h.dataset.tip;
  if (!el.getAttribute('aria-label') && !el.textContent?.trim()) el.setAttribute('aria-label', title);
}

export function installTooltips() {
  if (installed) return;
  installed = true;
  // A `title` set again (React draws an element anew, or changes its text) or on an element just
  // added is moved at once.
  const watch = new MutationObserver((records) => {
    for (const r of records) {
      if (r.type === 'attributes') adopt(r.target as Element);
      else
        for (const n of r.addedNodes) {
          if (!(n instanceof Element)) continue;
          adopt(n);
          n.querySelectorAll('[title]').forEach(adopt);
        }
    }
  });
  watch.observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['title'] });
  document.querySelectorAll('[title]').forEach(adopt);
  // Whatever the watch would not have seen yet: under the pointer.
  document.addEventListener('pointerover', (e) => {
    const el = (e.target as Element | null)?.closest?.('[title]');
    if (el) adopt(el);
  });
}
