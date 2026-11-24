import { beforeEach, describe, expect, it } from "vitest";
import { checkedLines, selectCopilotLines, useCopilotStore } from "./copilotStore";

const card = { kind: "insight" as const, topic: "Budget", points: ["Review next quarter"], files: [], links: [] };
beforeEach(() => useCopilotStore.getState().reset());

describe("copilot store", () => {
  it("deduplicates notes and prepends cards; reset clears meeting state", () => {
    useCopilotStore.getState().appendResult(["Launch is in May"], [card]);
    useCopilotStore.getState().appendResult(["launch is in may", "Budget approved"], [card]);
    expect(useCopilotStore.getState().notes).toEqual(["Launch is in May", "Budget approved"]);
    expect(useCopilotStore.getState().cards).toHaveLength(2);
    useCopilotStore.getState().reset();
    expect(useCopilotStore.getState().notes).toEqual([]);
    expect(useCopilotStore.getState().sent.size).toBe(0);
  });

  it("marks lines as checked only when the tick succeeds", () => {
    expect(useCopilotStore.getState().beginTick()).toBe(true);
    useCopilotStore.getState().failTick("offline");
    expect(useCopilotStore.getState().sent.size).toBe(0);
    useCopilotStore.getState().beginTick();
    useCopilotStore.getState().finishTick([["a", 5], ["b", 3]], [], []);
    useCopilotStore.getState().beginTick();
    useCopilotStore.getState().finishTick([["a", 9]], [], []);
    expect([...useCopilotStore.getState().sent]).toEqual([["a", 9], ["b", 3]]);
  });

  it("deduplicates detected questions even after dismissing", () => {
    const id = useCopilotStore.getState().addQuestion("What is the launch date?");
    expect(id).toBeTruthy();
    useCopilotStore.getState().dismissQuestion(id!);
    expect(useCopilotStore.getState().addQuestion("what is the launch date")).toBeNull();
  });
});

it("sends only new final words, including words added to a line already sent", () => {
  const segments = [
    { id: "1", text: "old", speaker: "Them", is_final: true },
    { id: "2", text: "partial", speaker: "Them", is_final: false },
    { id: "3", text: "   ", speaker: "User", is_final: true },
    { id: "4", text: "Or okay.", speaker: "User", is_final: true },
  ];
  expect(selectCopilotLines(segments, new Map([["1", 3]]))).toEqual({ lines: ["You: Or okay."], marks: [["3", 3], ["4", 8]] });
  // The line grows while the speaker keeps talking: only the new words go, marked as a continuation.
  segments[3].text = "Or okay. Let's talk about the KASH project.";
  const grown = selectCopilotLines(segments, new Map([["1", 3], ["3", 3], ["4", 8]]));
  expect(grown).toEqual({ lines: ["You: … Let's talk about the KASH project."], marks: [["4", 43]] });
  // A line that turns final after later lines were checked is still sent.
  segments[1].is_final = true;
  expect(selectCopilotLines(segments, new Map([["1", 3], ["3", 3], ["4", 43]])).lines).toEqual(["Them: partial"]);
  const many = Array.from({ length: 70 }, (_, i) => ({ id: `m${i}`, text: `line ${i}`, speaker: "Them", is_final: true }));
  const picked = selectCopilotLines(many, new Map());
  expect(picked.lines).toHaveLength(60);
  expect(picked.lines[0]).toBe("Them: line 10");
  expect(picked.marks).toHaveLength(70);
});

it("gives the checked part of each line as context", () => {
  const segments = [
    { id: "1", text: "Hello there", speaker: "Them", is_final: true },
    { id: "2", text: "Or okay. More words", speaker: "User", is_final: true },
    { id: "3", text: "unsent", speaker: "User", is_final: true },
  ];
  const label = (s: { speaker: string }) => (s.speaker === "User" ? "You" : s.speaker);
  expect(checkedLines(segments, new Map([["1", 11], ["2", 8]]), label)).toEqual(["Them: Hello there", "You: Or okay."]);
});
