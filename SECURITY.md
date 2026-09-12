# Security policy

## Supported version

Security fixes are provided for the latest released minor version (see the changelog). Pre-release
branches and older development snapshots are not supported. The platform and interpreter boundary is
documented in the [support matrix](https://github.com/IonDen/mlx-guard/blob/main/docs/SUPPORT.md).

## Reporting a vulnerability

Use the repository's private **Report a vulnerability** form under the Security tab. Include the
affected version, macOS and hardware version, a minimal reproduction, and the expected impact. Do
not attach reports, commands, tokens, model identifiers, prompts, or private paths unless they have
been reduced to non-sensitive test data.

If private vulnerability reporting is unavailable, open a public issue containing no exploit or
secret details and ask the maintainer to establish a private channel. Please allow 14 days for an
initial response before public disclosure.

## Operational security

Treat supervised commands as trusted same-user programs. Keep report directories owner-only
(`0700`), keep report files local unless reviewed, choose limits from observed safe runs, and verify
the installed package with `mlx_guard.binary_version()`. See the
[threat model](https://github.com/IonDen/mlx-guard/blob/main/docs/THREAT_MODEL.md) for what the supervisor
does and does not defend.
