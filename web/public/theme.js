// The theme chosen, before the page is drawn (no flash of the other one).
try {
  var t = localStorage.getItem('stratacut.theme');
  if (t === 'light' || (t === 'system' && matchMedia('(prefers-color-scheme: light)').matches)) document.documentElement.dataset.theme = 'light';
} catch (e) {}
