# Demo Assets

Scripted terminal recordings made with [vhs](https://github.com/charmbracelet/vhs)
(tapes committed next to their GIFs, so they can be re-rendered whenever the
CLI output changes).

## Re-recording

```bash
# Requires: vhs + ttyd on PATH, a running fhd, and the demo project from the tape
brew install vhs          # or: go install github.com/charmbracelet/vhs@latest
vhs demo-build.tape
vhs demo-top.tape
```

The tapes drive a local `fhd` (started separately) and a scratch project at
`/tmp/farhand-demo/app` with a `.farhand.yaml` pointing at `127.0.0.1:9877`.

## Files

| File | Shows |
| :--- | :--- |
| `demo-build.gif` | First run: full delta sync (11 files / 543 bytes), live remote compile, artifact pull. Second run: `0 files to transfer` — the persistent workspace + CAS story. |
| `demo-top.tape/.gif` | `fh top --once` snapshot and `fh agent info` host telemetry with gauges. |

Keep GIFs under ~1 MB: `Set Framerate 12`, short sleeps, and re-record rather
than re-encoding.