"""Round-trip test for the trial's resumable loop. Tiny model, no supervisor, milliseconds."""

from pathlib import Path

import mlx.core as mx
import mlx.nn as nn
import mlx.optimizers as optim
import resumable_lora as rl  # scripts/trial on sys.path via conftest.py
from mlx.utils import tree_flatten
from mlx_lm.tuner.lora import LoRALinear

VOCAB, DIM, SEED = 32, 8, 7

# mlx.nn's public namespace re-exports Module/Embedding/Linear through a two-level
# wildcard-then-named-import chain that mypy's --strict (no_implicit_reexport) does not see
# through, so `nn.Module`/`nn.Embedding`/`nn.Linear` type-check as Any here (measured: mlx
# 0.32.2, mypy 2.3.1; `mlx.optimizers.Adam` is unaffected — see resumable_lora.py's own
# docstring-adjacent comment for the mechanism). The ignores below are that gap, not a bug.


class TinyLM(nn.Module):  # type: ignore[name-defined,misc]
    """Embedding → Linear (LoRA'd) → vocab logits, so default_loss applies unchanged."""

    def __init__(self) -> None:
        super().__init__()
        self.embed = nn.Embedding(VOCAB, DIM)  # type: ignore[attr-defined]
        self.proj = nn.Linear(DIM, DIM)  # type: ignore[attr-defined]
        self.out = nn.Linear(DIM, VOCAB)  # type: ignore[attr-defined]

    def __call__(self, tokens: mx.array) -> mx.array:
        return self.out(self.proj(self.embed(tokens)))  # type: ignore[no-any-return]


def build() -> tuple[TinyLM, optim.Adam]:
    mx.random.seed(SEED)
    model = TinyLM()
    model.freeze()
    model.proj = LoRALinear.from_base(model.proj, r=2, dropout=0.0, scale=20.0)
    return model, optim.Adam(learning_rate=1e-2)


def batches() -> list[tuple[mx.array, mx.array]]:
    mx.random.seed(SEED + 1)
    tokens = [mx.random.randint(1, VOCAB, (n,)).tolist() for n in (5, 7, 6, 9, 4, 8)]
    return rl.make_batches(tokens, batch_size=2, max_seq_length=16)  # type: ignore[arg-type]


def flat(model: TinyLM) -> dict[str, mx.array]:
    return dict(tree_flatten(model.trainable_parameters()))


def test_resume_matches_uninterrupted_training(tmp_path: Path) -> None:
    """Bug: load_state restores adapters but not optimizer state → Adam moments restart at
    zero and the parameters after step 5 differ; or load_state returns 0 → the batch order
    replays from the start."""
    data = batches()
    model_a, opt_a = build()
    rl.train_steps(
        model_a, opt_a, data, start_step=0, total_steps=5, seed=SEED, after_step=lambda _s: None
    )

    model_b, opt_b = build()
    rl.train_steps(
        model_b, opt_b, data, start_step=0, total_steps=3, seed=SEED, after_step=lambda _s: None
    )
    rl.save_state(tmp_path, model_b, opt_b, step=3, extra={"note": "test"})

    model_c, opt_c = build()
    step = rl.load_state(tmp_path, model_c, opt_c)
    assert step == 3
    rl.train_steps(
        model_c, opt_c, data, start_step=step, total_steps=5, seed=SEED, after_step=lambda _s: None
    )

    for key, value in flat(model_a).items():
        assert mx.allclose(value, flat(model_c)[key], atol=0, rtol=0).item(), key


def test_save_state_is_path_free_and_reports_bytes(tmp_path: Path) -> None:
    """Bug: meta.json records the absolute checkpoint directory (a home path leaks into
    evidence)."""
    model, opt = build()
    rl.train_steps(
        model, opt, batches(), start_step=0, total_steps=1, seed=SEED, after_step=lambda _s: None
    )
    written = rl.save_state(tmp_path, model, opt, step=1, extra={})
    meta = (tmp_path / "meta.json").read_text()
    assert str(tmp_path) not in meta and "/Users/" not in meta
    assert written == sum(p.stat().st_size for p in tmp_path.iterdir() if p.is_file())


def test_batch_for_step_is_deterministic_and_covers_epochs() -> None:
    """Bug: `step % len(batches)` without reseeding per epoch → epoch 2 repeats epoch 1's
    order exactly."""
    data = batches()
    first = [rl.batch_for_step(data, s, SEED)[0].tolist() for s in range(len(data))]
    second = [rl.batch_for_step(data, s, SEED)[0].tolist() for s in range(len(data))]
    assert first == second
    epoch2 = [rl.batch_for_step(data, s, SEED)[0].tolist() for s in range(len(data), 2 * len(data))]
    assert sorted(map(str, epoch2)) == sorted(map(str, first)) and epoch2 != first
