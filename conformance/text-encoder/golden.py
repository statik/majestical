# /// script
# requires-python = ">=3.11"
# dependencies = ["sentence-transformers==5.6.1", "torch==2.13.0"]
# ///
"""Golden embeddings from the pinned sentence-transformers reference.

Usage: uv run conformance/text-encoder/golden.py --revision <sha> --out golden.json

sentence-transformers is the pinned oracle for all-MiniLM-L6-v2 (mean-pooled,
L2-normalized 384-d embeddings) — the exact behavior our Rust `TextEncoder`
must match. torch is pinned too (not left to float) so a fresh CI runner
resolving it for the first time can't silently drift the oracle; both
versions are recorded in the output metadata as a cross-check.
"""

import argparse
import json

import sentence_transformers
import torch
from sentence_transformers import SentenceTransformer

FIXTURES = [
    "a red barn at dusk",
    "we discussed the quarterly budget and costs",
    "TIMECODE 01:02:03 dropped frame",
    "ümläuts and 日本語 mixed with english",
    " ".join(["repetition"] * 300),  # long input: exercises truncation
    "",  # empty string: exercises the all-special-tokens path
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--revision", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    model_id = "sentence-transformers/all-MiniLM-L6-v2"
    model = SentenceTransformer(model_id, revision=args.revision)
    print(f"model.max_seq_length = {model.max_seq_length}")
    vectors = model.encode(FIXTURES, normalize_embeddings=True).tolist()
    out = {
        "meta": {
            "model": model_id,
            "revision": args.revision,
            "sentence_transformers": sentence_transformers.__version__,
            "torch": torch.__version__,
        },
        "fixtures": FIXTURES,
        "vectors": vectors,
    }
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(out, handle)
    print(f"golden embeddings -> {args.out}")


if __name__ == "__main__":
    main()
