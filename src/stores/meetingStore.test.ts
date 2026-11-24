import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

/** config.json, which other windows may have written since this one read it. */
const saved = vi.hoisted(() => ({ config: {} as Record<string, unknown> }));
vi.mock("@tauri-apps/plugin-store", () => ({
  load: async () => ({
    get: async (key: string) => saved.config[key],
    set: async () => {},
  }),
}));

import { BUILT_IN_SCENARIOS } from "../lib/scenarios";
import type { PartyAudioConfig } from "../lib/types";
import { useMeetingStore } from "./meetingStore";
import { useScenarioStore } from "./scenarioStore";
import { useTranscriptStore } from "./transcriptStore";
import { useCopilotStore } from "./copilotStore";
import { useConfigStore } from "./configStore";

const boardReview = {
  id: "custom_1",
  name: "Board review",
  description: "Custom scenario",
  system_prompt: "Board system prompt",
  summary_prompt: "Board summary prompt",
  question_detection_prompt: "Board question prompt",
  is_custom: true,
};

const calls = new Map<string, Record<string, unknown>>();
let stopError: string | undefined;

beforeAll(() => {
  mockWindows("launcher");
  mockIPC((cmd, args) => {
    calls.set(cmd, args as Record<string, unknown>);
    if (cmd === "stop_capture" && stopError) throw stopError;
    if (cmd === "list_meetings") return "[]";
    return cmd === "start_meeting" ? JSON.stringify({ id: "meeting-1", title: "Standup" }) : [];
  });
});

beforeEach(() => {
  calls.clear();
  saved.config = {};
  stopError = undefined;
  useConfigStore.setState({ memoryLearnFromMeetings: false });
});

it("keeps the meeting open if the MAI transcript is still draining", async () => {
  const meetings = useMeetingStore.getState();
  await meetings.startMeetingFlow("Standup", "online", "team_meeting");
  stopError = "Timed out waiting for Azure MAI Streaming transcripts to drain";
  await expect(meetings.endMeetingFlow()).rejects.toThrow("press End again");
  expect(useMeetingStore.getState().activeMeeting?.id).toBe("meeting-1");
  expect(calls.has("end_meeting")).toBe(false);
});

it("starts an online meeting with the mic live after an in-person one muted it", async () => {
  const { useConfigStore } = await import("./configStore");
  const mic: PartyAudioConfig = { role: "You", device_id: "default", is_input_device: true, stt_provider: "deepgram" };
  const speakers: PartyAudioConfig = { role: "Them", device_id: "Speakers", is_input_device: false, stt_provider: "deepgram" };
  useConfigStore.setState({ meetingAudioConfig: { you: mic, them: speakers, recording_enabled: false, preset_name: null } });

  await useMeetingStore.getState().startMeetingFlow("Viva", "in_person", "team_meeting");
  expect(calls.get("set_source_muted")).toEqual({ source: "you", muted: true });
  expect(useConfigStore.getState().mutedYou).toBe(true);
  useMeetingStore.getState().stopTimer();
  useMeetingStore.setState({ activeMeeting: null, isRecording: false });

  await useMeetingStore.getState().startMeetingFlow("Standup", "online", "team_meeting");
  expect(calls.get("set_source_muted")).toEqual({ source: "you", muted: false });
  expect(useConfigStore.getState().mutedYou).toBe(false);
  useConfigStore.setState({ meetingAudioConfig: null, mutedYou: false });
});

afterEach(() => {
  const meetings = useMeetingStore.getState();
  meetings.stopTimer();
  useMeetingStore.setState({ activeMeeting: null, currentView: "launcher", isRecording: false, aiScenario: "team_meeting" });
  useScenarioStore.setState({ activeScenarioId: "team_meeting", customScenarios: [], scenarioOverrides: {} });
});

describe("startMeetingFlow's scenario", () => {
  it("uses a custom scenario's prompts, even one saved by another window", async () => {
    saved.config.customScenarios = [boardReview];
    await useMeetingStore.getState().startMeetingFlow("Standup", "online", "custom_1");

    expect(calls.get("set_active_scenario")).toEqual({
      systemPrompt: "Board system prompt",
      summaryPrompt: "Board summary prompt",
      questionDetectionPrompt: "Board question prompt",
    });
    expect(calls.get("update_meeting_mode")).toMatchObject({ aiScenario: "custom_1" });
    await vi.waitFor(() => {
      expect(calls.get("plugin:event|emit")).toMatchObject({ payload: { aiScenario: "custom_1" } });
    });
  });

  it("runs as Team Meeting when its custom scenario was deleted", async () => {
    await useMeetingStore.getState().startMeetingFlow("Standup", "online", "custom_1");

    expect(calls.get("set_active_scenario")).toMatchObject({ systemPrompt: BUILT_IN_SCENARIOS[0].system_prompt });
    expect(calls.get("update_meeting_mode")).toMatchObject({ aiScenario: "team_meeting" });
    expect(useMeetingStore.getState().aiScenario).toBe("team_meeting");
  });
});

describe("end meeting memory learning", () => {
  it("schedules once from final transcript lines and captured copilot notes", async () => {
    calls.delete("update_memory_from_meeting");
    useConfigStore.setState({ memoryLearnFromMeetings: true, copilotModel: "chosen-model" });
    await useMeetingStore.getState().startMeetingFlow("Memory test", "online", "team_meeting");
    useTranscriptStore.setState({ segments: [
      { id: "final", text: "Project KASH", speaker: "User", timestamp_ms: 1, is_final: true },
      { id: "partial", text: "ignore this", speaker: "Them", timestamp_ms: 2, is_final: false },
    ] } as never);
    useCopilotStore.setState({ notes: ["Launch is soon"] });
    await useMeetingStore.getState().endMeetingFlow();
    await vi.waitFor(() => expect(calls.get("update_memory_from_meeting")).toEqual({ transcript: "You: Project KASH", notes: ["Launch is soon"], model: "chosen-model" }));
    expect(calls.get("update_memory_from_meeting")).toEqual({ transcript: "You: Project KASH", notes: ["Launch is soon"], model: "chosen-model" });
  });

  it("does not schedule when learning is off", async () => {
    calls.delete("update_memory_from_meeting");
    useConfigStore.setState({ memoryLearnFromMeetings: false });
    await useMeetingStore.getState().startMeetingFlow("No learning", "online", "team_meeting");
    useTranscriptStore.setState({ segments: [{ id: "x", text: "hello", speaker: "User", timestamp_ms: 1, is_final: true }] } as never);
    await useMeetingStore.getState().endMeetingFlow();
    expect(calls.has("update_memory_from_meeting")).toBe(false);
  });

  it("does not schedule when the meeting has no final transcript line", async () => {
    calls.delete("update_memory_from_meeting");
    useConfigStore.setState({ memoryLearnFromMeetings: true });
    await useMeetingStore.getState().startMeetingFlow("No transcript", "online", "team_meeting");
    useTranscriptStore.setState({ segments: [{ id: "partial", text: "still speaking", speaker: "User", timestamp_ms: 1, is_final: false }] } as never);
    await useMeetingStore.getState().endMeetingFlow();
    expect(calls.has("update_memory_from_meeting")).toBe(false);
  });
});
