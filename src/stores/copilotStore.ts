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
  /** Ids of the transcript lines already checked. Ids, not positions: a line can turn final after later lines were sent. */
  sentIds: Set<string>;
  tickInFlight: boolean;
  lastError: string | null;
  lastCheckAt: number | null;
  appendResult: (notes: string[], cards: CopilotCard[]) => void;
  beginTick: () => boolean;
  finishTick: (ids: string[], notes: string[], cards: CopilotCard[]) => void;
  failTick: (error: string) => void;
  cancelTick: () => void;
  addQuestion: (question: string) => string | null;
  resolveQuestion: (id: string, card: CopilotCard) => void;
  retryQuestion: (id: string) => void;
  failQuestion: (id: string) => void;
  dismissQuestion: (id: string) => void;
  reset: () => void;
}
const fresh = () => ({ notes: [] as string[], cards: [] as CopilotCardItem[], questionCards: [] as PinnedQuestionCard[], questionKeys: [] as string[], sentIds: new Set<string>(), tickInFlight: false, lastError: null as string | null, lastCheckAt: null as number | null });
const normalize = (text: string) => text.toLocaleLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
const unique = (values: string[]) => {
  const seen = new Set<string>();
  return values.filter((v) => { const key = normalize(v); if (!key || seen.has(key)) return false; seen.add(key); return true; });
};
export const useCopilotStore = create<CopilotState>((set, get) => ({
  ...fresh(),
  appendResult: (notes, cards) => set((s) => ({ notes: unique([...s.notes, ...notes]), cards: [...cards.map((c) => ({ ...c, id: crypto.randomUUID(), time: Date.now() })), ...s.cards], lastCheckAt: Date.now(), lastError: null })),
  beginTick: () => { if (get().tickInFlight) return false; set({ tickInFlight: true }); return true; },
  finishTick: (ids, notes, cards) => set((s) => ({ ...s, notes: unique([...s.notes, ...notes]), cards: [...cards.map((c) => ({ ...c, id: crypto.randomUUID(), time: Date.now() })), ...s.cards], sentIds: new Set([...s.sentIds, ...ids]), tickInFlight: false, lastError: null, lastCheckAt: Date.now() })),
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

/** Final, non-empty lines not checked yet (the latest 60 of them), and the ids
 * to mark as checked when the tick succeeds (all of them, so older ones beyond
 * the cap aren't sent later). */
export function selectCopilotLines<S extends CopilotSegment>(segments: S[], sentIds: Set<string>, label: (s: S) => string = (s) => s.speaker === "User" ? "You" : s.speaker) {
  const unsent = segments.filter((s) => s.is_final && s.text.trim() && !sentIds.has(s.id));
  return { lines: unsent.slice(-60).map((s) => `${label(s)}: ${s.text.trim()}`), ids: unsent.map((s) => s.id) };
}
