# Performance baseline

Recorded on 2026-07-11 with `rustc 1.96.0` on Darwin arm64. Run with:

```sh
cargo test --release perf_probe_keystroke_and_scroll -- --ignored --nocapture
```

The probe used a 408 KB document with 39,001 lines and 12,000 Markdown blocks.

| Operation | Time |
| --- | ---: |
| First build and assemble | 54.6 µs |
| Edit, index, and assemble | 1.24 ms |
| Cursor-line move | 756 µs |
| Wheel step | 20.4 µs |

These numbers are a local comparison point, not a cross-machine benchmark.
