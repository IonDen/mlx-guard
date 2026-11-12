# Earlier runs, 2026-11-09

Four 30-minute runs of the 50 ms sampler test with 16 idle members, all taken on 2026-11-09 on
macOS 27.2 (26B123) on the M1 Max 32 GB reference host. All four are debug builds. The 2.0% bound
the test asserted at the time failed on all four. The two captures stopped with a failing chunk
(a non-zero exit). Their JSON files predate the `build_profile` field, so the build type is stated
here and not in the files.

The exit statuses and the sleep noted in the table come from the run logs and the macOS power log
on the host, and neither log is part of this bundle. The v0.3.1-tag build can be rebuilt from the
`v0.3.1` tag. The other three were built from earlier states of the 0.5.0 branch, whose commits
are not in this release's history, so there is no commit to fetch for them.

| Run | File | Reading |
|---|---|---|
| Calibration capture | [calibration-capture.json](calibration-capture.json) | 2.5823%, stopped by the 2.0% bound |
| Soak capture | [soak-capture.json](soak-capture.json) | 2.7886%, stopped by the 2.0% bound |
| Build from the v0.3.1 tag | [v0.3.1-tag-build.json](v0.3.1-tag-build.json) | 2.829% |
| Build from the 0.5.0 code | [v0.5.0-code-build.json](v0.5.0-code-build.json) | 2.804%, interrupted near its end by about 15 minutes of system sleep |

The percentage is the CPU time of every thread of the test process divided by the test's own
elapsed time, which excludes system sleep.
