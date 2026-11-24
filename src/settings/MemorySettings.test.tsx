import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ getMemory: vi.fn(), setMemory: vi.fn() }));
vi.mock("../lib/ipc", () => ipc);
import { MemorySettings } from "./MemorySettings";
import { useConfigStore } from "../stores/configStore";

describe("MemorySettings", () => {
  beforeEach(() => { ipc.getMemory.mockReset().mockResolvedValue("# About me\n"); ipc.setMemory.mockReset().mockResolvedValue(undefined); useConfigStore.setState({ memoryLearnFromMeetings: true }); });
  it("loads the memory and saves edits", async () => {
    render(<MemorySettings />);
    const editor = await screen.findByRole("textbox", { name: "Memory markdown" });
    await waitFor(() => expect(editor).toHaveValue("# About me\n"));
    fireEvent.change(editor, { target: { value: "# Updated" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(ipc.setMemory).toHaveBeenCalledWith("# Updated"));
    expect(await screen.findByText("Saved")).toBeInTheDocument();
  });
});
