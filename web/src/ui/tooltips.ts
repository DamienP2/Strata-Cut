/*
 * Drawn tooltips: any element with a `title` shows it in a card that follows
 * the pointer (the browser's own tooltip never appears). Write `title={t('…')}`.
 */

const DELAY = 380;
let installed = false;

export function installTooltips() {
  if (installed) return;
  installed = true;
  const tip = document.createElement('div');
  tip.id = 'tip';
  tip.className = 'tip';
  tip.setAttribute('role', 'tooltip');
  document.body.appendChild(tip);

  let target: HTMLElement | null = null;
  let timer = 0;
  let x = 0;
  let y = 0;

  const place = () => {
    const w = tip.offsetWidth;
    const h = tip.offsetHeight;
    let left = x + 14;
    let top = y + 18;
    if (left + w > innerWidth - 8) left = Math.max(8, x - 14 - w);
    if (top + h > innerHeight - 8) top = Math.max(8, y - 12 - h);
    tip.style.transform = `translate(${Math.round(left)}px, ${Math.round(top)}px)`;
  };

  const hide = () => {
    clearTimeout(timer);
    timer = 0;
    target = null;
    tip.classList.remove('on');
  };

  /** Moves `title` into `data-tip` so the browser no longer shows its own. */
  const adopt = (el: HTMLElement) => {
    const title = el.getAttribute('title');
    if (title !== null) {
      el.removeAttribute('title');
      if (title) el.dataset.tip = title;
      else delete el.dataset.tip;
      if (!el.getAttribute('aria-label') && !el.textContent?.trim()) el.setAttribute('aria-label', title);
    }
  };

  document.addEventListener('pointerover', (e) => {
    if (e.pointerType === 'touch') return;
    const el = (e.target as Element | null)?.closest?.('[title], [data-tip]') as HTMLElement | null;
    if (!el) return hide();
    adopt(el);
    if (el === target) return;
    hide();
    if (!el.dataset.tip) return;
    target = el;
    x = e.clientX;
    y = e.clientY;
    timer = window.setTimeout(() => {
      if (!target || !target.isConnected || !target.dataset.tip) return;
      tip.textContent = target.dataset.tip;
      place();
      tip.classList.add('on');
    }, DELAY);
  });
  document.addEventListener('pointermove', (e) => {
    if (!target) return;
    x = e.clientX;
    y = e.clientY;
    if (tip.classList.contains('on')) place();
  });
  document.addEventListener('pointerout', (e) => {
    if (!target) return;
    const to = e.relatedTarget as Node | null;
    if (!to || !target.contains(to)) hide();
  });
  for (const ev of ['pointerdown', 'wheel', 'keydown'] as const) document.addEventListener(ev, hide, { capture: true, passive: true });
  addEventListener('blur', hide);
}
