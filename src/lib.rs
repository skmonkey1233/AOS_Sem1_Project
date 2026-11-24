//! Smart Job Scheduling System - a discrete-time CPU scheduling simulator.
//!
//! Modules:
//! - `job`      : static job description and runtime process state
//! - `policy`   : the scheduling algorithms (all behind one `Policy` trait)
//! - `engine`   : the tick-by-tick simulation loop (dispatcher + clock)
//! - `metrics`  : waiting / turnaround / response time, utilisation, fairness
//! - `gantt`    : ASCII Gantt chart renderer
//! - `workload` : CSV loader, built-in demos, seeded random generator
//! - `smart`    : workload profiling + benchmark-and-pick auto selector

pub mod engine;
pub mod gantt;
pub mod job;
pub mod metrics;
pub mod policy;
pub mod smart;
pub mod workload;
