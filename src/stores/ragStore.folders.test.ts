import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  listRagFolders: vi.fn(),
  addRagFolder: vi.fn(),
  removeRagFolder: vi.fn(),
  syncRagFolders: vi.fn(),
  getRagStatus: vi.fn(),
  testOllamaEmbeddingConnection: vi.fn(),
  showToast: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  getRagConfig: vi.fn(), updateRagConfig: vi.fn(), getRagStatus: mocks.getRagStatus,
  rebuildRagIndex: vi.fn(), rebuildFileIndex: vi.fn(), removeFileRagIndex: vi.fn(),
  clearRagIndex: vi.fn(), testRagSearch: vi.fn(), testOllamaEmbeddingConnection: mocks.testOllamaEmbeddingConnection,
  pullEmbeddingModel: vi.fn(), listRagFolders: mocks.listRagFolders, addRagFolder: mocks.addRagFolder,
  removeRagFolder: mocks.removeRagFolder, syncRagFolders: mocks.syncRagFolders,
}));
vi.mock("./toastStore", () => ({ showToast: mocks.showToast }));
vi.mock("./configStore", () => ({ useConfigStore: { getState: () => ({ contextStrategy: "local_rag" }) } }));

import { useRagStore } from "./ragStore";

describe("RAG folders and keyword-mode Ollama gating", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useRagStore.setState({ ragConfig: null, ragFolders: [], indexStatus: null, isSyncingFolders: false });
  });

  it("loads and adds folders using the folder IPC actions", async () => {
    mocks.addRagFolder.mockResolvedValue(undefined);
    mocks.listRagFolders.mockResolvedValue([{ path: "C:/docs", exists: true, files: 4, chunks: 12 }]);

    await useRagStore.getState().addFolder("C:/docs");

    expect(mocks.addRagFolder).toHaveBeenCalledWith("C:/docs");
    expect(useRagStore.getState().ragFolders).toEqual([{ path: "C:/docs", exists: true, files: 4, chunks: 12 }]);
  });

  it("does not probe or toast about Ollama in keyword mode", async () => {
    useRagStore.setState({ ragConfig: { search_mode: "keyword" } as never });

    await useRagStore.getState().checkOllamaStatus();

    expect(mocks.testOllamaEmbeddingConnection).not.toHaveBeenCalled();
    expect(mocks.showToast).not.toHaveBeenCalled();
  });
});
