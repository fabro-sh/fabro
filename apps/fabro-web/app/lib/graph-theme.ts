export interface GraphTheme {
  fontcolor: string;
  edgeColor: string;
  nodeFill: string;
  nodeText: string;
  startFill: string;
  startBorder: string;
  startText: string;
  gateFill: string;
  gateBorder: string;
  gateText: string;
  completedFill: string;
  completedBorder: string;
  completedText: string;
  runningFill: string;
  runningBorder: string;
  runningText: string;
  runningPulseFill: string;
  runningPulseStroke: string;
  failedFill: string;
  failedBorder: string;
  failedText: string;
}

export const graphTheme: GraphTheme = {
  fontcolor: "#5a7a94",
  edgeColor: "#2a3f52",
  nodeFill: "#1a2b3c",
  nodeText: "#c6d4e0",
  startFill: "#0d4f4f",
  startBorder: "#14b8a6",
  startText: "#5eead4",
  gateFill: "#1a2030",
  gateBorder: "#f59e0b",
  gateText: "#fbbf24",
  completedFill: "#0a2a20",
  completedBorder: "#34d399",
  completedText: "#6ee7b7",
  runningFill: "#0d3a3a",
  runningBorder: "#14b8a6",
  runningText: "#5eead4",
  runningPulseFill: "#134e4a",
  runningPulseStroke: "#5eead4",
  failedFill: "#2a1215",
  failedBorder: "#f87171",
  failedText: "#fca5a5",
};
