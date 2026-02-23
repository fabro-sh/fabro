import { useState, useCallback } from "react";
import { getQuestions, submitAnswer, type ApiQuestion } from "./api";
import { usePolling } from "./hooks";

interface QuestionPanelProps {
  pipelineId: string;
  active: boolean;
}

function QuestionCard({
  pipelineId,
  question,
}: {
  pipelineId: string;
  question: ApiQuestion;
}) {
  const [value, setValue] = useState("");
  const [submitting, setSubmitting] = useState(false);

  async function submit(answer: string) {
    setSubmitting(true);
    try {
      await submitAnswer(pipelineId, question.id, answer);
      setValue("");
    } catch {
      // question will remain visible for retry
    } finally {
      setSubmitting(false);
    }
  }

  if (question.question_type === "YesNo") {
    return (
      <div className="question-card">
        <div className="question-type">{question.question_type}</div>
        <div className="question-text">{question.text}</div>
        <div className="answer-row">
          <button
            className="btn-yes-no"
            disabled={submitting}
            onClick={() => submit("Yes")}
          >
            Yes
          </button>
          <button
            className="btn-yes-no"
            disabled={submitting}
            onClick={() => submit("No")}
          >
            No
          </button>
        </div>
      </div>
    );
  }

  if (question.question_type === "Confirmation") {
    return (
      <div className="question-card">
        <div className="question-type">{question.question_type}</div>
        <div className="question-text">{question.text}</div>
        <div className="answer-row">
          <button
            className="btn-answer"
            disabled={submitting}
            onClick={() => submit("Yes")}
          >
            Confirm
          </button>
          <button
            className="btn-yes-no"
            disabled={submitting}
            onClick={() => submit("No")}
          >
            Decline
          </button>
        </div>
      </div>
    );
  }

  // Freeform and MultipleChoice (fallback to text input)
  return (
    <div className="question-card">
      <div className="question-type">{question.question_type}</div>
      <div className="question-text">{question.text}</div>
      <div className="answer-row">
        <input
          type="text"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="Type your answer..."
          onKeyDown={(e) => {
            if (e.key === "Enter" && value.trim()) submit(value);
          }}
        />
        <button
          className="btn-answer"
          disabled={submitting || !value.trim()}
          onClick={() => submit(value)}
        >
          Send
        </button>
      </div>
    </div>
  );
}

export function QuestionPanel({ pipelineId, active }: QuestionPanelProps) {
  const fetcher = useCallback(() => getQuestions(pipelineId), [pipelineId]);
  const { data: questions } = usePolling<ApiQuestion[]>(fetcher, 1000, active);

  if (!questions || questions.length === 0) {
    return null;
  }

  return (
    <div className="panel question-panel dashboard-full">
      <h3 className="panel-title">Questions</h3>
      {questions.map((q) => (
        <QuestionCard key={q.id} pipelineId={pipelineId} question={q} />
      ))}
    </div>
  );
}
