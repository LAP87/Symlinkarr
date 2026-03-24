# symlinkarr-autorust

Autonomous Rust agent doing self-improvement research on the Symlinkarr codebase.

## Setup

1. **Baseline established:** `symlinkarr-baseline-v1` tag is the locked baseline
2. **Current branch:** `autorust/mar24` — this is where all experiments happen
3. **Verify build works:** Run `cargo test --no-run 2>&1 | tail -5` to confirm compilation succeeds

## What you CAN do

- Modify any `.rs` source file in `src/`
- Add new test cases
- Refactor, optimize, simplify
- Add dependencies to `Cargo.toml` (keep it minimal)

## What you CANNOT do

- Modify the test infrastructure itself (`tests/` directory structure)
- Change the benchmark harness
- Install system packages

## The Goal

Improve Symlinkarr on two axes:

1. **Test coverage:** More tests passing (current: 239)
2. **Performance:** Faster benchmark time (current baseline: TBD)

## The Experiment Loop

Each experiment:

1. Make a code change
2. Run `cargo test 2>&1 | tee experiment.log`
3. Count passing tests: `grep -c "test result: ok" experiment.log`
4. Run benchmark: `cargo run --release -- report --format=json 2>&1 | jq .total_ms`
5. Commit if improved, revert if not

## Logging

Log to `experiments.tsv`:

```
commit	tests	benchmark_ms	status	description
```

## Metric

- **Tests:** Higher is better (max ~239)
- **Benchmark:** Lower is better (ms)

Run both, weight equally. Simplicity matters —删除 code that doesn't improve things is a win.

## Starting Baseline

Run the first experiment to establish baseline numbers:
- `cargo test 2>&1 | grep "test result"`
- `./target/release/symlinkarr report --format=json 2>&1 | jq .total_ms`
