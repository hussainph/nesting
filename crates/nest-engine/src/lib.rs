//! Nesting engine on top of jagua-rs.
//!
//! `nest` turns a job into jagua items and containers, then runs one search per CPU core until
//! the time limit, cancel, or a long stretch without improvement. Each search builds a solution
//! greedily and then improves it by ruin and recreate: remove a few parts, put them back
//! (biggest first) and keep the result if it places more area, uses cheaper stock, or packs
//! tighter toward the left edge. The best solution across searches is reported as it improves.

mod pip;
mod place;
mod prep;
mod sep;
mod solve;
pub mod validate;

use anyhow::Result;
use jagua_rs::io::export::int_to_ext_transformation;
use kurbo::Affine;
use nest_core::{NestJob, NestResult, Placement, SheetLayout};
use prep::Prepared;
use rand::prelude::*;
use solve::{Score, Sol};
use std::sync::atomic::{AtomicBool, Ordering as AtOrd};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

/// Shared with the caller. Set `cancel` to stop early; `nest` still returns the best result.
#[derive(Debug, Default)]
pub struct Control {
    pub cancel: AtomicBool,
}

struct Best {
    score: Option<Score>,
    sol: Option<Sol>,
    improved_at: Instant,
}

/// Nests `job`. Calls `on_progress` on the calling thread each time the best result improves.
///
/// With `settings.seed` set, every search starts from a fixed seed. Runs are repeatable as long as
/// they do the same amount of work; with a time limit, a faster or busier machine can still end
/// on a different result.
pub fn nest(
    job: &NestJob,
    ctl: &Control,
    mut on_progress: impl FnMut(&NestResult) + Send,
) -> Result<NestResult> {
    let start = Instant::now();
    let prep = prep::prepare(job)?;
    let total_area: f64 = prep.items.iter().map(|i| i.area * i.demand as f64).sum();

    if prep.items.is_empty() || prep.bins.is_empty() {
        let mut res = to_result(job, &prep, &Sol::empty(&prep));
        res.elapsed_secs = start.elapsed().as_secs_f64();
        on_progress(&res);
        return Ok(res);
    }

    let limit = Duration::from_secs_f64(job.settings.time_limit_secs.max(0.1));
    let deadline = start + limit;
    // Stop early after this long without any improvement.
    let stall = Duration::from_secs_f64((limit.as_secs_f64() * 0.4).max(5.0));
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8);
    let base_seed = job.settings.seed.unwrap_or_else(rand::random);

    let best = Mutex::new(Best {
        score: None,
        sol: None,
        improved_at: Instant::now(),
    });
    let (tx, rx) = mpsc::channel::<NestResult>();

    std::thread::scope(|scope| {
        for w in 0..workers {
            let tx = tx.clone();
            let (prep, best) = (&prep, &best);
            scope.spawn(move || {
                worker(
                    job,
                    prep,
                    w,
                    base_seed.wrapping_add(w as u64 * 0x9E37_79B9),
                    deadline,
                    stall,
                    ctl,
                    best,
                    total_area,
                    &tx,
                    start,
                );
            });
        }
        drop(tx);
        for res in rx {
            on_progress(&res);
        }
    });

    let best = best.into_inner().unwrap();
    let mut sol = best.sol.unwrap_or_else(|| Sol::empty(&prep));
    sol.snap_rotations();
    let mut res = to_result(job, &prep, &sol);
    res.elapsed_secs = start.elapsed().as_secs_f64();
    Ok(res)
}

#[allow(clippy::too_many_arguments)]
fn worker(
    job: &NestJob,
    prep: &Prepared,
    w: usize,
    seed: u64,
    deadline: Instant,
    stall: Duration,
    ctl: &Control,
    best: &Mutex<Best>,
    total_area: f64,
    tx: &mpsc::Sender<NestResult>,
    start: Instant,
) {
    let mut rng = SmallRng::seed_from_u64(seed);

    // Worker 0 keeps the default bin order; others shuffle bins within the same kind.
    let mut order = prep.bin_order.clone();
    if w > 0 {
        for i in 1..order.len() {
            if rng.random_bool(0.3) {
                let (a, b) = (order[i - 1], order[i]);
                let same = job.stock[prep.bins[a].stock_idx].kind
                    == job.stock[prep.bins[b].stock_idx].kind;
                if same {
                    order.swap(i - 1, i);
                }
            }
        }
    }

    let stop = |best: &Mutex<Best>| {
        let now = Instant::now();
        now >= deadline
            || ctl.cancel.load(AtOrd::Relaxed)
            || now.duration_since(best.lock().unwrap().improved_at) > stall
    };

    let publish = |sol: &Sol, score: Score| {
        let mut b = best.lock().unwrap();
        let better = b
            .score
            .is_none_or(|s| score.cmp(&s, total_area) == std::cmp::Ordering::Less);
        if better {
            b.score = Some(score);
            b.sol = Some(sol.clone());
            b.improved_at = Instant::now();
            let mut res = to_result(job, prep, sol);
            res.elapsed_secs = start.elapsed().as_secs_f64();
            let _ = tx.send(res);
        }
    };

    let mut cur = Sol::empty(prep);
    cur.recreate(prep, &order, place::Budget::CONSTRUCT, &mut rng);
    let mut cur_score = cur.score(prep);
    publish(&cur, cur_score);

    let total = deadline
        .saturating_duration_since(Instant::now())
        .as_secs_f64()
        .max(1e-3);
    // Ruin and recreate settles which parts go on which sheet; separation then packs them tight.
    let polish_at = Instant::now() + Duration::from_secs_f64(total * 0.3);
    let cancel = || ctl.cancel.load(AtOrd::Relaxed);
    let mut iter = 0u64;
    let mut polishing = false;
    // Compression step as a fraction of the used length: grows on success, shrinks on failure.
    let mut step = 0.01f32;
    let mut stats = (0u32, 0u32, 0u32, 0u32, 0.0f64, 0.0f64);
    while !stop(best) {
        iter += 1;
        if !polishing && Instant::now() >= polish_at {
            polishing = true;
            let b = best.lock().unwrap();
            if let (Some(bs), Some(bsol)) = (b.score, b.sol.as_ref()) {
                cur = bsol.clone();
                cur_score = bs;
            }
        }
        if polishing && !cur.sheets.is_empty() && rng.random_bool(0.85) {
            // Workers cooperate while polishing: each attempt starts from the shared best.
            {
                let b = best.lock().unwrap();
                if let (Some(bs), Some(bsol)) = (b.score, b.sol.as_ref())
                    && bs.cmp(&cur_score, total_area) == std::cmp::Ordering::Less
                {
                    cur = bsol.clone();
                    cur_score = bs;
                }
            }
            let mut cand = cur.clone();
            let n = cand.sheets.len();
            let missing: Vec<usize> = (0..prep.items.len())
                .filter(|&i| cand.missing[i] > 0)
                .collect();
            let ok = if !missing.is_empty() && rng.random_bool(0.6) {
                // Smaller items are likelier to squeeze in; try them more often.
                let it = *missing.choose(&mut rng).unwrap();
                let s = rng.random_range(0..n);
                let t = Instant::now();
                let r = cand.insert(
                    prep,
                    s,
                    it,
                    attempt_deadline(deadline, total),
                    &cancel,
                    &mut rng,
                );
                stats.0 += 1;
                stats.1 += r as u32;
                stats.4 += t.elapsed().as_secs_f64();
                r
            } else {
                let s = if n > 1 && rng.random_bool(0.6) {
                    n - 1
                } else {
                    rng.random_range(0..n)
                };
                let t = Instant::now();
                let r = cand.compress(
                    prep,
                    s,
                    step * rng.random_range(0.7..1.3),
                    attempt_deadline(deadline, total),
                    &cancel,
                    &mut rng,
                );
                step = if r {
                    (step * 1.2).min(0.03)
                } else {
                    (step * 0.7).max(0.001)
                };
                stats.2 += 1;
                stats.3 += r as u32;
                stats.5 += t.elapsed().as_secs_f64();
                r
            };
            if ok {
                let s = cand.score(prep);
                if s.cmp(&cur_score, total_area) != std::cmp::Ordering::Greater {
                    cur = cand;
                    cur_score = s;
                    publish(&cur, cur_score);
                }
            }
            continue;
        }
        let mut cand = cur.clone();
        if !cand.ruin(prep, &mut rng) {
            break;
        }
        cand.recreate(prep, &order, place::Budget::REPAIR, &mut rng);
        let s = cand.score(prep);
        let progress = 1.0
            - deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                / total;
        // Threshold acceptance on compactness, tightening over time; never accept less placed
        // area or more expensive stock. While polishing, only accept improvements.
        let threshold = if polishing {
            0.0
        } else {
            0.01 * (1.0 - progress).max(0.0)
        };
        let accept = match s.cmp(&cur_score, total_area) {
            std::cmp::Ordering::Less | std::cmp::Ordering::Equal => true,
            std::cmp::Ordering::Greater => {
                (s.placed - cur_score.placed).abs() <= total_area * 1e-7
                    && (s.cost - cur_score.cost).abs() <= 1e-6 * s.cost.max(1.0)
                    && s.compact <= cur_score.compact + threshold
            }
        };
        if accept {
            cur = cand;
            cur_score = s;
            publish(&cur, cur_score);
        }
        // Every so often, restart from the global best so workers don't drift apart for long.
        if !polishing && iter.is_multiple_of(400) {
            let b = best.lock().unwrap();
            if let (Some(bs), Some(bsol)) = (b.score, b.sol.as_ref())
                && bs.cmp(&cur_score, total_area) == std::cmp::Ordering::Less
                && rng.random_bool(0.5)
            {
                cur = bsol.clone();
                cur_score = bs;
            }
        }
    }
    log::debug!(
        "worker {w}: {iter} iters; insert {}/{} ok in {:.1}s; compress {}/{} ok in {:.1}s",
        stats.1,
        stats.0,
        stats.4,
        stats.3,
        stats.2,
        stats.5
    );
}

/// One separation attempt gets a slice of the run, so a hopeless attempt can't eat the budget.
fn attempt_deadline(deadline: Instant, total_secs: f64) -> Instant {
    (Instant::now() + Duration::from_secs_f64((total_secs * 0.04).clamp(0.2, 2.0))).min(deadline)
}

fn decompose(a: Affine) -> (bool, f64, (f64, f64)) {
    let c = a.as_coeffs();
    let det = c[0] * c[3] - c[1] * c[2];
    let rot = c[1].atan2(c[0]).to_degrees();
    (det < 0.0, rot, (c[4], c[5]))
}

fn placement(part_id: u64, a: Affine) -> Placement {
    let (mirrored, rotation_deg, translation) = decompose(a);
    Placement {
        part_id,
        mirrored,
        rotation_deg,
        translation,
    }
}

fn to_result(job: &NestJob, prep: &Prepared, sol: &Sol) -> NestResult {
    let mut sheets = Vec::new();
    for sh in &sol.sheets {
        let stock = &job.stock[prep.bins[sh.bin].stock_idx];
        let mut placements = Vec::new();
        let mut area = 0.0;
        for pi in sh.layout.placed_items().values() {
            let it = &prep.items[pi.item().idx()];
            let ext =
                int_to_ext_transformation(&pi.d_transf(), &pi.item().shape_orig().pre_transform);
            let parent = pip::affine_of(&ext);
            let part = &job.parts[it.part_idx];
            placements.push(placement(part.id, parent));
            area += part.geometry.area();
            for ch in &it.children {
                let cp = &job.parts[ch.part_idx];
                placements.push(placement(cp.id, parent * ch.local));
                area += cp.geometry.area();
            }
        }
        let utilization = if stock.area() > 0.0 {
            area / stock.area()
        } else {
            0.0
        };
        sheets.push(SheetLayout {
            stock_id: stock.id,
            placements,
            utilization,
        });
    }

    let mut missing = vec![0u32; job.parts.len()];
    for (i, &n) in sol.missing.iter().enumerate() {
        let it = &prep.items[i];
        missing[it.part_idx] += n as u32;
        for ch in &it.children {
            missing[ch.part_idx] += n as u32;
        }
    }
    for &(pi, n) in &prep.skipped {
        missing[pi] += n as u32;
    }
    let unplaced = missing
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, n)| (job.parts[i].id, *n))
        .collect();
    NestResult {
        sheets,
        unplaced,
        elapsed_secs: 0.0,
    }
}

#[cfg(test)]
mod tests;
