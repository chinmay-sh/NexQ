import { useCallback, useEffect, useState } from "react";
import { Layers } from "lucide-react";
import { getMemory, setMemory } from "../lib/ipc";
import { useConfigStore } from "../stores/configStore";

function Toggle({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) {
  return <button type="button" role="switch" aria-checked={checked} aria-label="Learn from each meeting" onClick={() => onChange(!checked)} className={`relative h-5 w-9 shrink-0 rounded-full ${checked ? "bg-primary" : "bg-muted"}`}><span className={`absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white shadow-sm ${checked ? "translate-x-4" : ""}`} /></button>;
}

export function MemorySettings() {
  const enabled = useConfigStore((s) => s.memoryLearnFromMeetings);
  const [memory, setMemoryDraft] = useState("");
  const [status, setStatus] = useState("");
  const [loading, setLoading] = useState(true);
  const reloadMemory = useCallback(async () => {
    setLoading(true);
    try { setMemoryDraft(await getMemory()); setStatus(""); }
    catch (e) { setStatus(`Couldn't load memory: ${e instanceof Error ? e.message : String(e)}`); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void reloadMemory(); }, [reloadMemory]);

  return <section aria-labelledby="memory-heading" className="rounded-xl border border-border/30 bg-card/50 p-4">
    <div className="mb-4 flex items-center gap-2.5">
      <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-primary/10"><Layers className="h-3.5 w-3.5 text-primary" /></div>
      <div><h3 id="memory-heading" className="text-sm font-semibold text-primary/80">Memory</h3><p className="text-meta text-muted-foreground">A growing Markdown file about you</p></div>
    </div>
    <p className="mb-3 text-xs leading-relaxed text-muted-foreground">memory.md contains useful context about your projects, work, people and vocabulary. The meeting copilot reads it on every check. When learning is on, NexQ updates it after each meeting. In the <code>## Vocabulary</code> section, list one term per line; optionally include misheard forms, for example <code>- KASH — heard as: cash, cache</code>.</p>
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-3"><label className="text-xs font-medium text-muted-foreground">Learn from each meeting</label><Toggle checked={enabled} onChange={useConfigStore.getState().setMemoryLearnFromMeetings} /></div>
      <textarea aria-label="Memory markdown" rows={12} disabled={loading} value={memory} onChange={(e) => { setMemoryDraft(e.target.value); setStatus(""); }} className="w-full resize-y rounded-lg border border-border/50 bg-secondary/30 px-3 py-2.5 font-mono text-xs text-foreground focus:border-primary/50 focus:outline-none focus:ring-1 focus:ring-primary/20 disabled:opacity-50" />
      <div className="flex items-center gap-2">
        <button type="button" disabled={loading} onClick={async () => { try { await setMemory(memory); setStatus("Saved"); } catch (e) { setStatus(`Couldn't save memory: ${e instanceof Error ? e.message : String(e)}`); } }} className="rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground disabled:opacity-50">Save</button>
        <button type="button" disabled={loading} onClick={() => void reloadMemory()} className="rounded-lg border border-border/50 px-3 py-1.5 text-xs text-muted-foreground disabled:opacity-50">Revert</button>
        <span role="status" className="text-xs text-muted-foreground">{loading ? "Loading…" : status}</span>
      </div>
    </div>
  </section>;
}
