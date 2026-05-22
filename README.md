# GPU Trace Viewer

Rust terminal and web viewer for `gpu_trace_stats.sqlite` databases produced by the Python importer.

## Install

```bash
cargo install --path . --force
```

This installs `gpu-trace-viewer` into `~/.cargo/bin`.

## Commands

```bash
gpu-trace-viewer --db ../gpu_trace_stats.sqlite
gpu-trace-viewer --db ../gpu_trace_stats.sqlite tui --summary-limit 500 --calls-limit 500
gpu-trace-viewer --db ../gpu_trace_stats.sqlite serve --host 127.0.0.1 --port 8766
gpu-trace-viewer --db ../gpu_trace_stats.sqlite runs
gpu-trace-viewer --db ../gpu_trace_stats.sqlite top --by first --limit 10
gpu-trace-viewer --db ../gpu_trace_stats.sqlite calls --call-order 10
```

If `--db` is omitted, the binary looks for `gpu_trace_stats.sqlite` in the current directory and then `../gpu_trace_stats.sqlite`.
If no subcommand is provided, the binary opens the TUI.

## TUI Keys

- `q`: quit
- `Tab`: switch between Summary and Calls
- `j/k` or arrow keys: move selection
- Moving the Summary selection automatically loads that kernel's calls
- `Enter`: switch from a selected summary op to the Calls panel
- `/`: edit kernel-name filter
- `g`: query exact call order
- `s`: cycle summary sort
- `r`: switch run
- `c`: clear filter, selected op, and call-order query

Selecting a call row automatically shows the `call_order +/- 5` context panel.
The Calls panel title shows the selected kernel or exact `call_order` query.
The bottom stats row shows device-time count, total, min, mean, max, p50, p75, p95, p99, and p99.9 for the current Calls query.

## Web Server

The Rust web server reuses the same SQLite query layer as the TUI and exposes:

- `/api/runs`
- `/api/summary`
- `/api/calls`
- `/api/call-context`
- `/download/summary.csv`
- `/download/calls.csv`
- `/api/delete-run` via `POST {"run_id": N}`

The Python trace importer is still the source of truth for creating/updating the SQLite database.
