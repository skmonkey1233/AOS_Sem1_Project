# Smart Job Scheduling System

An interactive Advanced Operating Systems CPU scheduling simulator. The Rust
engine runs each workload; a small local HTTP server connects that engine to the
browser interface. It demonstrates process arrivals, ready queues, preemption,
time quanta, context switches, and scheduling metrics.

## Run the browser simulator

From the project folder:

```bash
cargo run --release -- serve
```

Open `http://127.0.0.1:8000/` in a browser. The server binds to localhost only.
Choose a different port with `cargo run --release -- serve --port 8080`.

The browser submits workloads to `/api/simulate`, `/api/compare`, and `/api/smart`.
Those endpoints call the same Rust policy, engine, metrics, and smart-selection
modules used by the command-line tool. No third-party crates are required.

## Scheduling algorithms

| ID | Algorithm | Behavior |
| --- | --- | --- |
| `fcfs` | First-Come First-Served | Non-preemptive arrival order |
| `sjf` | Shortest Job First | Non-preemptive shortest CPU burst |
| `srtf` | Shortest Remaining Time First | Preemptive shortest remaining burst |
| `priority` | Priority | Non-preemptive; lower number is higher priority |
| `priority-p` | Preemptive Priority | Higher-priority arrivals may preempt |
| `priority-aging` | Priority with Aging | Waiting processes gain priority over time |
| `rr` | Round Robin | FIFO time slices with a tunable quantum |
| `mlfq` | Multi-Level Feedback Queue | Multiple quanta, demotion, and optional boosts |

The smart selector profiles the workload, suggests a policy for the selected goal,
then benchmarks eligible algorithms using the Rust engine. Throughput goal scoring
includes measured throughput along with turnaround and switch cost.

## Metrics and learning goals

The simulator reports per-process waiting, turnaround, and response time, plus
average and maximum wait, CPU utilization, throughput, context switches, overhead,
and Jain fairness. The Gantt timeline includes running, idle, and context-switch
intervals and can be played by time unit or followed event by event with narration.
Presentation mode focuses the browser on the timeline and results.

The browser and the command line both support optional process prerequisites. A dependent process stays
blocked until its prerequisite completes and its own arrival time has passed.
Prerequisites must form an acyclic graph. When dependencies are used, reported
waiting time includes time blocked on a prerequisite as well as ready-queue waiting.

This project directly supports the Advanced OS lab on comparing FCFS, SJF, Priority,
and Round Robin schedulers. Additional policies, workload scenarios, smart goals,
and invariant-focused tests support broader comparison and evaluation.

## CLI examples

```bash
cargo run --release -- list
cargo run --release -- run rr --demo classic --quantum 3
cargo run --release -- compare --demo classic
cargo run --release -- smart --random 12 --seed 7 --goal interactive
cargo run --release -- run fcfs --file workloads/dependencies.csv
cargo run --release -- compare --demo dependencies
cargo test
```

Workload CSV format: `name,arrival,burst[,priority[,depends_on]]`. Lower priority
numbers mean higher priority (a blank priority means 1); burst must be at least one
time unit. `depends_on` is the name of the job that must finish before this one can
run; leave it blank, or write `none` or `-`, for no prerequisite. A job may name one
that appears later in the file. Unknown names, duplicate names used as a
prerequisite, self-dependencies and cycles are reported as errors.

```csv
name,arrival,burst,priority,depends_on
Build library,0,3,2,
Compile app,0,4,1,Build library
Run tests,0,2,1,Compile app
Write docs,1,3,3,Build library
```

`run`, `compare` and `smart` all respect prerequisites, and `sjs gen --demo dependencies`
prints the example above. `workloads/dependencies.csv` is a larger sample.
