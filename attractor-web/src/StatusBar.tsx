import { cancelPipeline, type PipelineStatus } from "./api";

interface StatusBarProps {
  id: string;
  status: PipelineStatus;
  error?: string;
}

export function StatusBar({ id, status, error }: StatusBarProps) {
  async function handleCancel() {
    try {
      await cancelPipeline(id);
    } catch {
      // status will update via polling
    }
  }

  return (
    <div className="status-bar">
      <span className={`status-badge ${status}`}>{status}</span>
      <span className="pipeline-id">{id}</span>
      {error && <span className="error-msg">{error}</span>}
      {status === "running" && (
        <button className="btn-danger" onClick={handleCancel}>
          Cancel
        </button>
      )}
    </div>
  );
}
