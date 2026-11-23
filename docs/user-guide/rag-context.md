# Using Context Intelligence (RAG)

NexQ's Context Intelligence feature lets you load your own documents so the AI can reference them during meetings. When you load your resume, a job description, course notes, or project documentation, the AI uses that context to provide more relevant and informed responses.

## Prerequisites

- NexQ installed and running ([Getting Started](getting-started.md))
- An LLM provider configured ([AI Providers Guide](ai-providers.md))
- Documents you want to load (PDF, DOCX, TXT, or MD format)

## What RAG Does

RAG (Retrieval-Augmented Generation) works by:

1. **Chunking** your documents into smaller sections
2. **Embedding** each chunk as a vector (numerical representation of meaning)
3. **Indexing** the chunks in a local SQLite database
4. **Searching** for relevant chunks when the AI needs context during a meeting
5. **Including** the most relevant chunks in the AI prompt alongside the conversation

The result: the AI can reference your documents when generating responses, making its suggestions more specific and useful.

## Supported File Formats

| Format | Extension | Notes |
|--------|-----------|-------|
| PDF | `.pdf` | Text-based PDFs. Scanned image PDFs are not supported. |
| Office | `.docx`, `.pptx`, `.xlsx` | Word, PowerPoint, and Excel files |
| Text and code | `.txt`, `.md`, `.markdown`, `.rst`, `.adoc`, `.tex`, `.log`, `.csv`, `.tsv`, `.json`, `.jsonl`, `.yaml`, `.yml`, `.toml`, `.ini`, `.cfg`, `.conf`, `.env`, `.xml`, `.html`, `.htm`, `.css`, `.scss`, `.less`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.ts`, `.tsx`, `.vue`, `.svelte`, `.py`, `.ipynb`, `.rs`, `.go`, `.java`, `.kt`, `.kts`, `.scala`, `.swift`, `.c`, `.h`, `.cpp`, `.cc`, `.hpp`, `.cs`, `.fs`, `.rb`, `.php`, `.pl`, `.lua`, `.r`, `.dart`, `.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`, `.sql`, `.graphql`, `.proto`, `.gradle`, `.tf` | Plain text, markup, configuration, and source files |

## Loading Documents

### Step by Step

1. Open the **Context Intelligence** panel in the Launcher
2. Click **Add Document**
3. Select one or more files from the file picker (multiple selection is supported)
4. Indexing begins automatically -- progress is shown in real-time with a progress indicator
5. Wait for indexing to complete before starting your meeting

### What Happens During Indexing

When you add a document, NexQ processes it through a 4-phase pipeline:

1. **Extract** -- reads text content from the file
2. **Chunk** -- splits the text into overlapping segments (configurable chunk size)
3. **Embed** -- generates vector embeddings for each chunk
4. **Index** -- stores chunks and embeddings in the local SQLite database with full-text search support

Indexing is fully asynchronous -- progress events (`rag_index_progress`) update the UI in real-time. You can continue using NexQ while indexing runs, but AI responses will only reference indexed content.

## How Search Works

When the AI needs context during a meeting, NexQ searches your indexed documents using hybrid search:

- **Vector similarity search** -- finds chunks with meaning similar to the current conversation
- **Full-text search (FTS)** -- finds chunks containing specific keywords or phrases
- **Hybrid mode** -- combines both methods for the best results (default)

The top matching chunks are included in the AI prompt, limited by the token budget.

### Indexing folders in place

In the Context Intelligence panel, use **Add folder** to index a directory without copying its files into NexQ. Folder files are searchable knowledge-base sources, but they do not appear as loaded context resources or count toward the token budget. NexQ automatically rescans folders every minute; use **Rescan** to scan immediately. Changes are indexed in the background.

Scans skip hidden directories, common generated/dependency folders (`node_modules`, `target`, `dist`, `build`, `out`, `bin`, `obj`, `__pycache__`, and `venv`), files larger than 20 MB, and Office lock files. A missing folder remains listed with a **Folder not found** warning until it is available again or removed. Remove a folder from the list to stop tracking it; its indexed chunks are removed in the background.

### Keyword search without Ollama

Choose **Keyword (BM25) — no Ollama needed** in Search Mode (or use the **Fastest** preset). Keyword mode indexes and searches with BM25 and does not call Ollama for embeddings. It is suitable for exact names, terms, and phrases. Hybrid and semantic modes use embeddings; they need Ollama for indexing and fall back to keyword results if Ollama is unreachable during search.

## Configuration

![Context Strategy settings panel](../../website/public/screenshots/Setting/Setting-Context%20Strategy.png)

Open Settings (`Ctrl+,`) > Context to configure RAG parameters:

| Setting | Description | Default |
|---------|-------------|---------|
| Chunk Size | How many tokens per document chunk | Provider default |
| Chunk Overlap | Overlap between adjacent chunks (for context continuity) | Provider default |
| Top-K Results | Number of relevant chunks included in each AI request | 5 |
| Similarity Threshold | Minimum relevance score for a chunk to be included | 0.7 |
| Search Mode | Semantic, Keyword (FTS), or Hybrid | Hybrid |

### Token Budget

Each AI request has a limited token budget for context. The token budget determines how many document chunks can be included alongside the conversation transcript. If you load many documents, only the most relevant chunks are selected within the budget.

## Best Practices by Use Case

### For Interviews

Load focused, relevant documents:

- Your **resume** (1-2 pages) -- so the AI knows your background
- The **job description** -- so the AI understands the role
- **Company research notes** -- key facts, recent news, products
- **Common interview questions** for the role -- the AI can help frame your answers

### For Lectures

Load course materials for richer AI responses:

- **Syllabus** -- helps the AI understand the course structure
- **Textbook excerpts** -- key chapters related to the lecture topic
- **Previous lecture notes** -- provides continuity across sessions
- **Study guides** -- the AI can reference these when answering your questions

### For Meetings

Load project context:

- **Project documentation** -- specs, requirements, design docs
- **Previous meeting notes** -- the AI can reference past decisions
- **Relevant reports or data** -- the AI can cite specific numbers or findings

## Clearing Context

To remove documents from the RAG index:

1. Open the Context Intelligence panel
2. Find the document you want to remove
3. Click the **remove/delete** button next to it
4. The document's chunks and embeddings are removed from the index

Clearing a document does not delete the original file -- it only removes it from NexQ's index.

## Tips

- **Smaller, focused documents work better than large dumps.** A 2-page resume produces better context matches than a 50-page portfolio. The AI retrieves the most relevant chunks, so concise documents mean every chunk is useful.
- **Wait for indexing to complete before starting a meeting.** Only indexed content is available to the AI. Check the progress indicator before beginning.
- **Update documents when they change.** If you update your resume or project docs, remove the old version and re-add the new one to refresh the index.
- **Use Hybrid search mode** (the default) for the best results. It combines semantic understanding with keyword matching.
- **Check the AI's responses** to see if it is referencing your documents. If responses seem generic, verify that indexing completed and that your documents are relevant to the conversation topic.
- **For embedding models**, if you are using Ollama for LLM, you can also use it for embeddings. Pull an embedding model like `nomic-embed-text`:
  ```
  ollama pull nomic-embed-text
  ```

## Next Steps

- [Interview Copilot Guide](interview-copilot.md) -- Load resume and job description for interviews
- [Lecture Assistant Guide](lecture-assistant.md) -- Load course materials for lectures
- [AI Providers Guide](ai-providers.md) -- Choose the best LLM for RAG-enhanced responses
- [Configuration Guide](configuration.md) -- Full RAG configuration options
