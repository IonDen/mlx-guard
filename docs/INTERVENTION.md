# Intervention execution

The native intervention engine executes policy contract version 1. It accepts ordered monotonic
events, asks the pure policy machine for actions, and executes only those actions. The platform
adapter does not calculate thresholds, rebuild deadlines, or choose an escalation step.

## Targets

A checkpoint request writes the bounded request frame first, then sends the configured checkpoint
signal to the negotiated endpoint PID. It never broadcasts that signal to the process group. TERM,
KILL, and forwarded terminal signals target the validated owned process group.

Signal delivery has typed results: delivered, process missing, permission denied, or failed. A
missing process group means there is no remaining target and is not treated as a signal failure. A
missing checkpoint endpoint is a checkpoint failure because it cannot acknowledge the request.

## Deadlines and failures

Checkpoint and TERM deadlines come directly from the policy machine. A blocked worker cannot extend
either deadline. A checkpoint setup or delivery failure returns to policy, which selects TERM. A
TERM or forwarded-signal failure returns to policy and selects KILL. If KILL fails, the machine enters
a terminal supervisor-error state instead of retrying forever.

A matching acknowledgement must carry the unpredictable active request ID and per-run nonce. Wrong,
spoofed, late, duplicate, post-cancel, and malformed frames remain diagnostic rejections and cannot
suppress termination. This is possession-bound cooperation within the documented trusted-workload
model, not authentication against malicious descendants.

## Observation evidence

The runtime keeps observing after an attempt until the owned group is empty, the next policy
deadline arrives, or a terminal supervisor error occurs. Each retained attempt records:

- its policy-selected action, monotonic request time, typed result, and baseline footprint;
- latency to the first later valid footprint observation;
- latency to the first later observed footprint decrease;
- latency to an observed empty process group.

Detailed evidence is limited to the latest 64 attempts. Aggregate attempt and dropped-record counts
remain available. The engine also records the maximum sampled threshold overshoot. Signal success,
a lower footprint sample, and an empty process group do not prove immediate Metal-driver memory
reclamation.
