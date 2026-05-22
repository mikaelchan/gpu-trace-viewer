# GPU Trace Viewer

Rust terminal and web viewer for `gpu_trace_stats.sqlite` databases produced by the Python importer.

## Install

```bash
cargo install --path . --force
```

This installs `gpu-trace-viewer` into `~/.cargo/bin`.

## Commands

```bash
gpu-trace-viewer --db ../gpu_trace_stats.sqlite tui
gpu-trace-viewer --db ../gpu_trace_stats.sqlite serve --host 127.0.0.1 --port 8766
gpu-trace-viewer --db ../gpu_trace_stats.sqlite runs
gpu-trace-viewer --db ../gpu_trace_stats.sqlite top --by first --limit 10
gpu-trace-viewer --db ../gpu_trace_stats.sqlite calls --call-order 10
```

If `--db` is omitted, the binary looks for `gpu_trace_stats.sqlite` in the current directory and then `../gpu_trace_stats.sqlite`.

## TUI Keys

- `q`: quit
- `Tab`: switch between Summary and Calls
- `j/k` or arrow keys: move selection
- `Enter`: select a summary op and load its calls
- `/`: edit kernel-name filter
- `g`: query exact call order
- `s`: cycle summary sort
- `r`: switch run
- `c`: clear filter, selected op, and call-order query

Selecting a call row automatically shows the `call_order +/- 5` context panel.

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
