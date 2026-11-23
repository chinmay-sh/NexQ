import { useEffect, useRef } from "react";
import { copilotTick } from "../lib/ipc";
import { useConfigStore } from "../stores/configStore";
import { useMeetingStore } from "../stores/meetingStore";
import { selectCopilotLines, useCopilotStore } from "../stores/copilotStore";
import { useTranscriptStore } from "../stores/transcriptStore";
import { useSpeakerStore } from "../stores/speakerStore";
import { assistSpeakerLabel } from "../lib/speakerLabels";
import { useQuestionStore } from "../stores/questionStore";

export function useMeetingCopilot() {
  const active = useMeetingStore((s) => !!s.activeMeeting);
  const enabled = useConfigStore((s) => s.copilotEnabled);
  const interval = useConfigStore((s) => s.copilotIntervalSeconds);
  const answerQuestions = useConfigStore((s) => s.copilotAnswerQuestions);
  const model = useConfigStore((s) => s.copilotModel);
  const webSearch = useConfigStore((s) => s.copilotWebSearch);
  const questions = useQuestionStore((s) => s.questions);
  const askedQuestions = useRef(new Set<string>());

  useEffect(() => {
    if (!active || !enabled) return;
    let cancelled = false;
    const timer = window.setInterval(async () => {
      const state = useCopilotStore.getState();
      if (state.tickInFlight) return;
      const segments = useTranscriptStore.getState().segments;
      const label = (s: typeof segments[number]) => assistSpeakerLabel(s, (key) => useSpeakerStore.getState().speakers[key]?.display_name).replace(/^User$/, "You");
      const { lines, ids } = selectCopilotLines(segments, state.sentIds, label);
      if (!lines.length || !state.beginTick()) return;
      const recent = segments.filter((s) => s.is_final && s.text.trim() && state.sentIds.has(s.id)).slice(-20);
      try {
        const result = await copilotTick({ newLines: lines.join("\n"), recentContext: recent.map((s) => `${label(s)}: ${s.text.trim()}`).join("\n"), notes: state.notes, covered: state.cards.map((c) => c.topic), model, webSearch });
        if (cancelled) return;
        useCopilotStore.getState().finishTick(ids, result.notes, result.cards);
      } catch (error) {
        if (!cancelled) useCopilotStore.getState().failTick(error instanceof Error ? error.message : String(error));
      }
    }, Math.max(5, interval) * 1000);
    return () => { cancelled = true; window.clearInterval(timer); useCopilotStore.getState().cancelTick(); };
  }, [active, enabled, interval, model, webSearch]);

  useEffect(() => {
    if (!active || !enabled || !answerQuestions) return;
    const currentTexts = new Set(questions.map((q) => `${q.segmentId ?? ""}:${q.text}`));
    for (const key of askedQuestions.current) if (![...currentTexts].some((current) => current === key || current.startsWith(`${key.split(":")[0]}:`))) askedQuestions.current.delete(key);
    if (questions.length === 0) return;
    const segments = useTranscriptStore.getState().segments;
    const label = (s: typeof segments[number]) => assistSpeakerLabel(s, (key) => useSpeakerStore.getState().speakers[key]?.display_name).replace(/^User$/, "You");
    const recentContext = segments.filter((s) => s.is_final && !s.pending_text?.trim() && s.text.trim()).slice(-20).map((s) => `${label(s)}: ${s.text.trim()}`).join("\n");
    for (const question of questions) {
      const key = `${question.segmentId ?? ""}:${question.text}`;
      if (askedQuestions.current.has(key)) continue;
      askedQuestions.current.add(key);
      const id = useCopilotStore.getState().addQuestion(question.text);
      if (!id) continue;
      void copilotTick({ newLines: "", recentContext, notes: useCopilotStore.getState().notes, covered: useCopilotStore.getState().cards.map((c) => c.topic), question: question.text, model, webSearch })
        .then((result) => { const card = result.cards[0]; if (card) useCopilotStore.getState().resolveQuestion(id, card); else useCopilotStore.getState().failQuestion(id); })
        .catch(() => useCopilotStore.getState().failQuestion(id));
    }
  }, [active, enabled, answerQuestions, questions, model, webSearch]);
}
