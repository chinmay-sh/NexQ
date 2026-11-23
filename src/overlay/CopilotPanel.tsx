import { useState } from "react";
import { Check, Copy, LoaderCircle, RotateCw, X } from "lucide-react";
import { copilotTick } from "../lib/ipc";
import { useCopilotStore } from "../stores/copilotStore";
import { useConfigStore } from "../stores/configStore";
import { useTranscriptStore } from "../stores/transcriptStore";

const fileName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() || path;

export function CopilotPanel() {
  const notes = useCopilotStore((s) => s.notes);
  const cards = useCopilotStore((s) => s.cards);
  const questionCards = useCopilotStore((s) => s.questionCards);
  const inFlight = useCopilotStore((s) => s.tickInFlight);
  const error = useCopilotStore((s) => s.lastError);
  const lastCheckAt = useCopilotStore((s) => s.lastCheckAt);
  const enabled = useConfigStore((s) => s.copilotEnabled);
  const model = useConfigStore((s) => s.copilotModel);
  const webSearch = useConfigStore((s) => s.copilotWebSearch);
  const [copied, setCopied] = useState(false);
  if (!enabled) return null;

  const retry = async (id: string, question: string) => {
    useCopilotStore.getState().retryQuestion(id);
    const questionId = id;
    const segments = useTranscriptStore.getState().segments;
    const recentContext = segments.filter((s) => s.is_final && s.text.trim()).slice(-20).map((s) => `${s.speaker === "User" ? "You" : s.speaker}: ${s.text.trim()}`).join("\n");
    try {
      const result = await copilotTick({ newLines: "", recentContext, notes: useCopilotStore.getState().notes, covered: useCopilotStore.getState().cards.map((c) => c.topic), question, model, webSearch });
      if (result.cards[0]) useCopilotStore.getState().resolveQuestion(questionId, result.cards[0]);
      else useCopilotStore.getState().failQuestion(questionId);
    } catch { useCopilotStore.getState().failQuestion(questionId); }
  };
  const copyNotes = async () => {
    try {
      await navigator.clipboard.writeText(notes.map((note) => `- ${note}`).join("\n"));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    } catch { /* Clipboard access may be unavailable in this webview. */ }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col" aria-label="Meeting copilot">
      <div className="mb-2 flex items-center justify-between gap-2 text-[10px] text-muted-foreground/70">
        <span className="flex min-w-0 items-center gap-1.5" role="status">
          {inFlight && <><LoaderCircle className="h-3 w-3 animate-spin" /> Checking…</>}
          {!inFlight && error && <span className="truncate text-destructive" title={error}>{error}</span>}
          {!inFlight && !error && lastCheckAt && <>Last check {new Date(lastCheckAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</>}
          {!inFlight && !error && !lastCheckAt && "Listening for new speech"}
        </span>
      </div>
      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto pr-0.5">
        {questionCards.map((item) => (
          <article key={item.id} className="rounded-lg border border-amber-400/25 bg-amber-400/10 p-2.5 text-amber-100">
            <div className="flex items-start gap-2">
              <div className="min-w-0 flex-1">
                <p className="text-xs font-semibold leading-snug text-amber-200">{item.question}</p>
                {item.status === "pending" && <p className="mt-2 flex items-center gap-1.5 text-[11px] text-amber-100/70"><LoaderCircle className="h-3 w-3 animate-spin" /> Finding an answer…</p>}
                {item.status === "error" && <div className="mt-2 flex items-center justify-between gap-2 text-[11px] text-amber-100/80"><span>Couldn’t answer this question.</span><button onClick={() => void retry(item.id, item.question)} className="inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-1 hover:bg-amber-300/15" aria-label={`Retry answer: ${item.question}`}><RotateCw className="h-3 w-3" /> Retry</button></div>}
                {item.card && <CardBody card={item.card} />}
              </div>
              <button onClick={() => useCopilotStore.getState().dismissQuestion(item.id)} className="shrink-0 rounded p-0.5 text-amber-100/50 hover:bg-amber-300/15 hover:text-amber-100" aria-label="Dismiss pinned answer"><X className="h-3.5 w-3.5" /></button>
            </div>
          </article>
        ))}
        {cards.map((card) => <article key={card.id} className={`rounded-lg border p-2.5 ${card.kind === "question" ? "border-amber-400/20 bg-amber-400/5" : card.kind === "insight" ? "border-primary/20 bg-primary/5" : "border-border/20 bg-card/30"}`}>
          <h3 className={`text-xs font-semibold leading-snug ${card.kind === "question" ? "text-amber-200" : card.kind === "insight" ? "text-primary" : "text-foreground/90"}`}>{card.topic}</h3>
          <CardBody card={card} />
        </article>)}
        <section className="pt-1" aria-label="Meeting notes">
          <div className="mb-1 flex items-center justify-between gap-2"><h3 className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/70">Meeting notes</h3><button onClick={() => void copyNotes()} className="inline-flex items-center gap-1 rounded px-1.5 py-1 text-[10px] text-muted-foreground/70 hover:bg-accent/50 hover:text-foreground" aria-label="Copy meeting notes as Markdown"><>{copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}{copied ? "Copied" : "Copy"}</></button></div>
          {notes.length > 0 ? <ul className="list-disc space-y-1 pl-4 text-[11px] leading-snug text-foreground/75">{notes.map((note, i) => <li key={`${note}-${i}`}>{note}</li>)}</ul> : <p className="text-[11px] text-muted-foreground/45">Notes will appear as new decisions and actions come up.</p>}
        </section>
        {!cards.length && !notes.length && !questionCards.length && <p className="py-5 text-center text-[11px] leading-relaxed text-muted-foreground/50">New notes and useful answers will appear here as the meeting continues.</p>}
      </div>
    </div>
  );
}

function CardBody({ card }: { card: { points: string[]; files: string[] } }) {
  return <>
    {card.points.length > 0 && <ul className="mt-1.5 list-disc space-y-1 pl-4 text-[11px] leading-snug text-foreground/75">{card.points.map((point, i) => <li key={`${point}-${i}`}>{point}</li>)}</ul>}
    {card.files.length > 0 && <div className="mt-2 flex min-w-0 flex-wrap gap-1">{card.files.map((file) => <span key={file} title={file} className="max-w-full truncate rounded border border-border/25 bg-background/30 px-1.5 py-0.5 text-[9px] text-muted-foreground/80">{fileName(file)}</span>)}</div>}
  </>;
}
