#!/usr/bin/env python3
"""Cooperative LoRA fine-tune for the in-house trial: a real, resumable loop under mlx-guard.

The optimizer round trip replaces the WHOLE state dict (`Optimizer.state` setter), and
`apply_gradients` reads `self.step` unconditionally while `init` never re-adds `step` or
`learning_rate` (they are not parameters), so the saved optimizer file must carry both
(verified `optimizers.py`: `Optimizer.__init__` seeds `self._state = {"step": mx.array(0,
mx.uint64)}`; `apply_gradients` does `self.state["step"] = self.step + 1` unconditionally,
reading the property back from `self.state["step"]`; the `state` setter only flips
`_initialized = False` and swaps `_state` wholesale — it never re-seeds `step`).

Measured on mlx 0.32.2 (Darwin arm64): `mx.save_safetensors` accepts a rank-0 array
unchanged — round-tripping `mx.array(3, mx.uint64)` and `mx.array(1e-2, mx.float32)` through
`save_safetensors`/`load` preserves shape `()` and dtype exactly (independently re-verified in
the trial venv). No reshape-to-`(1,)` workaround was needed; `save_state`/`load_state` below
flatten and restore the optimizer's whole state dict as-is, `step` and `learning_rate` included.
"""

import argparse
import contextlib
import json
import os
import sys
import time
from collections.abc import Callable, Mapping
from dataclasses import asdict, dataclass
from pathlib import Path

import mlx.core as mx
import mlx.optimizers as optim
import numpy as np

# `mlx.nn`'s public namespace re-exports `Module`/`value_and_grad` through a two-level
# wildcard-then-named-import chain (`mlx/nn/__init__.py` -> `from .layers import *` ->
# `mlx/nn/layers/__init__.py` -> `from .layers.base import (Module, ...)`), which mypy's
# `--strict` (`no_implicit_reexport`) does not see through — `mlx.nn.Module` type-checks as
# `Any` (measured: mlx 0.32.2, mypy 2.3.1). `mlx.optimizers.Optimizer`/`Adam` are unaffected
# because they are defined directly in the module a single wildcard import pulls from.
# Importing the defining submodules directly sidesteps the gap with no `type: ignore`.
from mlx.nn.layers.base import Module
from mlx.nn.utils import value_and_grad
from mlx.utils import tree_flatten, tree_unflatten

_PROCESS_START = time.perf_counter()
PAD_TO = 32
ADAPTERS, OPTIMIZER, META = "adapters.safetensors", "optimizer.safetensors", "meta.json"


@dataclass(frozen=True, slots=True)
class LoopConfig:
    """Everything defining the workload; recorded (path-free) in every checkpoint's meta.json."""

    model: str = "mlx-community/Qwen2.5-0.5B-Instruct-4bit"
    dataset: str = "mlx-community/wikisql"
    examples: int = 512
    batch_size: int = 4
    max_seq_length: int = 512
    num_layers: int = 16
    rank: int = 8
    scale: float = 20.0
    dropout: float = 0.0
    learning_rate: float = 1e-5
    seed: int = 0


def make_batches(
    token_lists: list[list[int]], batch_size: int, max_seq_length: int
) -> list[tuple[mx.array, mx.array]]:
    """Length-sort and pad like mlx_lm's iterate_batches.

    Width = min(1 + 32 * ceil(max_len / 32), max_seq_length).
    """
    order = sorted(range(len(token_lists)), key=lambda i: len(token_lists[i]))
    groups = [order[i : i + batch_size] for i in range(0, len(order) - batch_size + 1, batch_size)]
    batches = []
    for group in groups:
        seqs = [token_lists[j][:max_seq_length] for j in group]
        lengths = [len(s) for s in seqs]
        width = min(1 + PAD_TO * ((max(lengths) + PAD_TO - 1) // PAD_TO), max_seq_length)
        arr = np.zeros((len(seqs), width), np.int32)
        for row, seq in enumerate(seqs):
            arr[row, : len(seq)] = seq
        batches.append((mx.array(arr), mx.array([(0, n) for n in lengths])))
    return batches


def batch_for_step(
    batches: list[tuple[mx.array, mx.array]], step: int, seed: int
) -> tuple[mx.array, mx.array]:
    """Order deterministically: epoch e uses permutation(seed + e); position is step within it."""
    epoch, position = divmod(step, len(batches))
    order = np.random.default_rng(seed + epoch).permutation(len(batches))
    return batches[int(order[position])]


def _fsync_write(path: Path, data: bytes) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    with tmp.open("wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)


def _save_safetensors_atomic(path: Path, tensors: dict[str, mx.array]) -> None:
    """Write ``tensors`` to a fsynced temp file, then ``os.replace`` it onto ``path``.

    The temp name keeps the ``.safetensors`` suffix: measured on mlx 0.32.2,
    ``mx.save_safetensors`` appends that extension to any path that does not already end with
    it, so a ``<name>.safetensors.tmp`` temp name would silently land at
    ``<name>.safetensors.tmp.safetensors`` instead of the file this function fsyncs and replaces.
    """
    tmp = path.with_name(f"{path.stem}.tmp{path.suffix}")
    mx.save_safetensors(str(tmp), tensors)
    fd = os.open(tmp, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, path)


def save_state(
    directory: Path,
    model: Module,
    optimizer: optim.Optimizer,
    *,
    step: int,
    extra: dict[str, object],
) -> int:
    """Save adapters + optimizer state + meta, each fsynced, then the directory.

    Returns bytes on disk.
    """
    mx.clear_cache()  # hand MLX's retained buffers back before writing (PYTHON_ADAPTER.md:122-126)
    trainable = model.trainable_parameters()  # type: ignore[no-untyped-call]
    _save_safetensors_atomic(directory / ADAPTERS, dict(tree_flatten(trainable)))
    _save_safetensors_atomic(directory / OPTIMIZER, dict(tree_flatten(optimizer.state)))
    _fsync_write(directory / META, json.dumps({"step": step, **extra}, sort_keys=True).encode())
    fd = os.open(directory, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    return sum(p.stat().st_size for p in directory.iterdir() if p.is_file())


def load_state(directory: Path, model: Module, optimizer: optim.Optimizer) -> int:
    """Restore adapters and optimizer state; returns the saved step (the next batch index)."""
    model.load_weights(str(directory / ADAPTERS), strict=False)
    loaded = mx.load(str(directory / OPTIMIZER))
    if not isinstance(loaded, dict):
        raise TypeError(f"{directory / OPTIMIZER} did not decode to a flat safetensors dict")
    optimizer.state = tree_unflatten(list(loaded.items()))
    return int(json.loads((directory / META).read_text())["step"])


def train_steps(
    model: Module,
    optimizer: optim.Optimizer,
    batches: list[tuple[mx.array, mx.array]],
    *,
    start_step: int,
    total_steps: int,
    seed: int,
    after_step: Callable[[int], None],
) -> None:
    """Evaluate one step at a time.

    `after_step(step)` is the poll boundary — `worker.poll()` lives there.
    """
    from mlx_lm.tuner.trainer import default_loss

    loss_and_grad = value_and_grad(model, default_loss)
    for step in range(start_step, total_steps):
        batch, lengths = batch_for_step(batches, step, seed)
        (loss, _ntoks), grads = loss_and_grad(model, batch, lengths)
        optimizer.update(model, grads)
        parameters = model.parameters()  # type: ignore[no-untyped-call]
        # materialize before the poll so the checkpoint callback allocates nothing
        mx.eval(parameters, optimizer.state, loss)
        after_step(step + 1)


def worker(args: argparse.Namespace) -> None:
    """Run the supervised side: real LoRA training on wikisql that saves state when asked."""
    import mlx_guard
    from datasets import load_dataset  # type: ignore[import-untyped]
    from mlx_lm import load
    from mlx_lm.tuner.utils import linear_to_lora_layers

    cfg = LoopConfig()
    # load()'s declared return type is a 2-tuple or a 3-tuple depending on `return_config`,
    # which defaults to False and is never passed True here, so the runtime value is always
    # the 2-tuple; mypy still has to account for the union's 3-tuple branch.
    model, tokenizer = load(cfg.model)  # type: ignore[misc]
    model.freeze()
    lora_config = {"rank": cfg.rank, "scale": cfg.scale, "dropout": cfg.dropout}
    linear_to_lora_layers(model, cfg.num_layers, lora_config)
    rows = load_dataset(cfg.dataset, split="train").select(range(cfg.examples))
    eos = tokenizer.eos_token_id
    token_lists = [[*tokenizer.encode(r["text"]), eos] for r in rows]
    batches = make_batches(token_lists, cfg.batch_size, cfg.max_seq_length)
    optimizer = optim.Adam(learning_rate=cfg.learning_rate)
    mx.eval(model.parameters())

    state = {"step": 0}
    restored = 0
    if args.resume_from is not None:
        state["step"] = load_state(Path(args.resume_from), model, optimizer)
        restored = state["step"]

    def save_checkpoint(request: mlx_guard.CheckpointRequest) -> mlx_guard.CheckpointResponse:
        root = Path(args.checkpoints)
        root.mkdir(mode=0o700, exist_ok=True)
        directory = root / str(request.request_id)
        directory.mkdir(mode=0o700, parents=False, exist_ok=True)
        started = time.perf_counter()
        written = save_state(
            directory,
            model,
            optimizer,
            step=state["step"],
            extra={"request_id": request.request_id, "config": asdict(cfg)},
        )
        elapsed = time.perf_counter() - started
        print(
            f"checkpoint {request.request_id}: step {state['step']}, {written} bytes, "
            f"{elapsed:.3f}s",
            file=sys.stderr,
        )
        artifact = mlx_guard.CheckpointArtifact(
            mlx_guard.CheckpointArtifactKind.DIRECTORY, size_bytes=written
        )
        return mlx_guard.CheckpointResponse.completed(artifact)

    worker_handle = mlx_guard.CheckpointWorker.connect(save_checkpoint)
    step_times: list[float] = []
    setup_seconds = 0.0
    last = time.perf_counter()

    def after_step(step: int) -> None:
        nonlocal last, setup_seconds
        now = time.perf_counter()
        step_times.append(now - last)
        if len(step_times) == 1:
            # everything before the first completed step, connect() included
            setup_seconds = now - _PROCESS_START
        last = now
        state["step"] = step
        if worker_handle is not None:
            worker_handle.poll()

    with worker_handle if worker_handle is not None else contextlib.nullcontext():
        train_steps(
            model,
            optimizer,
            batches,
            start_step=state["step"],
            total_steps=args.total_steps,
            seed=cfg.seed,
            after_step=after_step,
        )
    if args.calibrate:  # C0: the timing and memory facts C1/C2's parameters derive from
        root = Path(args.checkpoints)
        root.mkdir(mode=0o700, exist_ok=True)
        calibrate_dir = root / "calibrate"
        calibrate_dir.mkdir(mode=0o700, exist_ok=True)
        started = time.perf_counter()
        written = save_state(calibrate_dir, model, optimizer, step=state["step"], extra={})
        print(
            json.dumps(
                {
                    "steps": len(step_times),
                    "setup_seconds": setup_seconds,
                    "first_step_seconds": step_times[0],
                    "step_seconds_max": max(step_times[1:] or step_times),
                    "step_seconds_median": sorted(step_times)[len(step_times) // 2],
                    "save_seconds": time.perf_counter() - started,
                    "save_bytes": written,
                    "mlx_peak_memory_bytes": mx.get_peak_memory(),
                }
            )
        )
    if args.resume_from is not None:
        marker = {
            "resumed_from_request_id": int(Path(args.resume_from).name),
            "restored_step": restored,
            "final_step": state["step"],
        }
        _fsync_write(Path(args.marker), json.dumps(marker).encode())


def launch(args: argparse.Namespace) -> None:
    """Launch the worker under mlx-guard, PYTHON_ADAPTER.md's pattern.

    C2 additionally executes PYTHON_API.md's resume match.
    """
    import mlx_guard

    attempt = Path.cwd()
    (attempt / "reports").mkdir(mode=0o700, exist_ok=True)
    command = [
        sys.executable,
        __file__,
        "train",
        "--checkpoints",
        "checkpoints",
        "--total-steps",
        str(args.total_steps),
    ]
    wall_time_ms: int | None = args.wall_time_ms
    if args.resume_report is not None:  # C2: the documented match, verbatim from PYTHON_API.md
        if args.c1_checkpoints is None:
            raise SystemExit("--resume-report requires --c1-checkpoints")
        report = mlx_guard.load_report(Path(args.resume_report))
        checkpoint = report.payload["checkpoint"]
        # payload values are frozen via MappingProxyType (see _client.py's _freeze_json), which
        # is a Mapping but NOT a dict subclass — `isinstance(checkpoint, dict)` would always be
        # False here and mask every resumable checkpoint as unresumable.
        assert isinstance(checkpoint, Mapping)  # noqa: S101 — payload is JsonValue; narrow before .get
        status, request_id = checkpoint["status"], checkpoint.get("request_id")
        if request_id is None or status not in ("acknowledged_unverified_durability", "timed_out"):
            raise SystemExit(f"nothing to resume from: {status} / {request_id}")
        command += [
            "--resume-from",
            f"{args.c1_checkpoints}/{request_id}",
            "--marker",
            "resume-marker.json",
        ]
        wall_time_ms = None
    result = mlx_guard.run(
        mlx_guard.RunConfig(
            command=tuple(command),
            report=attempt / "reports" / f"{args.arm}.json",
            # limit_C from C0: 125% of the observed OS peak (bands.headroom_limit), never fires
            max_footprint_bytes=args.max_footprint_bytes,
            wall_time_ms=wall_time_ms,
            cwd=attempt,
            checkpoint_timeout_ms=args.checkpoint_timeout_ms,
        )
    )
    print(
        json.dumps(
            {
                "returncode": result.returncode,
                "outcome": result.report.outcome.kind.value,
                "checkpoint": result.report.payload["checkpoint"],
            },
            default=str,
        )
    )


def _build_parser() -> argparse.ArgumentParser:
    """Build the frozen CLI: `train` runs the worker, `launch` runs it under mlx-guard."""
    parser = argparse.ArgumentParser(
        description="The in-house trial's resumable LoRA loop: the supervised worker (`train`) "
        "and the adapter that launches it under mlx-guard (`launch`)."
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    train_parser = subparsers.add_parser("train", help="run the supervised LoRA worker")
    train_parser.add_argument("--checkpoints", required=True, help="checkpoint root directory")
    train_parser.add_argument("--total-steps", required=True, type=int)
    train_parser.add_argument(
        "--calibrate", action="store_true", help="also save+print C0's timing/memory JSON line"
    )
    train_parser.add_argument("--resume-from", default=None, help="a directory save_state wrote")
    train_parser.add_argument("--marker", default=None, help="where to write the resume marker")
    train_parser.set_defaults(func=worker)

    launch_parser = subparsers.add_parser("launch", help="run the worker under mlx-guard")
    launch_parser.add_argument("--arm", required=True, choices=("c1", "c2"))
    launch_parser.add_argument("--total-steps", required=True, type=int)
    launch_parser.add_argument("--max-footprint-bytes", required=True, type=int)
    launch_parser.add_argument("--wall-time-ms", default=None, type=int)
    launch_parser.add_argument("--checkpoint-timeout-ms", required=True, type=int)
    launch_parser.add_argument("--resume-report", default=None, help="a completed C1 report.json")
    launch_parser.add_argument("--c1-checkpoints", default=None, help="C1's checkpoint root")
    launch_parser.set_defaults(func=launch)

    return parser


def main(argv: list[str] | None = None) -> None:
    """CLI entry point: dispatch to `train` (the worker) or `launch` (the adapter)."""
    parser = _build_parser()
    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
