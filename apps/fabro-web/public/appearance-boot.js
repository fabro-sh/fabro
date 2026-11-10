// Themes the document before first paint. Loaded as a blocking external script from
// index.template.html, so `script-src 'self'` keeps working without an inline script or
// a CSP change. It reads one preference cookie, Fabro's own, and writes data-theme,
// data-mode, the dark class and color-scheme. It never writes the cookie.
//
// This repeats the allow-lists and resolution rules in app/lib/appearance.ts because it
// must run before the application bundle. appearance.test.ts runs this file against that
// module for every palette, mode and OS setting, so the two can't drift.
(function () {
  var COOKIE_NAME = "fabro-appearance";
  var DEFAULT_THEME = "brutalist";
  var DEFAULT_MODE = "light";
  var THEMES = ["signal", "readability", "command", "payline", "brutalist", "graphite", "lavender"];
  var MODES = ["light", "dark", "system"];
  // Experimental presets an earlier build saved. Each keeps only its light or dark intent.
  var LEGACY_PRESETS = [
    ["system", "system"],
    ["editorial-light", "light"],
    ["editorial-dark", "dark"],
    ["full-editorial", "light"],
    ["terminal", "dark"],
    ["sketch", "light"],
    ["consultant", "light"],
  ];

  var theme = DEFAULT_THEME;
  var mode = DEFAULT_MODE;

  function restore(value) {
    var current = /^v2\.([a-z]+)\.([a-z]+)$/.exec(value);
    if (current) {
      if (THEMES.indexOf(current[1]) !== -1 && MODES.indexOf(current[2]) !== -1) {
        theme = current[1];
        mode = current[2];
      }
      return;
    }
    var legacy = /^v1\.([a-z-]+)$/.exec(value);
    if (!legacy) return;
    for (var i = 0; i < LEGACY_PRESETS.length; i += 1) {
      if (LEGACY_PRESETS[i][0] === legacy[1]) mode = LEGACY_PRESETS[i][1];
    }
  }

  try {
    var parts = (document.cookie || "").split(";");
    var values = [];
    for (var i = 0; i < parts.length; i += 1) {
      var separator = parts[i].indexOf("=");
      if (separator < 0) continue;
      if (parts[i].slice(0, separator).trim() === COOKIE_NAME) {
        values.push(parts[i].slice(separator + 1).trim());
      }
    }
    // Two cookies of this name are ambiguous, so neither counts.
    if (values.length === 1) restore(values[0]);
  } catch (_error) {
    // A blocked cookie jar still gets the default look.
  }

  var systemDark = false;
  try {
    systemDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch (_error) {
    // No media queries: light.
  }

  var dark = mode === "dark" || (mode === "system" && systemDark);
  var root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.setAttribute("data-mode", mode);
  root.classList.toggle("dark", dark);
  root.style.colorScheme = dark ? "dark" : "light";
})();
