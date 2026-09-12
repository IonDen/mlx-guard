r"""Summarize a folder of Markdown documents with a local LLM, one page at a time.

This is the workload behind TUTORIAL.md. It is deliberately ordinary: load a small model with
mlx-lm, cut the documents into pages, ask for a short summary of each page, write the summaries
to a JSON file. It also carries one deliberate, realistic bug: with ``--keep-caches`` (the
default) it holds on to every page's prompt cache "for the follow-up question pass", so the
process footprint grows with every page and never comes back down.

Run it from a fresh environment::

    uv run --no-project --with mlx-lm==0.31.3 --with mlx-guard==0.2.0 python \
        examples/tutorial/summarize_docs.py --progress progress.json

Under ``mlx-guard run`` the script answers cooperative checkpoint requests by saving its progress
file, and ``--resume`` continues from that file.
"""

import argparse
import json
import sys
import time
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import mlx_guard

PARAGRAPH_BREAK = "\n\n"
SYSTEM_PROMPT = "Summarize the page in two plain sentences. Reply with the summary only."


def paginate(text: str, max_chars: int) -> list[str]:
    """Split ``text`` into pages of at most ``max_chars`` characters.

    Pages break on blank lines when a paragraph fits; a paragraph longer than a page is cut
    hard. Concatenating the pages reproduces ``text`` exactly.
    """
    if max_chars < 1:
        raise ValueError("max_chars must be at least 1")
    paragraphs: list[str] = []
    rest = text
    while rest:
        cut = rest.find(PARAGRAPH_BREAK)
        if cut == -1:
            paragraphs.append(rest)
            break
        paragraphs.append(rest[: cut + len(PARAGRAPH_BREAK)])
        rest = rest[cut + len(PARAGRAPH_BREAK) :]

    pages: list[str] = []
    current = ""
    for paragraph in paragraphs:
        if len(paragraph) > max_chars:
            if current:
                pages.append(current)
                current = ""
            pages.extend(paragraph[i : i + max_chars] for i in range(0, len(paragraph), max_chars))
            continue
        if current and len(current) + len(paragraph) > max_chars:
            pages.append(current)
            current = ""
        current += paragraph
    if current:
        pages.append(current)
    return pages


def load_progress(path: Path) -> dict[int, str]:
    """Return the summaries saved so far, keyed by page index; empty when there is no file."""
    if not path.exists():
        return {}
    raw = json.loads(path.read_text())
    summaries = raw["summaries"]
    return {int(index): summaries[index] for index in sorted(summaries, key=int)}


def save_progress(path: Path, summaries: dict[int, str]) -> int:
    """Write the summaries atomically and return the file size in bytes."""
    payload = {"summaries": {str(index): summaries[index] for index in sorted(summaries)}}
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2))
    tmp.replace(path)
    return path.stat().st_size


def remaining_pages(total: int, done: dict[int, str]) -> list[int]:
    """Page indices still to summarize, in order."""
    return [index for index in range(total) if index not in done]


def collect_documents(docs: Sequence[Path]) -> str:
    """Concatenate the Markdown files under the given paths, each under a heading."""
    files: list[Path] = []
    for entry in docs:
        if entry.is_dir():
            files.extend(sorted(p for p in entry.rglob("*.md") if p.is_file()))
        elif entry.is_file():
            files.append(entry)
    parts = [f"# Document: {file.name}\n\n{file.read_text()}\n\n" for file in files]
    return "".join(parts)


def connect_checkpoint(
    progress: Path, summaries: dict[int, str]
) -> "mlx_guard.CheckpointWorker | None":
    """Answer mlx-guard checkpoint requests by saving the progress file.

    Returns ``None`` when mlx-guard is not installed or did not launch this process.
    """
    try:
        import mlx_guard
    except ImportError:
        return None

    def save_checkpoint(request: mlx_guard.CheckpointRequest) -> mlx_guard.CheckpointResponse:
        size = save_progress(progress, summaries)
        print(f"checkpoint {request.request_id}: saved {len(summaries)} summaries", flush=True)
        return mlx_guard.CheckpointResponse.completed(
            mlx_guard.CheckpointArtifact(mlx_guard.CheckpointArtifactKind.FILE, size)
        )

    return mlx_guard.CheckpointWorker.connect(save_checkpoint)


def _parse(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", maxsplit=1)[0])
    parser.add_argument("--docs", nargs="+", type=Path, default=[Path("README.md"), Path("docs")])
    parser.add_argument("--model", default="mlx-community/Llama-3.2-3B-Instruct-4bit")
    parser.add_argument("--max-chars", type=int, default=3200, help="page size in characters")
    parser.add_argument("--max-pages", type=int, default=None, help="stop after this many pages")
    parser.add_argument("--max-tokens", type=int, default=60, help="summary length")
    parser.add_argument("--progress", type=Path, default=Path("progress.json"))
    parser.add_argument("--resume", action="store_true", help="skip pages already in --progress")
    parser.add_argument(
        "--keep-caches",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="keep every page's prompt cache for a later follow-up pass (the bug)",
    )
    parser.add_argument(
        "--checkpoint",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="answer mlx-guard checkpoint requests by saving --progress",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """Summarize every page, answering checkpoint requests when supervised."""
    args = _parse(sys.argv[1:] if argv is None else argv)

    import mlx.core as mx  # type: ignore[import-not-found]
    from mlx_lm import generate, load  # type: ignore[import-not-found]
    from mlx_lm.models.cache import make_prompt_cache  # type: ignore[import-not-found]

    text = collect_documents(args.docs)
    pages = paginate(text, args.max_chars)
    if args.max_pages is not None:
        pages = pages[: args.max_pages]
    summaries = load_progress(args.progress) if args.resume else {}
    todo = remaining_pages(len(pages), summaries)
    print(f"{len(pages)} pages, {len(todo)} to do, keep_caches={args.keep_caches}", flush=True)

    worker = connect_checkpoint(args.progress, summaries) if args.checkpoint else None

    model, tokenizer = load(args.model)
    caches: dict[int, object] = {}
    started = time.monotonic()
    for index in todo:
        page_started = time.monotonic()
        messages = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": pages[index]},
        ]
        prompt = tokenizer.apply_chat_template(messages, add_generation_prompt=True)
        cache = make_prompt_cache(model)
        summaries[index] = generate(
            model, tokenizer, prompt, max_tokens=args.max_tokens, prompt_cache=cache
        ).strip()
        if args.keep_caches:
            caches[index] = cache
        active_gib = mx.get_active_memory() / 2**30
        print(
            f"page {index + 1}/{len(pages)} in {time.monotonic() - page_started:.1f}s; "
            f"mlx active {active_gib:.2f} GiB; caches held {len(caches)}",
            flush=True,
        )
        if worker is not None:
            worker.poll()

    save_progress(args.progress, summaries)
    if worker is not None:
        worker.close()
    elapsed = time.monotonic() - started
    print(f"done: {len(summaries)} summaries in {elapsed:.0f}s -> {args.progress}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
