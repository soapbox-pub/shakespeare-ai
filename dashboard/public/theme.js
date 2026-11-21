// The theme, set before the page is drawn, so a prerendered page doesn't
// flash light for someone in dark mode until the app starts. AppProvider
// keeps it up to date after that.
try {
  var theme = (JSON.parse(localStorage.getItem('nostr:app-config') || '{}') || {}).theme;
  if (theme !== 'light' && theme !== 'dark') theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  document.documentElement.classList.add(theme);
} catch (e) {
  // Leave it to the app.
}
