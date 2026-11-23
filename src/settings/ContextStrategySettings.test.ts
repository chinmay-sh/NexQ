import { describe, expect, it } from "vitest";
import { DEFAULT_RAG_CONFIG, PRESETS, getEmbeddingModelDimensions, getEmbeddingProviderDefaults } from "./ContextStrategySettings";

describe("embedding provider defaults", () => {
  it("uses Ollama by default and includes it in each preset config", () => {
    expect(DEFAULT_RAG_CONFIG.embedding_provider).toBe("ollama");
    for (const preset of PRESETS) expect(preset.config.embedding_provider).toBe("ollama");
  });

  it("switches providers to their default model and dimensions", () => {
    expect(getEmbeddingProviderDefaults("gemini")).toEqual({ model: "gemini-embedding-2", dimensions: 768 });
    expect(getEmbeddingProviderDefaults("voyage")).toEqual({ model: "voyage-4-lite", dimensions: 1024 });
    expect(getEmbeddingProviderDefaults("ollama")).toEqual({ model: "nomic-embed-text", dimensions: 768 });
  });

  it("uses provider-specific dimensions when a model changes", () => {
    expect(getEmbeddingModelDimensions("gemini", "gemini-embedding-001")).toBe(768);
    expect(getEmbeddingModelDimensions("voyage", "voyage-4-large")).toBe(1024);
    expect(getEmbeddingModelDimensions("ollama", "all-minilm")).toBe(384);
  });
});
