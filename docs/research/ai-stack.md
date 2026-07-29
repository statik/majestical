Research complete across all seven areas. Here is the structured report.

---

# Local AI Media Indexing on macOS — Landscape (mid-2026)

## 1. Ollama

**Embeddings.** `POST /api/embed` — text only. Accepts `input` as string or array, params `truncate` and `dimensions`; returns L2-normalized vectors ([docs.ollama.com/api/embed](https://docs.ollama.com/api/embed)). Recommended models: `embeddinggemma`, `qwen3-embedding`, `all-minilm` (384–1024 dims) ([capabilities/embeddings](https://docs.ollama.com/capabilities/embeddings)).

**Vision.** Ollama shipped its own multimodal engine (replacing llama.cpp orchestration) with self-contained per-model projection layers: Llama 4 Scout/Maverick, Gemma 3, Qwen 2.5-VL, Mistral Small 3.1 ([blog/multimodal-models](https://ollama.com/blog/multimodal-models)); current docs use `qwen3-vl:8b` and reference Gemma 4. Images go in as base64 arrays via `images` on `/api/generate` and `/api/chat`. Accepts JPEG/PNG/BMP/WebP/TIFF ([deepwiki 7.3](https://deepwiki.com/ollama/ollama/7.3-multimodal-and-vision-support)).

**OpenAI-compatible.** Yes: `/v1/chat/completions` (vision supported, **base64 only — image URLs not supported**), `/v1/completions`, `/v1/models`, `/v1/embeddings` (`dimensions` + `encoding_format` supported; token-array input not), `/v1/responses` (non-stateful only, v0.13.3+), and experimental `/v1/images/generations` ([docs.ollama.com/openai](https://docs.ollama.com/openai)).

**Critical gap — no image embeddings.** Issue [#5304](https://github.com/ollama/ollama/issues/5304) (open since Jun 2024, still open May 2026) and [#4296](https://github.com/ollama/ollama/issues/4296) request CLIP-style image vectors. PR #10728 and draft PR #15166 are unmerged; maintainers have not responded. Commenters report migrating to vLLM for Qwen3-VL-Embedding/Reranker. **Ollama cannot produce image vectors — only captions.**

## 2. LM Studio

**Server.** GUI Developer tab or `lms server start --port 1234`. Four API surfaces: native REST v1 (`/api/v1/*`), legacy v0, OpenAI-compatible (`/v1/*`), and Anthropic-compatible ([docs/developer](https://lmstudio.ai/docs/developer)). `llmster` daemon runs headless without the GUI.

**Model discovery is better than Ollama's.** `GET /api/v1/models` returns `type: "llm"|"embedding"` plus a `capabilities` object with explicit `vision`, `trained_for_tool_use`, and `reasoning` booleans ([rest/list](https://lmstudio.ai/docs/developer/rest/list)) — you can enumerate which local models can see images without a trial call.

**Embeddings.** `POST /api/v0/embeddings` and OpenAI `/v1/embeddings` — **text only** ([rest/endpoints](https://lmstudio.ai/docs/developer/rest/endpoints)). Same gap as Ollama. Additionally, LM Studio does not correctly type MLX embedding models as embeddings ([bug #808](https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/808), noted in [mlx-serve-embeddings](https://github.com/aperepel/mlx-serve-embeddings)).

**Vision.** VLMs via chat; SDK uses `client.files.prepareImage()` → `ImageHandle` passed in a message `images` array ([deepwiki 5.5](https://deepwiki.com/lmstudio-ai/docs/5.5-vision-language-models-(vlms))). The MIT-licensed [mlx-engine](https://github.com/lmstudio-ai/mlx-engine) composes `mlx-lm` text models with `mlx-vlm` `VisionAddOn` modules ([unified-mlx-engine](https://lmstudio.ai/blog/unified-mlx-engine)). June 2026 update added continuous batching to the vision runner and disk-offloaded KV cache: ~3.5x faster on repeated high-resolution image prompts ([mlx-engine-agentic-workloads](https://lmstudio.ai/blog/mlx-engine-agentic-workloads)).

## 3. OpenRouter

OpenAI-compatible, cloud-only (disqualifies it as the offline path; useful as an optional quality tier). **It does support embeddings** — `POST /api/v1/embeddings` with `input`, `model`, `dimensions`, `encoding_format`, `input_type`, and `provider` routing preferences. No streaming; deterministic output ([docs/api/reference/embeddings](https://openrouter.ai/docs/api/reference/embeddings.mdx)). `GET /api/v1/embeddings/models` lists them.

**Multimodal embeddings are available here and nowhere else in this trio.** Wrap input in a `content` array with `image_url` objects; text and image can be combined into a single joint vector. Named multimodal models: `nvidia/llama-nemotron-embed-vl-1b-v2` and **Gemini Embedding 2** (Google's first multimodal embedding model — unified text/image space, 8,192-token context, flexible 128–3,072 output dims, recommended 768/1536/3072) ([collections/embedding-models](https://openrouter.ai/collections/embedding-models), [every-modality-one-api](https://openrouter.ai/blog/insights/every-modality-one-api/)). Text-only options include Qwen3-Embedding-8B/4B/0.6B, text-embedding-3-small/large, bge-m3, mistral-embed, pplx-embed-v1.

Also relevant: `video_url` content type on chat completions, `/api/v1/audio/transcriptions`, `/api/v1/videos`.

## 4. Architecture — dual-encoder vs caption-then-embed

**Recommendation: dual-encoder as the primary index; captions as a secondary signal, not a replacement.**

CLIP-family encoders give one vector per image in a space shared with text queries — milliseconds per image, deterministic, exhaustively appliable to a whole library. Caption-then-embed costs seconds per image (100–1000x slower), is non-deterministic, and is lossy: anything the caption didn't mention is unsearchable. Since neither Ollama nor LM Studio exposes image embeddings, a dual-encoder means **an in-process encoder (Core ML / MLX / ONNX), not an inference-server call** — which is architecturally significant.

**Model options on Apple Silicon:**
- **MobileCLIP2** (Apple, Aug 2025) — S0 at 11.4M+63.4M params, 1.5+3.3ms latency, 71.5% IN-1k zero-shot; S4 matches SigLIP-SO400M/14 accuracy (81.9%) at 2x fewer params and 2.5x lower latency than DFN ViT-L/14 ([ml-mobileclip](https://github.com/apple/ml-mobileclip), [MobileCLIP2-S4](https://huggingface.co/apple/MobileCLIP2-S4)). Apple ships `apple/coreml-mobileclip` in Core ML form.
- **SigLIP 2** B/16-256 → 768-d. Core ML conversions exist ([batmac/ViT-B-16-SigLIP2-Image-CoreML](https://huggingface.co/batmac/ViT-B-16-SigLIP2-Image-CoreML)). Practical warning: the text tower uses Gemma2's 256K vocab — token embedding alone is 393MB fp16, ~565MB total, so the common pattern is **ship the image tower in Core ML, run the text tower on demand** (~50ms per query on MPS/CPU). Preprocessing is load-bearing: squash-resize to 256 (no center crop), `Normalize(0.5, 0.5)` → [-1,1]; using the usual [0,1] mapping silently costs ~0.024 cosine. Text must be padded to 64 tokens with pad token 0 and no attention mask.
- **MLX**: [mlx-embeddings](https://github.com/Blaizzy/mlx-embeddings) supports SigLIP (`siglip-so400m-patch14-384`), Qwen3-VL multimodal embedding/reranking, and Llama Nemotron VL. Reported throughput: **260+ images/sec on M4 Max** with MLX CLIP ([local-image-search](https://github.com/xaviedoanhduy/local-image-search)).

Use VLM captions for what dual-encoders are bad at: text-in-image, fine-grained counting, and generating human-readable classification labels. Store both.

## 5. Vector storage

| | sqlite-vec | LanceDB | usearch | FAISS |
|---|---|---|---|---|
| Algorithm | Brute force (exact) | IVF-PQ / HNSW | HNSW | flat/IVF/PQ/HNSW |
| Persistence | Single `.db` file | Directory (Lance columnar) | Index file | None — library only |
| Memory | Disk-backed via SQLite pager | Memory-mapped, >RAM OK | Full index in RAM | You build it |

- **sqlite-vec** — pure C, zero deps, MIT/Apache-2.0. GA v0.1.9 (2026-03-31) is **brute-force only**; IVF and DiskANN exist only in v0.1.10-alpha, no HNSW. `vec0` virtual tables support partition and metadata columns; binary and int8 quantization ([d-central survey](https://d-central.tech/self-hosted-vector-databases/)). Benchmarks put SQLite+FTS5+sqlite-vec at **0.1–1.0ms/query and ~5ms indexing — ~4x faster than LanceDB and ~40x faster than DuckDB** at small scale, with quality identical across engines once a reranker is applied ([sanityboard vecdb](https://sanityboard.lr7.dev/evals/vecdb)). Impractical above ~100k ([node-vector-bench](https://github.com/photostructure/node-vector-bench)).
- **LanceDB** — Rust, Apache-2.0. MVCC versioning/time travel, ACID, zero-copy column evolution (add an embedding column without rewriting rows), native Tantivy BM25 + vector hybrid with RRF, and an FM-index for substring search. Caveat for a desktop app: the **Node NAPI binding serializes every `add()` into one Arrow IPC buffer and concurrent adds block on an internal write lock** — no parallelism knobs exposed. Also needs per-scale `numPartitions`/`nprobes` tuning; not set-and-forget.
- **usearch** — HNSW, multi-threaded batch insert via C++ bindings, ~2–3x raw vector bytes peak build RAM, slow build / fast query ([M4 index comparison](https://llmmac.com/blog/articles/2026-llmmac-mac-vector-index-usearch-faiss-sqlitevec.html)).
- **FAISS** — deliberately library-only: no persistence, metadata, or serving layer. Only worth it for maximum index control.

**For a photo/video desktop app:** 100k images at 768-d fp32 is only ~300MB, and int8/binary quantization cuts that 4–32x — brute force is genuinely viable, and the exact-recall / nothing-to-corrupt / one-file-to-back-up properties matter more than QPS. Start with **sqlite-vec** colocated with EXIF/date/folder metadata so filters are plain SQL `WHERE` joins. Migrate to **LanceDB** when you exceed a few hundred thousand vectors or want hybrid caption/transcript BM25 fused with vectors in one engine (both speak Arrow, so it's a clean path). Note that several shipping apps skip vector libraries entirely: Sharkfin uses a contiguous in-memory cache and a single `vDSP_mmul` Accelerate call for the whole dot-product sweep ([Sharkfin](https://github.com/xplato/Sharkfin)); photomind stores packed float32 BLOBs in SQLite with numpy cosine ([photomind-mcp](https://github.com/davidcjw/photomind-mcp)).

## 6. Video

The converged pipeline across every serious local implementation:

1. `ffprobe` — duration/resolution/fps/codec
2. **PySceneDetect** `AdaptiveDetector` — scene boundaries. Two field-tested details: enforce a **2s minimum scene length** to filter compression flicker, and **fall back to uniform sampling if fewer than 10 scenes are detected** — continuous footage and screencasts have no hard cuts. Cap total frames (~150) ([ai-video-prepper pipeline](https://github.com/crimsonsunset/ai-video-prepper/blob/main/docs/system/pipeline-architecture.md))
3. ffmpeg/OpenCV keyframe extraction at boundaries
4. Image embedding per keyframe, stored with `(video_id, timestamp)` so hits are seekable
5. ffmpeg → WAV → **faster-whisper** (CTranslate2; faster than openai-whisper for equal quality on macOS CPU). Diarization locally and keyless via sherpa-onnx ([klaket](https://github.com/huseyinstif/klaket))
6. Per-scene OCR of on-screen text — cheap, high-value third signal

**Keep the modalities in separate indexes and merge at query time**, rather than fusing into one vector. SearchLightAI is the clearest reference: SigLIP2 768-d visual collection + all-MiniLM-L6-v2 384-d speech collection, queried as visual / speech / hybrid ([SearchLightAI](https://github.com/kiranbaby14/SearchLightAI)).

Performance note: `vllm-mlx` reports content-based prefix caching of vision embeddings (content-hashing images regardless of input format) giving **28x speedup on repeated image queries and 24.7x on 64-frame video analysis** on M4 Max ([arXiv 2601.19139](https://arxiv.org/html/2601.19139v2)) — the same hashing idea applies to re-indexing runs.

## 7. Apple-native

- **Foundation Models framework** (Swift; on-device + Private Cloud Compute). WWDC26 added **image inputs** to the on-device model — attachments from `UIImage`, `NSImage`, `CGImage`, Core Image types, `CVPixelBuffer`, and file URLs at any size (larger images cost more tokens). New built-in `BarcodeReaderTool` and `OCRTool`, plus a **Spotlight-powered search tool for fully local RAG**. It also now defines a Language Model protocol so third-party providers can conform via a Swift package ([WWDC26 241](https://developer.apple.com/videos/play/wwdc2026/241/), [developer.apple.com/machine-learning](https://developer.apple.com/machine-learning/)). **Caveat:** Apple describes it as a device-scale model optimized for summarization, extraction, and classification — explicitly not world knowledge ([WWDC25 286](https://developer.apple.com/videos/play/wwdc2025/286/)). Good for tagging and classification; weak for open-domain photo captions. Also gated on Apple Intelligence-eligible hardware, so it can't be the only path.
- **Vision framework** — free, no model download, no token cost: OCR, barcode, classification, plus the new **tap-to-segment** request; now available on watchOS ([WWDC26 237](https://developer.apple.com/videos/play/wwdc2026/237/)).
- **Core ML** — the delivery vehicle for CLIP/SigLIP on the Neural Engine. Model gallery has FastViT and MobileNetV2; `VNCoreMLRequest` handles crop/scale ([classifying-images](https://developer.apple.com/documentation/coreml/classifying-images-with-vision-and-core-ml)). ONNX Runtime with the CoreML execution provider is the viable alternative — Sharkfin runs a ~350MB vision model on CoreML EP and the ~250MB text model on CPU (CoreML has dynamic-shape issues on the text tower).
- **osxphotos** is the standard route into the Photos.app library (albums, EXIF, people, live photos) without reimplementing the database format.

**Cross-cutting conclusion:** the local inference servers (Ollama, LM Studio) can caption and classify but structurally cannot embed images. A semantic media search product therefore needs an in-process image encoder — Core ML or MLX MobileCLIP2/SigLIP 2 — with Ollama/LM Studio relegated to captioning, classification, and query understanding, and OpenRouter available as an optional cloud tier where its multimodal embedding models (Gemini Embedding 2, llama-nemotron-embed-vl) are the only hosted way to get image vectors.
