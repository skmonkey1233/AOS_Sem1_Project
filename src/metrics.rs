use crate::engine::SimResult;

#[derive(Clone, Debug)]
pub struct ProcMetrics {
    pub name: String,
    pub arrival: u32,
    pub burst: u32,
    pub priority: u32,
    pub first_run: u32,
    pub completion: u32,
    /// completion - arrival
    pub turnaround: u32,
    /// turnaround - burst
    pub waiting: u32,
    /// first_run - arrival
    pub response: u32,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub avg_wait: f64,
    pub avg_tat: f64,
    pub avg_resp: f64,
    pub max_wait: u32,
    pub makespan: u32,
    /// Percentage of the makespan spent running jobs.
    pub cpu_util: f64,
    /// Jobs completed per time unit.
    pub throughput: f64,
    pub context_switches: u32,
    pub overhead: u32,
    /// Jain's fairness index over burst/turnaround (1.0 = perfectly fair).
    pub jain: f64,
}

pub fn analyze(r: &SimResult) -> (Vec<ProcMetrics>, Summary) {
    let rows: Vec<ProcMetrics> = r
        .procs
        .iter()
        .map(|p| {
            let completion = p.completion.expect("job did not finish");
            let first_run = p.first_run.expect("job never ran");
            let turnaround = completion - p.job.arrival;
            ProcMetrics {
                name: p.job.name.clone(),
                arrival: p.job.arrival,
                burst: p.job.burst,
                priority: p.job.priority,
                first_run,
                completion,
                turnaround,
                waiting: turnaround - p.job.burst,
                response: first_run - p.job.arrival,
            }
        })
        .collect();

    let n = rows.len() as f64;
    let avg = |f: fn(&ProcMetrics) -> u32| rows.iter().map(|m| f(m) as f64).sum::<f64>() / n;

    // Jain's index on x_i = burst_i / turnaround_i  (each x_i in (0,1]).
    let xs: Vec<f64> = rows
        .iter()
        .map(|m| m.burst as f64 / m.turnaround as f64)
        .collect();
    let sum: f64 = xs.iter().sum();
    let sum_sq: f64 = xs.iter().map(|x| x * x).sum();
    let jain = if sum_sq > 0.0 {
        sum * sum / (n * sum_sq)
    } else {
        1.0
    };

    let summary = Summary {
        avg_wait: avg(|m| m.waiting),
        avg_tat: avg(|m| m.turnaround),
        avg_resp: avg(|m| m.response),
        max_wait: rows.iter().map(|m| m.waiting).max().unwrap_or(0),
        makespan: r.makespan,
        cpu_util: r.busy as f64 / r.makespan as f64 * 100.0,
        throughput: n / r.makespan as f64,
        context_switches: r.context_switches,
        overhead: r.overhead,
        jain,
    };
    (rows, summary)
}
