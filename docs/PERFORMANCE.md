# Arch Linux performance benchmarks

The baseline uses the production sources from `b448adf` with the benchmark-only commit `3709e35`. Run the same tests on the performance branch to compare. Both recorded runs used Arch Linux x86-64, Linux 7.3.5, Hyprland 0.57.2, an Intel Core i5-1155G7 with eight logical CPUs, OpenSSH 10.6p2 and Arch's Rust 1.100.0 toolchain.

Use the same Cargo target directory throughout and run one command at a time. Measure both binaries after their respective builds. Use the dev binary in the Wayland session so each launch can use a fresh configuration directory.

```sh
export CARGO_TARGET_DIR=/tmp/vela-target-perf
cargo build --release --locked
stat -c%s "$CARGO_TARGET_DIR/release/vela"
cargo build --release --features dev --locked
stat -c%s "$CARGO_TARGET_DIR/release/vela"

config_dir="$(mktemp -d)"
VELA_CONFIG_DIR="$config_dir" VELA_TRACE_STARTUP=1 "$CARGO_TARGET_DIR/release/vela"
```

Record the `Vela startup` line from each of three launches and close the window after the trace. In three additional launches, wait six seconds after the first frame and record `VmRSS` from `/proc/<pid>/status` and `Pss` from `/proc/<pid>/smaps_rollup` before closing the window. Divide the reported KiB by 1024 to obtain MiB.

```sh
for trial in 1 2 3; do
    cargo test --release --locked -p vela-core --test sshd benchmark_transfer_and_directory_listing -- --ignored --nocapture
done
for trial in 1 2 3; do
    cargo test --release --locked -p vela-core --test sshd benchmark_perf_suite -- --ignored --nocapture
done
cargo test --release --locked -p vela-app tree::tests::flattens_ten_thousand_entries_under_five_ms -- --nocapture
cargo test --release --locked -p vela-app pane::perf_tests::benchmark_perf_suite -- --ignored --nocapture
```

The SSH fixtures use loopback and a private agent, and they verify transfer integrity. The connect latency samples measure a trusted-host connection, with a disconnect between samples. Use each invocation's median for the five connect samples and three samples per directory-listing benchmark, then take the median across invocations. The app suite times local listing and tree preparation together, includes the row projection in the drag-target timing, and measures terminal prepaint on a synthetic 160×50 grid. The existing tree flatten test reports the median and range across nine iterations. The other benchmarks print individual samples. Calculate percentage change as `100 * (after / before - 1)`. MB/s uses decimal megabytes.

The unrestricted loopback trials varied substantially across runs. The controlled comparison alternates the preserved baseline and optimized test executables for five pairs, reversing their order on every other pair. Run each executable directly with `benchmark_transfer_and_directory_listing --ignored --nocapture` under `taskset` on one logical CPU per physical core. On the measured machine those CPUs are 0–3. Cargo builds continue to use all available CPUs.

For the paired desktop follow-up, copy both dev binaries to the same temporary filesystem and alternate them in the same Wayland session. Run five pairs for startup and three pairs for idle memory with fresh configurations. This checks for timing differences related to test order and to the filesystem that backs the executable.
