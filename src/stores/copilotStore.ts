import { create } from "zustand";
import type { CopilotCard } from "../lib/types";

export interface CopilotCardItem extends CopilotCard { id: string; time: number }
export interface PinnedQuestionCard {
  id: string; question: string; status: "pending" | "done" | "error"; card: CopilotCardItem | null;
}
interface CopilotState {
  notes: string[];
  cards: CopilotCardItem[];
  questionCards: PinnedQuestionCard[];
  questionKeys: string[];
  /** How much of each transcript line (by id) was already checked. A line keeps
   * growing under the same id while its speaker talks, so only its new words go next. */
  sent: Map<string, number>;
  tickInFlight: boolean;
  lastError: string | null;
  lastCheckAt: number | null;
  appendResult: (notes: string[], cards: CopilotCard[]) => void;
  beginTick: () => boolean;
  finishTick: (marks: [string, number][], notes: string[], cards: CopilotCard[]) => void;
  failTick: (error: string) => void;
  cancelTick: () => void;
  addQuestion: (question: string) => string | null;
  resolveQuestion: (id: string, card: CopilotCard) => void;
  retryQuestion: (id: string) => void;
  failQuestion: (id: string) => void;
  dismissQuestion: (id: string) => void;
  reset: () => void;
}
const fresh = () => ({ notes: [] as string[], cards: [] as CopilotCardItem[], questionCards: [] as PinnedQuestionCard[], questionKeys: [] as string[], sent: new Map<string, number>(), tickInFlight: false, lastError: null as string | null, lastCheckAt: null as number | null });
const normalize = (text: string) => text.toLocaleLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
const unique = (values: string[]) => {
  const seen = new Set<string>();
  return values.filter((v) => { const key = normalize(v); if (!key || seen.has(key)) return false; seen.add(key); return true; });
};
export const useCopilotStore = create<CopilotState>((set, get) => ({
  ...fresh(),
  appendResult: (notes, cards) => set((s) => ({ notes: unique([...s.notes, ...notes]), cards: [...cards.map((c) => ({ ...c, id: crypto.randomUUID(), time: Date.now() })), ...s.cards], lastCheckAt: Date.now(), lastError: null })),
  beginTick: () => { if (get().tickInFlight) return false; set({ tickInFlight: true }); return true; },
  finishTick: (marks, notes, cards) => set((s) => ({ ...s, notes: unique([...s.notes, ...notes]), cards: [...cards.map((c) => ({ ...c, id: crypto.randomUUID(), time: Date.now() })), ...s.cards], sent: new Map([...s.sent, ...marks]), tickInFlight: false, lastError: null, lastCheckAt: Date.now() })),
  failTick: (error) => set({ tickInFlight: false, lastError: error }),
  cancelTick: () => set({ tickInFlight: false, lastError: null }),
  addQuestion: (question) => {
    const normalized = normalize(question.trim());
    if (!normalized || get().questionKeys.includes(normalized)) return null;
    const id = crypto.randomUUID(); set((s) => ({ questionKeys: [...s.questionKeys, normalized], questionCards: [{ id, question, status: "pending", card: null }, ...s.questionCards] })); return id;
  },
  resolveQuestion: (id, card) => set((s) => ({ questionCards: s.questionCards.map((q) => q.id === id ? { ...q, status: "done", card: { ...card, id, time: Date.now() } } : q) })),
  retryQuestion: (id) => set((s) => ({ questionCards: s.questionCards.map((q) => q.id === id ? { ...q, status: "pending", card: null } : q) })),
  failQuestion: (id) => set((s) => ({ questionCards: s.questionCards.map((q) => q.id === id ? { ...q, status: "error" } : q) })),
  dismissQuestion: (id) => set((s) => ({ questionCards: s.questionCards.filter((q) => q.id !== id) })),
  reset: () => set(fresh()),
}));

type CopilotSegment = { id: string; text: string; speaker: string; is_final: boolean };

/** The final words not checked yet, one line per transcript line ("Speaker: words";
 * "Speaker: … words" when a line grew since its start was sent), the latest 60
 * lines, and the [id, length] marks to save when the tick succeeds (all lines, so
 * older ones beyond the cap aren't sent later). */
export function selectCopilotLines<S extends CopilotSegment>(segments: S[], sent: Map<string, number>, label: (s: S) => string = (s) => s.speaker === "User" ? "You" : s.speaker) {
  const lines: string[] = [];
  const marks: [string, number][] = [];
  for (const s of segments) {
    const done = sent.get(s.id) ?? 0;
    if (!s.is_final || s.text.length <= done) continue;
    const words = s.text.slice(done).trim();
    marks.push([s.id, s.text.length]);
    if (words) lines.push(`${label(s)}: ${done > 0 ? "… " : ""}${words}`);
  }
  return { lines: lines.slice(-60), marks };
}

/** The checked part of each line, the latest `max` lines, as "Speaker: words" (context for the next check). */
export function checkedLines<S extends CopilotSegment>(segments: S[], sent: Map<string, number>, label: (s: S) => string, max = 20): string[] {
  return segments
    .filter((s) => (sent.get(s.id) ?? 0) > 0)
    .map((s) => `${label(s)}: ${s.text.slice(0, sent.get(s.id)).trim()}`)
    .slice(-max);
}
