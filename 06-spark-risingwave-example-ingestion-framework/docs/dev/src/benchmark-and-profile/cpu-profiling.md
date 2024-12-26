# CPU Profiling Guide

## Profiling on host
Share an easy-to-use profiler and flamegraph tool: https://github.com/koute/not-perf.git

Record samples:

```shell
nperf record -p `pidof compute-node` -o perf.data
```

Generate flamegraph:

```shell
nperf flamegraph --merge-threads perf.data > perf.svg
```

## Profiling remote compute nodes
You can profile remote compute nodes from a local machine by simply type the following command.
```shell
./risedev ctl profile cpu --sleep [seconds]
```
All compute nodes will be profile for a given `seconds` time and generated flame graph will be transferred to your local machine `.risingwave/profiling/`.

Note: To profile compute nodes remotely, please make sure all remote nodes have a public IP address accessible from your local machine (where you are running `risedev`).

## CPU Profiling on OSX

Get the pid of the node you want to profile.

```shell
pgrep compute-node
```

Use `cargo flamegraph`:
```shell
cargo install flamegraph
sudo flamegraph -o flamegraph.svg --pid [pid]
```

When you're satisfied, you can `Ctrl+C` to stop the profiler.

```shell
open flamegraph.svg
```
