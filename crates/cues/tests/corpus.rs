//! Parity with cuelume's own offline renderer over the corpus `scripts/cues-refs` writes: every
//! theme, cue, emphasis, part, walk step and channel.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use cues::{Context, Cue, Emphasis, Part, Request, reference_seed, render};
use serde_json::Value;

/// The residual must sit at least this far below each reference file's energy.
const PASS_DB: f64 = 100.0;

fn corpus_dir() -> PathBuf {
    std::env::var_os("CUES_REFS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/cues-refs"))
}

/// One play in the corpus and its reference file per channel.
struct Play {
    theme: String,
    cue: String,
    emphasis: String,
    part: String,
    step: usize,
    take: u32,
    files: [Option<(String, usize)>; 2],
}

struct Outcome {
    name: String,
    theme: String,
    channel: usize,
    db: f64,
    lengths: (usize, usize),
}

fn plays(manifest: &Value) -> Vec<Play> {
    let mut plays: BTreeMap<String, Play> = BTreeMap::new();
    for file in manifest["files"].as_array().expect("files") {
        let text = |key: &str| file[key].as_str().expect(key).to_owned();
        let number = |key: &str| file[key].as_u64().expect(key);
        let key = format!(
            "{}-{}-{}-{}-s{}-t{}",
            text("theme"),
            text("cue"),
            text("emphasis"),
            text("part"),
            number("step"),
            number("take")
        );
        let play = plays.entry(key).or_insert_with(|| Play {
            theme: text("theme"),
            cue: text("cue"),
            emphasis: text("emphasis"),
            part: text("part"),
            step: number("step") as usize,
            take: number("take") as u32,
            files: [None, None],
        });
        play.files[number("channel") as usize] = Some((text("file"), number("length") as usize));
    }
    plays.into_values().collect()
}

pub fn cue(name: &str) -> Cue {
    match name {
        "tap" => Cue::Tap,
        "type" => Cue::Type,
        "select" => Cue::Select,
        "toggle" => Cue::Toggle,
        "open" => Cue::Open,
        "close" => Cue::Close,
        "navigate" => Cue::Navigate,
        "success" => Cue::Success,
        "warning" => Cue::Warning,
        "error" => Cue::Error,
        "loading" => Cue::Loading,
        "ready" => Cue::Ready,
        "attention" => Cue::Attention,
        "count" => Cue::Count,
        _ => panic!("unknown cue {name}"),
    }
}

pub fn emphasis(name: &str) -> Emphasis {
    match name {
        "subtle" => Emphasis::Subtle,
        "normal" => Emphasis::Normal,
        "strong" => Emphasis::Strong,
        _ => panic!("unknown emphasis {name}"),
    }
}

pub fn part(name: &str) -> Part {
    match name {
        "press" => Part::Press,
        "release" => Part::Release,
        "both" => Part::Both,
        _ => panic!("unknown part {name}"),
    }
}

fn check(play: &Play, dir: &std::path::Path, rate: u32) -> Vec<Outcome> {
    let (theme, cue, emphasis, part) = (
        common::theme(&play.theme),
        cue(&play.cue),
        emphasis(&play.emphasis),
        part(&play.part),
    );
    let stereo = render(
        &Request {
            theme,
            cue,
            emphasis,
            context: Context::default(),
            since_last: None,
            step: play.step,
            part,
            vary: false,
            seed: reference_seed(theme, cue, emphasis, part, play.take),
        },
        rate,
    );
    [&stereo.left, &stereo.right]
        .into_iter()
        .zip(&play.files)
        .enumerate()
        .map(|(channel, (ours, file))| {
            let (name, length) = file.as_ref().expect("both channels in the manifest");
            let bytes = std::fs::read(dir.join(name)).expect("reference file");
            let reference: Vec<f64> = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|&b| f64::from_le_bytes(b))
                .collect();
            assert_eq!(
                reference.len(),
                *length,
                "{name} is not the length the manifest says"
            );
            let ours: Vec<f64> = ours.iter().map(|&v| f64::from(v)).collect();
            Outcome {
                name: name.clone(),
                theme: play.theme.clone(),
                channel,
                db: common::residual_db(&reference, &ours),
                lengths: (reference.len(), ours.len()),
            }
        })
        .collect()
}

#[test]
#[ignore = "needs the reference corpus: run scripts/cues-refs first"]
fn render_should_match_cuelume_over_the_corpus() {
    let dir = corpus_dir();
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json"))
            .expect("no corpus manifest: run scripts/cues-refs first"),
    )
    .expect("manifest.json");
    let rate = manifest["rate"].as_u64().expect("rate") as u32;
    let plays = plays(&manifest);

    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = plays.len().div_ceil(workers);
    let outcomes: Vec<Outcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = plays
            .chunks(chunk)
            .map(|plays| {
                scope.spawn(|| {
                    plays
                        .iter()
                        .flat_map(|play| check(play, &dir, rate))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker"))
            .collect()
    });

    let mut worst: BTreeMap<&str, &Outcome> = BTreeMap::new();
    for outcome in &outcomes {
        let entry = worst.entry(&outcome.theme).or_insert(outcome);
        if outcome.db < entry.db {
            *entry = outcome;
        }
    }
    for (theme, outcome) in &worst {
        println!(
            "{theme}: worst {:.1} dB down ({}, channel {})",
            outcome.db, outcome.name, outcome.channel
        );
    }
    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| o.lengths.0 != o.lengths.1 || o.db < PASS_DB)
        .map(|o| {
            format!(
                "{}: {:.1} dB, length {} vs ours {}",
                o.name, o.db, o.lengths.0, o.lengths.1
            )
        })
        .collect();
    println!("checked {} files", outcomes.len());
    assert!(
        failures.is_empty(),
        "{} of {} files miss:\n{}",
        failures.len(),
        outcomes.len(),
        failures.join("\n")
    );
}
