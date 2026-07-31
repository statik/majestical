# /// script
# requires-python = ">=3.11"
# dependencies = ["transformers==5.14.1", "torch==2.13.0", "pillow==12.3.0"]
# ///
"""Golden embeddings from the reference SigLIP 2 implementation.

Usage: uv run conformance/encoder/golden.py --revision <sha> --out golden.json
transformers is the pinned oracle (v5 resizes with torchvision's antialiased
bilinear — the exact behavior our Rust preprocessing must match). torch and
pillow are pinned too (not left to float) so a fresh CI runner resolving
these deps for the first time can't silently drift the oracle; versions are
still recorded in the output metadata as a cross-check.
"""

import argparse
import json
import pathlib

import PIL
import torch
import transformers
from PIL import Image
from transformers import AutoModel, AutoProcessor

TEXTS = [
    "a photo of a beach at sunset",
    "portrait of a golden retriever",
    "city skyline at night",
]
# Anchored to this file, not the cwd: `uv run` from outside the repo root
# would otherwise glob nothing and silently write a golden.json with an
# empty "images" map — a gate that trivially "passes" because it checks
# nothing.
REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
FIXTURES = REPO_ROOT / "crates/index/tests/fixtures"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--revision", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    model_id = "google/siglip2-base-patch16-256"
    processor = AutoProcessor.from_pretrained(model_id, revision=args.revision)
    model = AutoModel.from_pretrained(model_id, revision=args.revision)
    model.eval()
    out = {
        "meta": {
            "model": model_id,
            "revision": args.revision,
            "transformers": transformers.__version__,
            "torch": torch.__version__,
            "pillow": PIL.__version__,
        },
        "images": {},
        "texts": {},
        "token_ids": {},
    }
    pngs = sorted(FIXTURES.glob("*.png"))
    if not pngs:
        raise FileNotFoundError(
            f"no *.png fixtures found in {FIXTURES} — "
            "run `cargo run -p majestical-index --example gen_fixtures` first"
        )
    with torch.no_grad():
        for png in pngs:
            image = Image.open(png).convert("RGB")
            inputs = processor(images=image, return_tensors="pt")
            feats = model.get_image_features(**inputs)
            feats = feats.pooler_output if hasattr(feats, "pooler_output") else feats
            feats = feats / feats.norm(p=2, dim=-1, keepdim=True)
            out["images"][png.name] = feats[0].tolist()
        for text in TEXTS:
            inputs = processor(
                text=text,
                padding="max_length",
                max_length=64,
                truncation=True,
                return_tensors="pt",
            )
            out["token_ids"][text] = inputs["input_ids"][0].tolist()
            feats = model.get_text_features(**inputs)
            feats = feats.pooler_output if hasattr(feats, "pooler_output") else feats
            feats = feats / feats.norm(p=2, dim=-1, keepdim=True)
            out["texts"][text] = feats[0].tolist()
    pathlib.Path(args.out).write_text(json.dumps(out))
    print(f"golden embeddings -> {args.out}")


if __name__ == "__main__":
    main()
