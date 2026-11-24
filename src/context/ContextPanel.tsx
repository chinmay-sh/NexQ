import { useEffect, useCallback, useState } from "react";
import { useContextStore } from "../stores/contextStore";
import { useConfigStore } from "../stores/configStore";
import { useRagStore } from "../stores/ragStore";
import { FileUpload } from "./FileUpload";
import { ResourceCard } from "./ResourceCard";
import { TokenBudget } from "./TokenBudget";
import { RagIndexBar } from "./RagIndexBar";
import { FileText, Folder, FolderPlus, Loader2, RefreshCw, Trash2, AlertTriangle } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { showToast } from "../stores/toastStore";
import { onRagFoldersSynced } from "../lib/events";
import type { UnlistenFn } from "@tauri-apps/api/event";

export function ContextPanel() {
  const resources = useContextStore((s) => s.resources);
  const removeFile = useContextStore((s) => s.removeFile);
  const loadResources = useContextStore((s) => s.loadResources);
  const refreshTokenBudget = useContextStore((s) => s.refreshTokenBudget);
  const contextStrategy = useConfigStore((s) => s.contextStrategy);
  const autoRemoveFileIndex = useRagStore((s) => s.autoRemoveFileIndex);
  const folders = useRagStore((s) => s.ragFolders);
  const loadRagFolders = useRagStore((s) => s.loadRagFolders);
  const addFolder = useRagStore((s) => s.addFolder);
  const removeFolder = useRagStore((s) => s.removeFolder);
  const syncFolders = useRagStore((s) => s.syncFolders);
  const isSyncingFolders = useRagStore((s) => s.isSyncingFolders);
  const [isAddingFolder, setIsAddingFolder] = useState(false);

  const handleRemoveFile = useCallback((id: string) => {
    removeFile(id);
    if (contextStrategy === "local_rag") {
      autoRemoveFileIndex(id);
    }
  }, [removeFile, contextStrategy, autoRemoveFileIndex]);

  // Load resources and token budget on mount
  useEffect(() => {
    loadResources();
    refreshTokenBudget();
    loadRagFolders();
  }, [loadResources, refreshTokenBudget, loadRagFolders]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let active = true;
    onRagFoldersSynced(async () => {
      if (!active) return;
      await Promise.all([loadRagFolders(), useRagStore.getState().refreshIndexStatus()]);
    }).then((stop) => { if (active) unlisten = stop; else stop(); });
    return () => { active = false; unlisten?.(); };
  }, [loadRagFolders]);

  const handleAddFolder = async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected !== "string") return;
      setIsAddingFolder(true);
      await addFolder(selected);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e), "error");
    } finally { setIsAddingFolder(false); }
  };

  return (
    <div className="flex h-full flex-col gap-5 overflow-y-auto p-5">
      {/* Panel header */}
      <div className="flex items-center gap-2">
        <FileText className="h-4 w-4 text-primary" />
        <h2 className="text-sm font-semibold text-foreground">Meeting Context</h2>
        {contextStrategy === "local_rag" && (
          <div className="flex items-center gap-1.5 rounded-full bg-primary/10 px-2.5 py-1 text-meta font-medium text-primary">
            <span>Smart Search Active</span>
          </div>
        )}
      </div>

      {/* Token budget visualization */}
      <TokenBudget />

      {/* RAG index bar (when RAG is active) */}
      {contextStrategy === "local_rag" && <RagIndexBar />}

      {/* File upload area */}
      <FileUpload />

      {/* Shown under every strategy: the meeting copilot searches these folders. */}
      <section className="space-y-2.5" aria-labelledby="rag-folders-heading">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <Folder className="h-3.5 w-3.5 text-muted-foreground" />
            <h3 id="rag-folders-heading" className="text-xs font-medium text-muted-foreground">Folders ({folders.length})</h3>
          </div>
          <div className="flex items-center gap-1.5">
            <button type="button" onClick={() => syncFolders()} disabled={isSyncingFolders} className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50" title="Rescan folders">
              {isSyncingFolders ? <Loader2 className="h-3 w-3 animate-spin" /> : <RefreshCw className="h-3 w-3" />} Rescan
            </button>
            <button type="button" onClick={handleAddFolder} disabled={isAddingFolder} className="inline-flex items-center gap-1 rounded-lg bg-primary/10 px-2.5 py-1 text-xs font-medium text-primary transition-colors hover:bg-primary/15 disabled:opacity-50">
              {isAddingFolder ? <Loader2 className="h-3 w-3 animate-spin" /> : <FolderPlus className="h-3 w-3" />} Add folder
            </button>
          </div>
        </div>
        {folders.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border/40 px-3 py-3 text-xs text-muted-foreground">Add a folder to index its supported files in place. The meeting copilot searches them (with Local RAG, so does Ask).</p>
        ) : folders.map((folder) => {
          const name = folder.path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || folder.path;
          return (
            <div key={folder.path} className="group flex min-w-0 items-center gap-3 rounded-xl border border-border/40 bg-secondary/20 p-3">
              <Folder className="h-4 w-4 flex-shrink-0 text-primary/70" />
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium text-foreground" title={folder.path}>{name}</p>
                <p className="truncate text-xs text-muted-foreground" title={folder.path}>{folder.path}</p>
                <div className="mt-1 flex items-center gap-2 text-meta text-muted-foreground">
                  <span>{folder.files} files</span><span>{folder.chunks} chunks</span>
                  {!folder.exists && <span className="inline-flex items-center gap-1 font-medium text-amber-500"><AlertTriangle className="h-3 w-3" />Folder not found</span>}
                </div>
              </div>
              <button type="button" onClick={() => removeFolder(folder.path)} className="flex-shrink-0 rounded-lg p-1.5 text-muted-foreground/60 transition-colors hover:bg-destructive/10 hover:text-destructive" title={`Remove ${name}`} aria-label={`Remove folder ${name}`}>
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </div>
          );
        })}
      </section>

      {/* Loaded resources list */}
      {resources.length > 0 && (
        <div className="flex flex-col gap-2.5">
          <div className="flex items-center gap-2">
            <FileText className="h-3.5 w-3.5 text-muted-foreground" />
            <span className="text-xs font-medium text-muted-foreground">
              Loaded Context ({resources.length} file
              {resources.length !== 1 ? "s" : ""})
            </span>
          </div>
          <div className="flex flex-col gap-2.5">
            {resources.map((resource) => (
              <ResourceCard
                key={resource.id}
                resource={resource}
                onRemove={handleRemoveFile}
              />
            ))}
          </div>
        </div>
      )}

    </div>
  );
}
