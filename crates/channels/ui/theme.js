// Runs before the page is drawn, so a chosen theme never flashes the other one first.
try { const t = window.localStorage.getItem("theme"); if (t === "light" || t === "dark") document.documentElement.dataset.theme = t; } catch { /* the system's choice stands */ }
