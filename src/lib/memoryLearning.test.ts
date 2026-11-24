import { describe, expect, it, vi } from "vitest";
import { learnFromMeeting } from "./memoryLearning";

describe("learnFromMeeting", () => {
  it("passes the captured transcript, notes and model and reports success", async () => {
    const update = vi.fn().mockResolvedValue("memory");
    const toast = vi.fn();
    learnFromMeeting(true, "You: hello", ["Note"], "", update, toast);
    await vi.waitFor(() => expect(toast).toHaveBeenCalledWith("Memory updated", "success"));
    expect(update).toHaveBeenCalledWith("You: hello", ["Note"], null);
  });

  it("skips disabled or empty transcripts", () => {
    const update = vi.fn();
    learnFromMeeting(false, "You: hello", [], "", update, vi.fn());
    learnFromMeeting(true, "  ", [], "", update, vi.fn());
    expect(update).not.toHaveBeenCalled();
  });
});
