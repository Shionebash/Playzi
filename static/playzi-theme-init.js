/* ─────────────────────────────────────────────────────────
   playzi-theme-init.js  ·  Add to index.html before app.js
   Handles: color preset persistence + color picker clicks
   ───────────────────────────────────────────────────────── */
(function () {
  /* Restore saved preferences immediately (before paint) */
  var color = localStorage.getItem("playzi-color") || "pink";
  var theme = localStorage.getItem("playzi-theme") || "dark";
  document.documentElement.dataset.color = color;
  document.documentElement.dataset.theme = theme;

  function syncDots() {
    var active = document.documentElement.dataset.color;
    document.querySelectorAll(".color-dot").forEach(function (d) {
      d.classList.toggle("active", d.dataset.colorPreset === active);
    });
  }

  /* Sync once DOM is ready */
  document.addEventListener("DOMContentLoaded", syncDots);

  /* Handle color dot clicks (delegated — works even if dots added later) */
  document.addEventListener("click", function (ev) {
    var dot = ev.target.closest("[data-color-preset]");
    if (!dot) return;
    var color = dot.dataset.colorPreset;
    document.documentElement.dataset.color = color;
    localStorage.setItem("playzi-color", color);
    syncDots();
  });
})();
