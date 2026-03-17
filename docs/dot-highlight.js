// Client-side DOT/Fabro syntax highlighting
// Workaround for Mintlify not loading custom TextMate grammars in production builds.
// Targets <code language="text"> blocks whose text starts with "digraph" or "strict digraph".
(function () {
  var KEYWORD = "color:#CF222E;--shiki-dark:#569CD6";
  var STRING = "color:#0A3069;--shiki-dark:#CE9178";
  var COMMENT = "color:#59636E;--shiki-dark:#6A9955";
  var ATTR = "color:#953800;--shiki-dark:#DCDCAA";
  var OPERATOR = "color:#CF222E;--shiki-dark:#D4D4D4";
  var SHAPE = "color:#0550AE;--shiki-dark:#9CDCFE";

  var rules = [
    // block comments
    { pattern: /\/\*[\s\S]*?\*\//g, style: COMMENT },
    // line comments
    { pattern: /\/\/.*$/gm, style: COMMENT },
    { pattern: /#(?![a-fA-F0-9]{3,8}\b).*$/gm, style: COMMENT },
    // strings
    { pattern: /"(?:[^"\\]|\\.)*"/g, style: STRING },
    // keywords
    {
      pattern: /\b(digraph|graph|subgraph|node|edge|strict)\b/g,
      style: KEYWORD,
    },
    // node/edge/graph attributes
    {
      pattern:
        /\b(label|shape|style|color|fillcolor|fontcolor|fontname|fontsize|bgcolor|rankdir|rank|ranksep|nodesep|arrowhead|arrowsize|arrowtail|dir|weight|constraint|splines|compound|concentrate|margin|pad|size|ratio|orientation|ordering|fixedsize|width|height|peripheries|regular|sides|skew|distortion|penwidth|pencolor|class|model|goal|model_stylesheet|reasoning_effort|prompt|tools|stage|include|checkpoint|human|parallel|sandbox|sandbox_image|hooks|run_config)\b/g,
      style: ATTR,
    },
    // shapes
    {
      pattern:
        /\b(box|polygon|ellipse|circle|point|egg|triangle|plaintext|diamond|trapezium|parallelogram|house|pentagon|hexagon|septagon|octagon|doublecircle|doubleoctagon|tripleoctagon|invtriangle|invtrapezium|invhouse|Mdiamond|Msquare|Mcircle|rect|rectangle|none|note|tab|folder|box3d|component)\b/g,
      style: SHAPE,
    },
    // operators
    { pattern: /->|--/g, style: OPERATOR },
  ];

  function escapeHtml(text) {
    return text
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  }

  function highlightLine(text) {
    // Build list of tokens with positions
    var tokens = [];
    rules.forEach(function (rule) {
      var re = new RegExp(rule.pattern.source, rule.pattern.flags);
      var m;
      while ((m = re.exec(text)) !== null) {
        tokens.push({ start: m.index, end: m.index + m[0].length, style: rule.style, text: m[0] });
      }
    });
    // Sort by start position, longer matches first for ties
    tokens.sort(function (a, b) {
      return a.start - b.start || b.end - a.end;
    });
    // Remove overlapping tokens (earlier/longer wins)
    var filtered = [];
    var lastEnd = 0;
    tokens.forEach(function (t) {
      if (t.start >= lastEnd) {
        filtered.push(t);
        lastEnd = t.end;
      }
    });
    // Build HTML
    var html = "";
    var pos = 0;
    filtered.forEach(function (t) {
      if (t.start > pos) {
        html += escapeHtml(text.slice(pos, t.start));
      }
      html += '<span style="' + t.style + '">' + escapeHtml(t.text) + "</span>";
      pos = t.end;
    });
    if (pos < text.length) {
      html += escapeHtml(text.slice(pos));
    }
    return html;
  }

  function highlightDotBlocks() {
    var codes = document.querySelectorAll('code[language="text"]');
    codes.forEach(function (code) {
      var raw = code.textContent || "";
      if (!/^\s*(strict\s+)?digraph\b/.test(raw)) return;
      // Update language attribute
      code.setAttribute("language", "dot");
      var lines = code.querySelectorAll("span.line");
      lines.forEach(function (line) {
        var inner = line.querySelector("span");
        if (!inner) return;
        var text = inner.textContent || "";
        inner.innerHTML = highlightLine(text);
      });
    });
  }

  // Run after a short delay to let React hydration complete
  function init() {
    highlightDotBlocks();
    // Re-run on SPA navigation (Mintlify uses Next.js)
    var observer = new MutationObserver(function () {
      highlightDotBlocks();
    });
    observer.observe(document.body, { childList: true, subtree: true });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", function () {
      setTimeout(init, 100);
    });
  } else {
    setTimeout(init, 100);
  }
})();
