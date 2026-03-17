import { instance } from "@viz-js/viz";

let vizInstance: Awaited<ReturnType<typeof instance>> | undefined;

async function getViz() {
  if (!vizInstance) {
    vizInstance = await instance();
  }
  return vizInstance;
}

/**
 * Renders a Graphviz DOT string to an SVG string, styled to match the site's
 * teal-on-navy palette. The returned SVG has no fixed dimensions (uses viewBox)
 * so it can be sized by its container.
 */
export async function renderWorkflow(dot: string): Promise<string> {
  const viz = await getViz();

  const styledDot = injectGraphStyle(dot);
  let svg = viz.renderString(styledDot, { format: "svg", engine: "dot" });

  // Make SVG responsive: remove fixed width/height, keep viewBox
  svg = svg.replace(/\s*width="[^"]*"/, "");
  svg = svg.replace(/\s*height="[^"]*"/, "");

  return svg;
}

/**
 * Injects graph-level styling attributes into the DOT string so the rendered
 * SVG uses the site's color palette without post-processing.
 */
function injectGraphStyle(dot: string): string {
  const styleBlock = `
    bgcolor="transparent"
    node [
      fontname="Space Grotesk"
      fontsize=13
      fontcolor="#e8edf3"
      style="filled"
      fillcolor="#141c2f"
      color="#357f9e"
      penwidth=1.5
      shape=box
      margin="0.15,0.1"
    ]
    edge [
      color="#4b5768"
      fontname="DM Sans"
      fontsize=10
      fontcolor="#a8b5c5"
      arrowsize=0.7
      penwidth=1.2
    ]
  `;

  // Insert style after the opening brace of the digraph
  return dot.replace(/\{/, `{\n${styleBlock}\n`);
}
