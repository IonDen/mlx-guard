# mlx-guard

External runtime safety supervision for MLX workloads on Apple Silicon.

`mlx-guard` is planned as an external, application-neutral circuit breaker for MLX commands. It will
observe OS-accounted macOS process footprint from a separate native supervisor, request an optional
cooperative checkpoint, and escalate against an explicitly configured limit. It reduces risk; it
cannot guarantee that polling beats every allocation spike or system-wide failure.

Planned command shape:

```bash
mlx-guard run --max-footprint 26G -- python train.py
```

The project is in the research and planning phase. MetalGuard already covers adjacent in-application
MLX safety and recovery. The first gate therefore decides build-versus-contribute, validates demand,
and proves that an unprivileged parent can measure and control its supported process boundary without
intentionally endangering the Mac.

Repository work is governed by [AGENTS.md](AGENTS.md). The dated North Star and granular backlog
are linked there and in the local `CLAUDE.md`.

Independent community project; not affiliated with or endorsed by Apple.

## Licence

Apache License 2.0. The licence permits commercial use without royalties or mandatory payment.
Commercial opportunities, if the project earns adoption, are support, integration, hosted
observability, and enterprise services around the open-source core.
