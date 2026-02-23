import { useState } from "react";
import "./index.css";
import { StartForm } from "./StartForm";
import { PipelineDashboard } from "./PipelineDashboard";

export function App() {
  const [pipelineId, setPipelineId] = useState<string | null>(null);

  return (
    <div className="app">
      <div className="app-header">
        <h1>Attractor</h1>
        <span>Pipeline Dashboard</span>
      </div>
      {pipelineId ? (
        <PipelineDashboard
          pipelineId={pipelineId}
          onBack={() => setPipelineId(null)}
        />
      ) : (
        <StartForm onStart={setPipelineId} />
      )}
    </div>
  );
}

export default App;
