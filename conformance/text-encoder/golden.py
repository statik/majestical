# /// script
# requires-python = ">=3.11"
# dependencies = ["sentence-transformers==5.6.1"]
# ///
"""Golden embeddings from the pinned sentence-transformers reference.

Usage: uv run conformance/text-encoder/golden.py --revision <sha> --out golden.json

sentence-transformers is the pinned oracle for all-MiniLM-L6-v2 (mean-pooled,
L2-normalized 384-d embeddings) — the exact behavior our Rust `TextEncoder`
must match.
"""

import argparse
import json

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
    model = SentenceTransformer(
        "sentence-transformers/all-MiniLM-L6-v2", revision=args.revision
    )
    print(f"model.max_seq_length = {model.max_seq_length}")
    vectors = model.encode(FIXTURES, normalize_embeddings=True).tolist()
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump({"fixtures": FIXTURES, "vectors": vectors}, handle)
    print(f"golden embeddings -> {args.out}")


if __name__ == "__main__":
    main()
