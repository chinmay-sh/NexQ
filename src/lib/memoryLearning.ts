/** Schedule one non-blocking memory update from a captured meeting snapshot. */
export function learnFromMeeting(
  enabled: boolean,
  transcript: string,
  notes: string[],
  model: string,
  update: (transcript: string, notes: string[], model: string | null) => Promise<string>,
  toast: (message: string, type: "success" | "warning") => void,
  warn: (...args: unknown[]) => void = console.warn,
): void {
  if (!enabled || !transcript.trim()) return;
  void update(transcript, notes, model || null)
    .then(() => toast("Memory updated", "success"))
    .catch((error: unknown) => {
      const message = error instanceof Error ? error.message : String(error);
      warn("[meetingStore] Failed to update memory:", error);
      if (!/no transcript|no final lines|no transcript to learn/i.test(message)) toast("Couldn't update memory", "warning");
    });
}
