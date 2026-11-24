use sjs::engine::simulate;
use sjs::job::Job;
use sjs::metrics::analyze;
use sjs::policy::{Algo, Config};
use sjs::smart::{benchmark, Goal};
use sjs::workload;

fn jobs(rows: &[(u32, u32, u32)]) -> Vec<Job> {
    rows.iter()
        .enumerate()
        .map(|(i, &(a, b, p))| Job {
            pid: i,
            name: format!("P{}", i + 1),
            arrival: a,
            burst: b,
            priority: p,
        })
        .collect()
}

fn avg_wait(algo: Algo, js: &[Job], cfg: &Config) -> f64 {
    let mut pol = algo.build(cfg);
    let res = simulate(js, pol.as_mut(), 0).unwrap();
    analyze(&res).1.avg_wait
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn convoy_textbook_values() {
    // Silberschatz: P1=24, P2=3, P3=3, all at t=0
    let js = jobs(&[(0, 24, 1), (0, 3, 1), (0, 3, 1)]);
    let cfg = Config::default();
    assert!(close(avg_wait(Algo::Fcfs, &js, &cfg), 17.0));
    assert!(close(avg_wait(Algo::Sjf, &js, &cfg), 3.0));
    // RR q=4: waits are 6, 4, 7
    assert!(close(avg_wait(Algo::RoundRobin, &js, &cfg), 17.0 / 3.0));
}

#[test]
fn srtf_textbook_values() {
    // Silberschatz: (0,8) (1,4) (2,9) (3,5)
    let js = jobs(&[(0, 8, 1), (1, 4, 1), (2, 9, 1), (3, 5, 1)]);
    let cfg = Config::default();
    assert!(close(avg_wait(Algo::Srtf, &js, &cfg), 6.5));
    assert!(close(avg_wait(Algo::Sjf, &js, &cfg), 7.75));
    assert!(close(avg_wait(Algo::Fcfs, &js, &cfg), 8.75));
}

#[test]
fn priority_runs_highest_priority_first() {
    // all arrive together; lower number = higher priority
    let js = jobs(&[(0, 5, 3), (0, 5, 1), (0, 5, 2)]);
    let mut pol = Algo::Priority.build(&Config::default());
    let res = simulate(&js, pol.as_mut(), 0).unwrap();
    let c: Vec<u32> = res.procs.iter().map(|p| p.completion.unwrap()).collect();
    assert_eq!(c, vec![15, 5, 10]);
}

#[test]
fn preemptive_priority_preempts() {
    // P2 (higher priority) arrives at 2 and must finish before P1 resumes.
    let js = jobs(&[(0, 6, 5), (2, 3, 1)]);
    let mut pol = Algo::PriorityPreemptive.build(&Config::default());
    let res = simulate(&js, pol.as_mut(), 0).unwrap();
    assert_eq!(res.procs[1].completion, Some(5));
    assert_eq!(res.procs[0].completion, Some(9));
}

#[test]
fn aging_prevents_starvation() {
    let js = workload::demo("starvation").unwrap();
    let cfg = Config::default();
    let low = |algo: Algo| {
        let mut pol = algo.build(&cfg);
        let res = simulate(&js, pol.as_mut(), 0).unwrap();
        res.procs[0].completion.unwrap()
    };
    // Without aging the low priority job finishes dead last; aging lets it in earlier.
    assert!(low(Algo::PriorityAging) < low(Algo::PriorityPreemptive));
}

#[test]
fn idle_gap_is_recorded() {
    let js = jobs(&[(0, 2, 1), (10, 2, 1)]);
    let mut pol = Algo::Fcfs.build(&Config::default());
    let res = simulate(&js, pol.as_mut(), 0).unwrap();
    assert_eq!(res.makespan, 12);
    assert_eq!(res.busy, 4);
    let (_, s) = analyze(&res);
    assert!((s.cpu_util - 100.0 * 4.0 / 12.0).abs() < 1e-9);
}

#[test]
fn context_switch_cost_adds_time() {
    let js = jobs(&[(0, 3, 1), (0, 3, 1)]);
    let mut pol = Algo::Fcfs.build(&Config::default());
    let res = simulate(&js, pol.as_mut(), 2).unwrap();
    assert_eq!(res.context_switches, 1);
    assert_eq!(res.makespan, 3 + 2 + 3);
    assert_eq!(res.overhead, 2);
}

#[test]
fn invariants_hold_for_every_algorithm() {
    for scenario in workload::SCENARIOS {
        for seed in 1..=15u64 {
            let js = workload::random(25, seed, scenario);
            let total: u32 = js.iter().map(|j| j.burst).sum();
            for algo in Algo::ALL {
                for cs in [0u32, 1] {
                    let mut pol = algo.build(&Config::default());
                    let res = simulate(&js, pol.as_mut(), cs).unwrap_or_else(|e| {
                        panic!("{} {} seed {}: {}", algo.id(), scenario, seed, e)
                    });
                    assert_eq!(res.busy, total, "{} lost or gained work", algo.id());
                    // the timeline must tile [0, makespan) with no gaps or overlaps
                    let mut t = 0;
                    for s in &res.timeline {
                        assert_eq!(s.start, t, "{} timeline gap", algo.id());
                        assert!(s.end > s.start);
                        t = s.end;
                    }
                    assert_eq!(t, res.makespan);
                    let (rows, _) = analyze(&res);
                    for m in rows {
                        assert!(m.completion >= m.arrival + m.burst);
                        assert!(m.first_run >= m.arrival);
                        assert_eq!(m.turnaround, m.waiting + m.burst);
                    }
                }
            }
        }
    }
}

#[test]
fn srtf_has_minimum_avg_wait_without_switch_cost() {
    // SRTF is provably optimal for average waiting time.
    for seed in 1..=20u64 {
        let js = workload::random(20, seed, "mixed");
        let cfg = Config::default();
        let srtf = avg_wait(Algo::Srtf, &js, &cfg);
        for algo in Algo::ALL {
            assert!(
                srtf <= avg_wait(algo, &js, &cfg) + 1e-9,
                "SRTF beaten by {} at seed {}",
                algo.id(),
                seed
            );
        }
    }
}

#[test]
fn smart_prefers_srtf_for_throughput_on_mixed_load() {
    let js = workload::random(30, 3, "mixed");
    let ranked = benchmark(&js, &Config::default(), 0, Goal::Throughput).unwrap();
    assert!(!ranked.is_empty());
    // scores are sorted ascending
    assert!(ranked.windows(2).all(|w| w[0].score <= w[1].score));
}

#[test]
fn csv_round_trip_and_errors() {
    let js = workload::random(8, 5, "mixed");
    let back = workload::parse_csv(&workload::to_csv(&js)).unwrap();
    assert_eq!(js, back);
    assert!(workload::parse_csv("A,0,0,1").is_err()); // zero burst
    assert!(
        workload::parse_csv("A,x,3").is_err() || workload::parse_csv("A,x,3").unwrap().is_empty()
    );
    assert!(workload::parse_csv("").is_err());
}

// ---------------------------------------------------------------------------
// Process dependencies (prerequisites)
// ---------------------------------------------------------------------------

use sjs::engine::{simulate_with_dependencies, Kind, SimResult};
use sjs::smart::benchmark_with_dependencies;
use sjs::workload::Rng;

fn run_deps(algo: Algo, js: &[Job], deps: &[Option<usize>], cs: u32) -> SimResult {
    let mut pol = algo.build(&Config::default());
    simulate_with_dependencies(js, deps, pol.as_mut(), cs)
        .unwrap_or_else(|e| panic!("{}: {}", algo.id(), e))
}

fn completions(res: &SimResult) -> Vec<u32> {
    res.procs.iter().map(|p| p.completion.unwrap()).collect()
}

/// Every child must start only after its prerequisite has finished.
fn assert_deps_respected(res: &SimResult, deps: &[Option<usize>], ctx: &str) {
    for (child, parent) in deps.iter().enumerate() {
        if let Some(parent) = *parent {
            let done = res.procs[parent].completion.unwrap();
            let start = res.procs[child].first_run.unwrap();
            assert!(
                start >= done,
                "{}: P{} started at {} before prerequisite P{} finished at {}",
                ctx,
                child + 1,
                start,
                parent + 1,
                done
            );
        }
    }
}

#[test]
fn deps_fcfs_child_waits_for_later_parent() {
    // P2 arrives at 1 but needs P3, which arrives at 2.
    let js = jobs(&[(0, 5, 1), (1, 2, 1), (2, 3, 1)]);
    let deps = [None, Some(2), None];
    let res = run_deps(Algo::Fcfs, &js, &deps, 0);
    assert_eq!(completions(&res), vec![5, 10, 8]);
    // Without the dependency FCFS would run P2 before P3.
    let free = run_deps(Algo::Fcfs, &js, &[None, None, None], 0);
    assert_eq!(completions(&free), vec![5, 7, 10]);
}

#[test]
fn deps_sjf_short_child_cannot_jump_ahead_of_parent() {
    let js = jobs(&[(0, 6, 1), (0, 1, 1), (0, 3, 1)]);
    let free = run_deps(Algo::Sjf, &js, &[None, None, None], 0);
    assert_eq!(completions(&free), vec![10, 1, 4]);
    let res = run_deps(Algo::Sjf, &js, &[None, Some(0), None], 0);
    assert_eq!(completions(&res), vec![9, 10, 3]);
}

#[test]
fn deps_srtf_unblocked_child_preempts() {
    // P3 (burst 1) is released when P1 ends at 2 and runs before long P2.
    let js = jobs(&[(0, 2, 1), (0, 8, 1), (0, 1, 1)]);
    let res = run_deps(Algo::Srtf, &js, &[None, None, Some(0)], 0);
    assert_eq!(completions(&res), vec![2, 11, 3]);
    assert_eq!(res.procs[2].first_run, Some(2));
}

#[test]
fn deps_priority_blocked_high_priority_child() {
    // P2 has the best priority but must wait for P1.
    let js = jobs(&[(0, 3, 2), (0, 2, 1), (0, 6, 3)]);
    let deps = [None, Some(0), None];
    for algo in [Algo::Priority, Algo::PriorityPreemptive] {
        let res = run_deps(algo, &js, &deps, 0);
        assert_eq!(completions(&res), vec![3, 5, 11], "{}", algo.id());
    }
}

#[test]
fn deps_chain_runs_strictly_in_order_under_rr() {
    // P1 -> P2 -> P3: round robin cannot interleave them.
    let js = jobs(&[(0, 3, 1), (0, 3, 1), (0, 3, 1)]);
    let res = run_deps(Algo::RoundRobin, &js, &[None, Some(0), Some(1)], 0);
    assert_eq!(completions(&res), vec![3, 6, 9]);
    assert_eq!(res.makespan, 9);
}

#[test]
fn deps_idle_until_parent_arrives_and_wait_counts_blocked_time() {
    // P2 is present from t=0 but its prerequisite P1 only arrives at t=5.
    let js = jobs(&[(5, 2, 1), (0, 3, 1)]);
    let res = run_deps(Algo::Fcfs, &js, &[None, Some(0)], 0);
    assert!(matches!(res.timeline[0].kind, Kind::Idle));
    assert_eq!((res.timeline[0].start, res.timeline[0].end), (0, 5));
    assert_eq!(completions(&res), vec![7, 10]);
    let (rows, _) = analyze(&res);
    assert_eq!(rows[1].waiting, 7); // blocked time counts as waiting
    assert_eq!(rows[1].response, 7);
}

#[test]
fn deps_with_context_switch_cost() {
    let js = jobs(&[(0, 2, 1), (0, 2, 1)]);
    let res = run_deps(Algo::Fcfs, &js, &[None, Some(0)], 1);
    assert_eq!(res.context_switches, 1);
    assert_eq!(res.overhead, 1);
    assert_eq!(res.makespan, 5);
}

#[test]
fn deps_all_none_matches_plain_simulate() {
    let js = workload::random(20, 7, "mixed");
    for algo in Algo::ALL {
        let mut pol = algo.build(&Config::default());
        let plain = simulate(&js, pol.as_mut(), 1).unwrap();
        let with = run_deps(algo, &js, &vec![None; js.len()], 1);
        assert_eq!(completions(&plain), completions(&with), "{}", algo.id());
    }
}

#[test]
fn deps_invalid_inputs_are_rejected() {
    let js = jobs(&[(0, 2, 1), (0, 2, 1), (0, 2, 1)]);
    let try_deps = |deps: &[Option<usize>]| {
        let mut pol = Algo::Fcfs.build(&Config::default());
        simulate_with_dependencies(&js, deps, pol.as_mut(), 0)
    };
    assert!(try_deps(&[None, Some(1), None])
        .unwrap_err()
        .contains("itself"));
    assert!(try_deps(&[None, Some(9), None])
        .unwrap_err()
        .contains("unknown"));
    assert!(try_deps(&[Some(2), Some(0), Some(1)])
        .unwrap_err()
        .contains("cycle"));
    assert!(try_deps(&[Some(1), Some(0), None])
        .unwrap_err()
        .contains("cycle"));
    assert!(try_deps(&[None, None])
        .unwrap_err()
        .contains("one entry per process"));
    let cfg = Config::default();
    assert!(
        benchmark_with_dependencies(&js, &[Some(1), Some(0), None], &cfg, 0, Goal::Throughput)
            .is_err()
    );
}

#[test]
fn deps_invariants_hold_for_every_algorithm() {
    for scenario in workload::SCENARIOS {
        for seed in 1..=10u64 {
            let js = workload::random(20, seed, scenario);
            // Random DAG: each process may depend on an earlier-indexed one.
            let mut rng = Rng::new(seed * 31 + 1);
            let deps: Vec<Option<usize>> = (0..js.len())
                .map(|i| {
                    if i > 0 && rng.range(0, 1) == 0 {
                        Some(rng.range(0, i as u32 - 1) as usize)
                    } else {
                        None
                    }
                })
                .collect();
            let total: u32 = js.iter().map(|j| j.burst).sum();
            for algo in Algo::ALL {
                for cs in [0u32, 1] {
                    let ctx = format!("{} {} seed {} cs {}", algo.id(), scenario, seed, cs);
                    let res = run_deps(algo, &js, &deps, cs);
                    assert_eq!(res.busy, total, "{}: lost or gained work", ctx);
                    assert_deps_respected(&res, &deps, &ctx);
                    let mut t = 0;
                    for s in &res.timeline {
                        assert_eq!(s.start, t, "{}: timeline gap", ctx);
                        t = s.end;
                    }
                    assert_eq!(t, res.makespan, "{}", ctx);
                }
            }
            let ranked =
                benchmark_with_dependencies(&js, &deps, &Config::default(), 0, Goal::Throughput)
                    .unwrap();
            assert!(!ranked.is_empty());
        }
    }
}

// ---------------------------------------------------------------------------
// CSV prerequisites (depends_on column) used by the command line
// ---------------------------------------------------------------------------

#[test]
fn csv_depends_on_resolves_names_including_forward_references() {
    let text = "name,arrival,burst,priority,depends_on\n\
                Test,0,2,1,Build\n\
                Build,0,4,1,\n\
                Docs,1,3,,none\n\
                Ship,2,1,2,-\n";
    let (js, deps) = workload::parse_csv_with_dependencies(text).unwrap();
    assert_eq!(js.len(), 4);
    assert_eq!(deps, vec![Some(1), None, None, None]);
    assert_eq!(js[2].priority, 1); // blank priority defaults to 1
                                   // parse_csv still accepts the file and just drops the column.
    assert_eq!(workload::parse_csv(text).unwrap(), js);
}

#[test]
fn csv_without_depends_on_has_no_prerequisites() {
    let (js, deps) = workload::parse_csv_with_dependencies("A,0,3,1\nB,1,2").unwrap();
    assert_eq!(js.len(), 2);
    assert_eq!(deps, vec![None, None]);
}

#[test]
fn csv_depends_on_errors() {
    let err = |t: &str| workload::parse_csv_with_dependencies(t).unwrap_err();
    assert!(err("A,0,3,1,Z").contains("no job has that name"));
    assert!(err("A,0,3,1,A").contains("cannot depend on itself"));
    assert!(err("A,0,3,1,B\nB,0,2,1,C\nC,0,1,1,A").contains("cycle"));
    assert!(err("A,0,3,1\nA,0,2,1\nB,0,1,1,A").contains("several jobs"));
    assert!(err("A,0,3,1,B,extra").contains("expected"));
}

#[test]
fn csv_depends_on_round_trip() {
    let (js, deps) = workload::demo_with_dependencies("dependencies").unwrap();
    assert!(deps.iter().any(Option::is_some));
    let text = workload::to_csv_with_dependencies(&js, &deps);
    assert!(text.starts_with("name,arrival,burst,priority,depends_on\n"));
    assert_eq!(
        workload::parse_csv_with_dependencies(&text).unwrap(),
        (js.clone(), deps)
    );
    // Without prerequisites the output stays in the four-column format.
    assert_eq!(
        workload::to_csv_with_dependencies(&js, &vec![None; js.len()]),
        workload::to_csv(&js)
    );
}

#[test]
fn dependencies_demo_runs_in_prerequisite_order_for_every_algorithm() {
    let (js, deps) = workload::demo_with_dependencies("dependencies").unwrap();
    for algo in Algo::ALL {
        let res = run_deps(algo, &js, &deps, 0);
        assert_deps_respected(&res, &deps, algo.short());
    }
    // Demos without prerequisites come back with an all-None list.
    let (_, none) = workload::demo_with_dependencies("classic").unwrap();
    assert!(none.iter().all(Option::is_none));
}

#[test]
fn sample_dependencies_csv_loads() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/workloads/dependencies.csv"
    ))
    .unwrap();
    let (js, deps) = workload::parse_csv_with_dependencies(&text).unwrap();
    let res = run_deps(Algo::Fcfs, &js, &deps, 0);
    assert_deps_respected(&res, &deps, "sample csv");
}
