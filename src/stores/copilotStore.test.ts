import { beforeEach, describe, expect, it } from "vitest";
import { selectCopilotLines, useCopilotStore } from "./copilotStore";

const card = { kind: "insight" as const, topic: "Budget", points: ["Review next quarter"], files: [] };
beforeEach(() => useCopilotStore.getState().reset());

describe("copilot store", () => {
  it("deduplicates notes and prepends cards; reset clears meeting state", () => {
    useCopilotStore.getState().appendResult(["Launch is in May"], [card]);
    useCopilotStore.getState().appendResult(["launch is in may", "Budget approved"], [card]);
    expect(useCopilotStore.getState().notes).toEqual(["Launch is in May", "Budget approved"]);
    expect(useCopilotStore.getState().cards).toHaveLength(2);
    useCopilotStore.getState().reset();
    expect(useCopilotStore.getState().notes).toEqual([]);
    expect(useCopilotStore.getState().sentIds.size).toBe(0);
  });

  it("marks lines as checked only when the tick succeeds", () => {
    expect(useCopilotStore.getState().beginTick()).toBe(true);
    useCopilotStore.getState().failTick("offline");
    expect(useCopilotStore.getState().sentIds.size).toBe(0);
    useCopilotStore.getState().beginTick();
    useCopilotStore.getState().finishTick(["a", "b"], [], []);
    expect([...useCopilotStore.getState().sentIds]).toEqual(["a", "b"]);
  });

  it("deduplicates detected questions even after dismissing", () => {
    const id = useCopilotStore.getState().addQuestion("What is the launch date?");
    expect(id).toBeTruthy();
    useCopilotStore.getState().dismissQuestion(id!);
    expect(useCopilotStore.getState().addQuestion("what is the launch date")).toBeNull();
  });
});

it("selects only unchecked final non-empty lines and caps to the latest 60", () => {
  const segments = [
    { id: "1", text: "old", speaker: "Them", is_final: true },
    { id: "2", text: "partial", speaker: "Them", is_final: false },
    { id: "3", text: "   ", speaker: "User", is_final: true },
    { id: "4", text: "new", speaker: "User", is_final: true },
  ];
  expect(selectCopilotLines(segments, new Set(["1"]))).toEqual({ lines: ["You: new"], ids: ["4"] });
  // A line that turns final after later lines were checked is still sent.
  segments[1].is_final = true;
  expect(selectCopilotLines(segments, new Set(["1", "4"])).lines).toEqual(["Them: partial"]);
  const many = Array.from({ length: 70 }, (_, i) => ({ id: `m${i}`, text: `line ${i}`, speaker: "Them", is_final: true }));
  const picked = selectCopilotLines(many, new Set());
  expect(picked.lines).toHaveLength(60);
  expect(picked.lines[0]).toBe("Them: line 10");
  expect(picked.ids).toHaveLength(70);
});
