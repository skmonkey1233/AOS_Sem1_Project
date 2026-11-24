//! The "smart" part: look at the workload, suggest an algorithm from its shape,
//! then verify the suggestion by actually simulating every candidate and
//! ranking them with a weighted score for the chosen goal.

use crate::engine::simulate_with_dependencies;
use crate::job::Job;
use crate::metrics::{analyze, Summary};
use crate::policy::{Algo, Config};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Balanced,
    Throughput,
    Interactive,
    Fair,
}

impl Goal {
    pub fn parse(s: &str) -> Option<Goal> {
        match s.to_ascii_lowercase().as_str() {
            "balanced" => Some(Goal::Balanced),
            "throughput" => Some(Goal::Throughput),
            "interactive" => Some(Goal::Interactive),
            "fair" => Some(Goal::Fair),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Goal::Balanced => "balanced",
            Goal::Throughput => "throughput",
            Goal::Interactive => "interactive",
            Goal::Fair => "fair",
        }
    }

    /// Weights for [avg wait, avg response, avg turnaround, max wait, ctx switches, unfairness, inverse throughput].
    fn weights(self) -> [f64; 7] {
        match self {
            Goal::Balanced => [0.25, 0.20, 0.20, 0.15, 0.10, 0.10, 0.00],
            Goal::Throughput => [0.15, 0.00, 0.15, 0.00, 0.15, 0.00, 0.55],
            Goal::Interactive => [0.15, 0.50, 0.05, 0.15, 0.05, 0.10, 0.00],
            Goal::Fair => [0.10, 0.15, 0.00, 0.35, 0.05, 0.35, 0.00],
        }
    }
}

/// Shape of a workload.
#[derive(Clone, Debug)]
pub struct Profile {
    pub jobs: usize,
    pub mean_burst: f64,
    /// Coefficient of variation of the burst times (stddev / mean).
    pub burst_cv: f64,
    /// Fraction of jobs whose burst is at most half the mean.
    pub short_ratio: f64,
    pub priority_levels: usize,
    pub arrival_span: u32,
}

pub fn profile(jobs: &[Job]) -> Profile {
    let n = jobs.len() as f64;
    let mean = jobs.iter().map(|j| j.burst as f64).sum::<f64>() / n;
    let var = jobs
        .iter()
        .map(|j| (j.burst as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    let mut prios: Vec<u32> = jobs.iter().map(|j| j.priority).collect();
    prios.sort_unstable();
    prios.dedup();
    Profile {
        jobs: jobs.len(),
        mean_burst: mean,
        burst_cv: if mean > 0.0 { var.sqrt() / mean } else { 0.0 },
        short_ratio: jobs.iter().filter(|j| j.burst as f64 <= mean / 2.0).count() as f64 / n,
        priority_levels: prios.len(),
        arrival_span: jobs.iter().map(|j| j.arrival).max().unwrap_or(0)
            - jobs.iter().map(|j| j.arrival).min().unwrap_or(0),
    }
}

/// Rule-based suggestion from the workload shape alone (no simulation).
pub fn heuristic(p: &Profile, goal: Goal) -> (Algo, String) {
    match goal {
        Goal::Interactive => {
            if p.burst_cv > 0.5 {
                (Algo::Mlfq, "bursts vary a lot, so MLFQ keeps short jobs snappy without knowing lengths in advance".into())
            } else {
                (
                    Algo::RoundRobin,
                    "similar burst lengths, so plain time slicing gives fast first response".into(),
                )
            }
        }
        Goal::Throughput => {
            if p.burst_cv > 0.3 {
                (Algo::Srtf, "varied bursts, so running the shortest remaining job minimises average waiting".into())
            } else {
                (Algo::Fcfs, "bursts are uniform, so SJF-style reordering gains nothing and FCFS switches least".into())
            }
        }
        Goal::Fair => {
            if p.priority_levels > 1 {
                (Algo::PriorityAging, "priorities are in use, and aging guarantees low-priority jobs still get the CPU".into())
            } else {
                (
                    Algo::RoundRobin,
                    "no priorities to honour, so equal time slices are the fairest choice".into(),
                )
            }
        }
        Goal::Balanced => {
            if p.priority_levels > 1 {
                (
                    Algo::PriorityAging,
                    "priorities differ and aging keeps the result starvation-free".into(),
                )
            } else if p.burst_cv > 0.7 {
                (
                    Algo::Mlfq,
                    "very uneven bursts, and MLFQ adapts to them without starving long jobs".into(),
                )
            } else {
                (
                    Algo::RoundRobin,
                    "fairly uniform bursts and no priorities, so Round Robin is a safe all-rounder"
                        .into(),
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ranked {
    pub algo: Algo,
    pub summary: Summary,
    /// Weighted score; 1.00 would mean best on every weighted metric. Lower is better.
    pub score: f64,
}

/// Simulate every eligible algorithm and rank them for `goal`.
///
/// Priority-based algorithms are skipped when every job has the same priority,
/// because they would just degenerate into FCFS.
pub fn benchmark(
    jobs: &[Job],
    cfg: &Config,
    cs_cost: u32,
    goal: Goal,
) -> Result<Vec<Ranked>, String> {
    benchmark_with_dependencies(jobs, &vec![None; jobs.len()], cfg, cs_cost, goal)
}

/// Benchmark policies while respecting optional process prerequisites.
pub fn benchmark_with_dependencies(
    jobs: &[Job],
    dependencies: &[Option<usize>],
    cfg: &Config,
    cs_cost: u32,
    goal: Goal,
) -> Result<Vec<Ranked>, String> {
    let prof = profile(jobs);
    let mut runs = Vec::new();
    for algo in Algo::ALL {
        let is_prio = matches!(
            algo,
            Algo::Priority | Algo::PriorityPreemptive | Algo::PriorityAging
        );
        if is_prio && prof.priority_levels < 2 {
            continue;
        }
        let mut pol = algo.build(cfg);
        let res = simulate_with_dependencies(jobs, dependencies, pol.as_mut(), cs_cost)?;
        let (_, s) = analyze(&res);
        runs.push((algo, s));
    }

    // Every value is "lower is better"; inverse throughput makes higher
    // completion rates rank better using the same normalized-ratio formula.
    let vals: Vec<[f64; 7]> = runs
        .iter()
        .map(|(_, s)| {
            [
                s.avg_wait,
                s.avg_resp,
                s.avg_tat,
                s.max_wait as f64,
                s.context_switches as f64,
                1.0 / s.jain,
                1.0 / s.throughput,
            ]
        })
        .collect();
    // offsets stop a best value of 0 from blowing up the ratio
    let eps = [0.5, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0005];
    let mut mins = [f64::MAX; 7];
    for v in &vals {
        for k in 0..7 {
            mins[k] = mins[k].min(v[k]);
        }
    }
    let w = goal.weights();
    let wsum: f64 = w.iter().sum();

    let mut ranked: Vec<Ranked> = runs
        .into_iter()
        .zip(vals)
        .map(|((algo, summary), v)| {
            let score = (0..7)
                .map(|k| w[k] * (v[k] + eps[k]) / (mins[k] + eps[k]))
                .sum::<f64>()
                / wsum;
            Ranked {
                algo,
                summary,
                score,
            }
        })
        .collect();
    ranked.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap());
    Ok(ranked)
}
