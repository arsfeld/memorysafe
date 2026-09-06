//! The harness behind the `mmr_lambda` and `diversity_cut_similarity` notes on
//! `BaselineConfig`. Retained in the checkout, and runnable, so the numbers
//! quoted there can be re-derived rather than taken on trust:
//!
//! ```text
//! cargo run --release --example mmr_calibration -p memorysafe-policy
//! ```
//!
//! **What it is and is not.** The corpora are SYNTHETIC — a generated
//! vocabulary with a function-word head and a content tail, not a real corpus,
//! because this project has none yet. Treat the output as bounding the SHAPE of
//! each effect and its sensitivity to function-word density, never as
//! calibration data. The generator is a fixed-seed LCG so every number is
//! reproducible, which is a different and much weaker claim than being right.
//!
//! It calls the crate's real `similarity::{coverage, overlap, token_set}`
//! rather than reimplementing them, so it cannot drift away from the code it
//! is describing.

use memorysafe_policy::similarity::{coverage, overlap, token_set};
use std::collections::HashSet;

/// Deterministic LCG, so the tables reproduce exactly across runs and machines.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
    fn unit(&mut self) -> f32 {
        (self.below(10_000) as f32) / 10_000.0
    }
}

const STOP: &[&str] = &[
    "the", "a", "of", "to", "and", "in", "is", "for", "on", "that", "was", "with", "it", "as",
    "at", "be", "this", "have", "from", "or", "by", "not", "are", "but", "they", "we", "an",
    "which", "you", "one",
];

/// The parameters that actually move the answers: how much of a body is
/// function words (which every item shares, so the union collects them while a
/// pairwise max sees only one item's worth), and how large the content
/// vocabulary is (which sets how often two items genuinely say the same thing).
struct Shape {
    label: &'static str,
    content_vocab: usize,
    body_len: usize,
    stop_frac: f32,
}

const SHAPES: &[Shape] = &[
    Shape {
        label: "prose-like",
        content_vocab: 400,
        body_len: 20,
        stop_frac: 0.45,
    },
    Shape {
        label: "terse facts",
        content_vocab: 4000,
        body_len: 8,
        stop_frac: 0.15,
    },
    Shape {
        label: "narrow topic",
        content_vocab: 60,
        body_len: 20,
        stop_frac: 0.45,
    },
];

const MAX_SELECTED: usize = 10;
const CANDIDATES: usize = 12;
/// The lambda the crate ships. Every comparison below is against the behaviour
/// of the PAIRWISE MAX form at this value -- that is the behaviour a
/// recalibration would be trying to reproduce.
const SHIPPED_LAMBDA: f32 = 0.70;

fn body(rng: &mut Rng, s: &Shape) -> String {
    (0..s.body_len)
        .map(|_| {
            if rng.unit() < s.stop_frac {
                STOP[rng.below(STOP.len())].to_string()
            } else {
                format!("w{}", rng.below(s.content_vocab))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn mean(v: &[f32]) -> f64 {
    v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64
}

fn sd(v: &[f32]) -> f64 {
    let m = mean(v);
    (v.iter().map(|x| (*x as f64 - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
}

fn mmr(lambda: f32, relevance: f32, penalty: f32) -> f32 {
    lambda * relevance - (1.0 - lambda) * penalty
}

fn argmax(lambda: f32, relevances: &[f32], penalties: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, (r, p)) in relevances.iter().zip(penalties).enumerate() {
        let v = mmr(lambda, *r, *p);
        if v > best_v {
            best_v = v;
            best = i;
        }
    }
    best
}

/// One trial: a growing selected set, a fixed pool of candidates, and both
/// penalties for every candidate at every selected-set size.
struct Trial {
    /// `union[n][i]` / `max[n][i]`: candidate `i`'s penalty at `|S| = n + 1`.
    union: Vec<Vec<f32>>,
    max: Vec<Vec<f32>>,
    relevances: Vec<f32>,
}

fn trial(rng: &mut Rng, s: &Shape) -> Trial {
    let selected: Vec<String> = (0..MAX_SELECTED).map(|_| body(rng, s)).collect();
    let pool: Vec<String> = (0..CANDIDATES).map(|_| body(rng, s)).collect();
    let relevances: Vec<f32> = (0..CANDIDATES).map(|_| rng.unit()).collect();

    let mut running: HashSet<String> = HashSet::new();
    let mut running_max = vec![0.0f32; CANDIDATES];
    let (mut union, mut max) = (Vec::new(), Vec::new());
    for sel in &selected {
        running.extend(token_set(sel));
        let mut u = Vec::with_capacity(CANDIDATES);
        for (i, b) in pool.iter().enumerate() {
            running_max[i] = running_max[i].max(overlap(b, sel));
            u.push(coverage(b, &running));
        }
        union.push(u);
        max.push(running_max.clone());
    }
    Trial {
        union,
        max,
        relevances,
    }
}

fn main() {
    let trials: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(20_000);
    println!("synthetic corpora, {trials} trials per shape, fixed seed; NOT calibration data");

    for s in SHAPES {
        // ---- Table 1: level, per-candidate variation, and the tagging rate.
        let mut rng = Rng(0x5EED_1234);
        let mut acc_m = [0.0f64; MAX_SELECTED];
        let mut acc_u = [0.0f64; MAX_SELECTED];
        let mut acc_sd_delta = [0.0f64; MAX_SELECTED];
        let mut acc_zero_delta = [0.0f64; MAX_SELECTED];
        let mut cut_m = [0usize; MAX_SELECTED];
        let mut cut_u = [0usize; MAX_SELECTED];
        for _ in 0..trials {
            let t = trial(&mut rng, s);
            for n in 0..MAX_SELECTED {
                let (u, m) = (&t.union[n], &t.max[n]);
                acc_m[n] += mean(m);
                acc_u[n] += mean(u);
                let delta: Vec<f32> = u.iter().zip(m).map(|(a, b)| a - b).collect();
                acc_sd_delta[n] += sd(&delta);
                acc_zero_delta[n] +=
                    delta.iter().filter(|d| **d == 0.0).count() as f64 / CANDIDATES as f64;
                // Only candidate 0 is sampled for the tagging rate, so each
                // trial contributes one independent observation rather than
                // twelve correlated ones.
                if m[0] > 0.50 {
                    cut_m[n] += 1;
                }
                if u[0] > 0.50 {
                    cut_u[n] += 1;
                }
            }
        }
        println!(
            "\n== {} (content vocab {}, {} tokens/body, {:.0}% function words)",
            s.label,
            s.content_vocab,
            s.body_len,
            s.stop_frac * 100.0
        );
        println!("  |S|  mean M  mean U   U-M    sd(U-M)  U==M    DiversityCut  max -> union");
        for n in 0..MAX_SELECTED {
            let (m, u) = (acc_m[n] / trials as f64, acc_u[n] / trials as f64);
            println!(
                "  {:>3}  {:.4}  {:.4}  {:+.4}   {:.4}  {:>5.1}%   {:>6.1}% -> {:>5.1}%",
                n + 1,
                m,
                u,
                u - m,
                acc_sd_delta[n] / trials as f64,
                100.0 * acc_zero_delta[n] / trials as f64,
                100.0 * cut_m[n] as f64 / trials as f64,
                100.0 * cut_u[n] as f64 / trials as f64,
            );
        }

        // ---- Table 2: whole-set spread (sd), the PROXY, against the argmax,
        // which is what MMR actually takes. Sweeping lambda answers the
        // question sd only gestures at: can raising lambda make the union form
        // reproduce the pairwise-max form's picks?
        let lambdas = [0.70f32, 0.75, 0.80, 0.84, 0.90, 0.95];
        let mut rng = Rng(0x5EED_1234);
        let mut sd_m = [0.0f64; MAX_SELECTED];
        let mut sd_u = [0.0f64; MAX_SELECTED];
        let mut differs = [[0usize; 6]; MAX_SELECTED];
        for _ in 0..trials {
            let t = trial(&mut rng, s);
            for n in 0..MAX_SELECTED {
                let (u, m) = (&t.union[n], &t.max[n]);
                sd_m[n] += sd(m);
                sd_u[n] += sd(u);
                let baseline = argmax(SHIPPED_LAMBDA, &t.relevances, m);
                for (slot, l) in differs[n].iter_mut().zip(&lambdas) {
                    if argmax(*l, &t.relevances, u) != baseline {
                        *slot += 1;
                    }
                }
            }
        }
        print!("  |S|   sd(M)   sd(U)  ratio |  argmax differs from max@0.70, at lambda =");
        println!();
        print!("                                 |");
        for l in &lambdas {
            print!("  {l:.2} ");
        }
        println!();
        for n in 0..MAX_SELECTED {
            let (m, u) = (sd_m[n] / trials as f64, sd_u[n] / trials as f64);
            print!("  {:>3}  {:.4}  {:.4}  {:.2}x |", n + 1, m, u, u / m);
            for d in &differs[n] {
                print!(" {:>5.1}%", 100.0 * *d as f64 / trials as f64);
            }
            println!();
        }
    }
}
