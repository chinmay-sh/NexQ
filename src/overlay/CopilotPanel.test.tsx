import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CopilotPanel } from "./CopilotPanel";
import { useCopilotStore } from "../stores/copilotStore";
import { useConfigStore } from "../stores/configStore";

beforeEach(() => {
  useCopilotStore.getState().reset();
  useConfigStore.setState({ copilotEnabled: true });
});
afterEach(cleanup);

it("renders pinned question answers before cards and meeting notes", () => {
  const store = useCopilotStore.getState();
  const id = store.addQuestion("When does launch happen?")!;
  store.resolveQuestion(id, { kind: "question", topic: "Launch timing", points: ["It is planned for May."], files: [] });
  store.appendResult(["The team agreed to May."], [{ kind: "insight", topic: "Launch plan", points: ["The plan is approved."], files: [] }]);
  render(<CopilotPanel />);
  const pinned = screen.getByText("When does launch happen?");
  const card = screen.getByText("Launch plan");
  const notes = screen.getByRole("heading", { name: "Meeting notes" });
  expect(pinned.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(card.compareDocumentPosition(notes) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});
