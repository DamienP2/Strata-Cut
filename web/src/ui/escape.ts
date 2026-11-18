/*
 * Escape anywhere closes the dialog on top: it "presses" the topmost
 * `.modal-back`, which closes on pointerdown (a click on the backdrop),
 * unless something else already handled the key.
 */

let installed = false;

/** The topmost backdrop: highest z-index, then last in the page. */
function topBackdrop(): HTMLElement | null {
  let best: HTMLElement | null = null;
  let bestZ = -Infinity;
  for (const el of document.querySelectorAll<HTMLElement>('.modal-back')) {
    const z = Number(getComputedStyle(el).zIndex) || 0;
    if (z >= bestZ) {
      best = el;
      bestZ = z;
    }
  }
  return best;
}

export function installEscape() {
  if (installed) return;
  installed = true;
  addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    const back = topBackdrop();
    if (!back) return;
    e.preventDefault();
    back.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true }));
  });
}
