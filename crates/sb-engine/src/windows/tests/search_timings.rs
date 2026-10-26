//! M1.6q search timings (spec/11 "Search qualification"): a reproducible
//! synthetic archive of 100,000 messages imported through the real
//! pipeline, then first-50 timings cold and warm, idle and under
//! background imports, exact counts, coverage, commit-to-searchable
//! freshness and a full rebuild, written as one JSON report. Recorded
//! by CI (`search-timings`), never asserted; the owner's Mac run is
//! checked against spec/06's budgets in qualification.
//!
//!   corpus ─► idle: cold, warm, counts, coverage, ids, deep page
//!         ─► background imports: cold, warm
//!         ─► freshness (idle, background) ─► rebuild (last: it resets)
//!
//! Run with `--release --ignored --nocapture`. `SB_SEARCH_TIMINGS_OUT`
//! names the report file; `SB_SEARCH_TIMINGS_ROWS` shrinks the archive
//! for a quick local run (the report records it).
//! `SB_SEARCH_TIMINGS_ROOT=<dir>` builds the archive there and keeps it:
//! the app opens `<dir>/store` for M1.6q2's keystroke benchmark. With
//! `SB_SEARCH_TIMINGS_SEED_ONLY=1` the run stops once the archive is
//! built and checked.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sb_resources::{
    ImportJobState, Query, RebuildPhase, SearchScope as StoreScope, counts, latest_rebuild,
    read_search_coverage,
};
use sb_test_support::{ScratchDir, block_on};
use serde_json::{Value, json};

use super::store::{sql_error, value};
use crate::{Anchor, Count, Engine, SearchScope, SearchSort, SearchSpec, WindowSpec};

/// The archive spec/11 asks for.
const ROWS: usize = 100_000;
/// The generator's seed and version: the same pair builds the same mail.
const SEED: u64 = 0x5eed_2026_1026;
const GENERATOR_VERSION: u32 = 1;
/// spec/11: a fixed 1,000-query warm sample per workload, cycling the
/// query set.
const WARM_SAMPLE: usize = 1_000;
/// Reopens per query: each times one query on a fresh core.
const COLD_ROUNDS: usize = 10;
/// Calls per query for the counts, coverage and id-index timings: at
/// 100k a phrase's id index and deep page take seconds each, and the
/// warm samples already take most of the run.
const DETAIL_ROUNDS: usize = 5;
const FRESHNESS_SAMPLES: usize = 25;
const FIRST_PAGE: u32 = 50;
const LATER_PAGE: u32 = 200;
/// One message in fifty keeps a failed document: unknowns stay real and
/// stable, as nothing re-queues them.
const FAILED_EVERY: i64 = 50;
/// The background load: a batch of new messages imported every few
/// seconds, a steady sync rather than a bulk import, so the archive
/// barely grows while timings run (the report records how much).
const BACKGROUND_BATCH: usize = 10;
const BACKGROUND_PAUSE: Duration = Duration::from_secs(5);
const PATIENCE: Duration = Duration::from_secs(4 * 60 * 60);
/// Background batches made so far: every batch's mail is new, or the
/// import's deduplication would stop the writes.
static BATCHES: AtomicU64 = AtomicU64::new(0);
const FRESHNESS_DEADLINE: Duration = Duration::from_secs(30);

/// One timed query: canonical v1 JSON (synthetic text only), its outer
/// scope and sort.
struct Probe {
    id: String,
    class: &'static str,
    json: String,
    scope: SearchScope,
    sort: SearchSort,
}

impl Probe {
    fn spec(&self) -> WindowSpec {
        WindowSpec::Search(SearchSpec::new(
            self.json.clone(),
            self.scope.clone(),
            self.sort,
        ))
    }

    fn store_scope(&self) -> StoreScope {
        match self.scope {
            SearchScope::All => StoreScope::All,
            SearchScope::Mailbox(id) => StoreScope::Mailbox(id),
            SearchScope::Account(_) => unreachable!("the query set has no account scope"),
        }
    }
}

#[test]
#[ignore = "timing record for CI, run with --release --ignored --nocapture"]
fn search_timings() {
    let rows = std::env::var("SB_SEARCH_TIMINGS_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(ROWS);
    let scratch = ScratchDir::new("search-timings");
    let base = match std::env::var_os("SB_SEARCH_TIMINGS_ROOT") {
        Some(dir) => PathBuf::from(dir),
        None => scratch.path().to_path_buf(),
    };
    let root = base.join("store");
    assert!(!root.exists(), "{} already holds a store", root.display());
    fs::create_dir_all(&base).unwrap();
    let mut engine = block_on(Engine::open(&root)).unwrap();

    let built = Instant::now();
    let (archive, projects) = write_corpus(&base, rows);
    let job = start(&engine, vec![archive, projects]);
    finish(&engine, &job);
    settle(&engine);
    let imported_ms = ms(built.elapsed());
    let failed = fail_every_fiftieth(&engine);
    let corpus_ms = ms(built.elapsed());
    let projects = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE remote_name = 'Projects'",
    );
    let probes = probes(rows, projects);
    for probe in &probes {
        Query::parse_json(&probe.json).unwrap_or_else(|e| panic!("{}: {e:?}", probe.id));
    }
    check_corpus(&engine, &probes, rows, failed);
    eprintln!("search timings: {rows} messages imported in {imported_ms:.0} ms");

    // Written after every phase: hours of measurement survive a later
    // failure or a CI timeout.
    let mut report = json!({
        "version": 1,
        "dataset": {
            "messages": rows,
            "seed": format!("{SEED:#x}"),
            "generator_version": GENERATOR_VERSION,
            "failed_documents": failed,
            "mailboxes": {"Archive": rows - rows * 3 / 10, "Projects": rows * 3 / 10},
            "imported_ms": imported_ms,
            "corpus_ms": corpus_ms,
        },
        "sampling": {
            "warm": format!("{WARM_SAMPLE} first-{FIRST_PAGE} opens per workload, cycling the query set; failures counted, never dropped"),
            "cold": format!("{COLD_ROUNDS} per query, each the first query on a freshly opened core (new core and read connections)"),
            "percentiles": "nearest rank over successful samples",
        },
        "environment": environment(&engine),
        "queries": probes.iter().map(|p| json!({
            "id": p.id, "class": p.class, "query": p.json,
            "scope": format!("{:?}", p.scope), "sort": format!("{:?}", p.sort),
        })).collect::<Vec<_>>(),
        "coverage_after_corpus": coverage_json(&engine),
        "phase_ms": {},
        "workloads": [],
        "complete": false,
    });
    save(&report);
    if std::env::var_os("SB_SEARCH_TIMINGS_SEED_ONLY").is_some() {
        report["seed_only"] = json!(true);
        report["data_root"] = json!(root.display().to_string());
        report["complete"] = json!(true);
        save(&report);
        block_on(engine.shutdown()).unwrap();
        return;
    }
    let mut lap = Instant::now();
    let mut mark = |report: &mut Value, name: &str| {
        report["phase_ms"][name] = json!(ms(lap.elapsed()));
        lap = Instant::now();
        save(report);
    };
    // Idle: nothing else writes.
    let (idle_cold, reopened) = cold(engine, &root, &probes, Background::Off);
    engine = reopened;
    let idle_warm = warm(&engine, &probes);
    push_workload(
        &mut report,
        json!({
            "name": "idle",
            "active_workload": "none",
            "cold": idle_cold,
            "warm": idle_warm,
            "counts": counts_json(&engine, &probes),
            "coverage_ms": coverage_timings(&engine, &probes),
            "ids_and_deep_page": ids_and_deep(&engine, &probes),
            "coverage": coverage_json(&engine),
        }),
    );
    mark(&mut report, "idle");

    // Background: real imports and their index writes run throughout.
    let (busy_cold, reopened) = cold(engine, &root, &probes, Background::Imports(&base));
    engine = reopened;
    let stop = AtomicBool::new(false);
    let imported = AtomicU64::new(0);
    let busy_warm = std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| background(&engine, &base, &stop, &imported));
        warm(&engine, &probes)
    });
    settle(&engine);
    push_workload(
        &mut report,
        json!({
            "name": "background-import",
            "active_workload": format!(
                "a new {BACKGROUND_BATCH}-message mbox imported every {} s, each into a new mailbox, its job status polled every 10 ms; {} messages imported during the warm sample; each cold sample's reopen starts one",
                BACKGROUND_PAUSE.as_secs(),
                imported.load(Ordering::SeqCst)
            ),
            "cold": busy_cold,
            "warm": busy_warm,
            "coverage": coverage_json(&engine),
        }),
    );
    mark(&mut report, "background-import");

    let idle_freshness = freshness(&engine, &base, "idle");
    let stop = AtomicBool::new(false);
    let imported = AtomicU64::new(0);
    let mut busy_freshness = std::thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        scope.spawn(|| background(&engine, &base, &stop, &imported));
        freshness(&engine, &base, "busy")
    });
    busy_freshness["messages_imported_alongside"] = json!(imported.load(Ordering::SeqCst));
    settle(&engine);
    report["freshness"] = json!({
        "idle": idle_freshness,
        "background-import": busy_freshness,
        "definition": "first search hit (core clock, stamped when the read returns) minus the acquisition's committed first_acquired_at",
    });

    mark(&mut report, "freshness");
    report["rebuild"] = rebuild(&engine, &probes[0]);
    mark(&mut report, "rebuild");
    report["complete"] = json!(true);
    save(&report);
    let text = serde_json::to_string_pretty(&report).unwrap();
    if std::env::var_os("SB_SEARCH_TIMINGS_OUT").is_none() {
        println!("SEARCH_TIMINGS_JSON {text}");
    }
    println!("{text}");
    // Checked only once the report is saved.
    if let Some(error) = report["rebuild"]["error"].as_str() {
        panic!("the rebuild failed to start: {error}");
    }
    assert_eq!(
        report["rebuild"]["reference_hits_after"],
        json!(1),
        "the rebuild lost the exact reference"
    );
    block_on(engine.shutdown()).unwrap();
}

/// Stops the background importer when dropped, also while a timed phase
/// unwinds: the scope joins the importer first, and it only stops on
/// this flag.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn push_workload(report: &mut Value, workload: Value) {
    report["workloads"].as_array_mut().unwrap().push(workload);
}

/// Write the report so far to `SB_SEARCH_TIMINGS_OUT`, whole: a
/// temporary file renamed over the last one.
fn save(report: &Value) {
    let Some(path) = std::env::var_os("SB_SEARCH_TIMINGS_OUT") else {
        return;
    };
    let path = PathBuf::from(path);
    let partial = path.with_extension("partial");
    fs::write(&partial, serde_json::to_string_pretty(report).unwrap()).unwrap();
    fs::rename(&partial, &path).unwrap();
}

/// The query set (spec/11): an exact identifier and an exact address,
/// a phrase, a scoped filter, a negative body query and a common term,
/// by relevance and by date.
fn probes(rows: usize, projects: i64) -> Vec<Probe> {
    let reference = rows / 100 / 2;
    let classes = [
        ("exact-reference", "exact identifier", format!(r#"{{"text":"ref{reference}"}}"#), SearchScope::All),
        (
            "exact-address",
            "exact identifier",
            r#"{"field":"from","op":"is","value":"sender17@beta.example"}"#.to_owned(),
            SearchScope::All,
        ),
        ("phrase", "phrase", r#"{"text":"quarterly budget review"}"#.to_owned(), SearchScope::All),
        (
            "scoped-filter",
            "scoped filter",
            r#"{"and":[{"field":"from","op":"domain","value":"beta.example"},{"date":{"after":"2024-03-01","tz":"UTC"}}]}"#.to_owned(),
            SearchScope::Mailbox(projects),
        ),
        (
            "negative-body",
            "negative body",
            r#"{"and":[{"text":"budget"},{"not":{"field":"body","op":"contains","value":"review"}}]}"#.to_owned(),
            SearchScope::All,
        ),
        // A quarter of all senders: a driver whose kept set is large
        // (spec/11 M1.6r2).
        ("common-term", "common term", r#"{"text":"beta"}"#.to_owned(), SearchScope::All),
    ];
    let mut probes = Vec::new();
    for (name, class, query, scope) in classes {
        for (sort, label) in [
            (SearchSort::Relevance, "relevance"),
            (SearchSort::Date, "date"),
        ] {
            probes.push(Probe {
                id: format!("{name}/{label}"),
                class,
                json: format!(r#"{{"v":1,"q":{query}}}"#),
                scope: scope.clone(),
                sort,
            });
        }
    }
    probes
}

/// Whether imports run during a cold sample.
#[derive(Clone, Copy)]
enum Background<'a> {
    Off,
    /// Each reopen starts an import whose writes run while the query is
    /// timed.
    Imports(&'a Path),
}

/// Cold first-50 timings: each sample reopens the core, then times one
/// query. The OS page cache is not flushed (the report says so).
fn cold(
    mut engine: Engine,
    root: &Path,
    probes: &[Probe],
    background: Background,
) -> (Value, Engine) {
    let mut samples: Vec<Vec<Result<f64, String>>> = probes.iter().map(|_| Vec::new()).collect();
    let mut opens = Vec::new();
    let mut overlapped = 0;
    for _ in 0..COLD_ROUNDS {
        for (index, probe) in probes.iter().enumerate() {
            block_on(engine.shutdown()).unwrap();
            let opened = Instant::now();
            engine = block_on(Engine::open(root)).unwrap();
            opens.push(ms(opened.elapsed()));
            let job = match background {
                Background::Off => None,
                Background::Imports(dir) => {
                    Some(start(&engine, vec![batch(dir, BACKGROUND_BATCH)]))
                }
            };
            samples[index].push(first_page(&engine, probe));
            if let Some(job) = job {
                if !block_on(engine.import_job(&job)).unwrap().state().ended() {
                    overlapped += 1;
                }
                finish(&engine, &job);
            }
        }
    }
    let per_query: Vec<Value> = probes
        .iter()
        .zip(&samples)
        .map(|(probe, samples)| json!({"id": probe.id, "first50": summary(samples)}))
        .collect();
    let mut value = json!({"queries": per_query, "core_open_ms": stats(&opens)});
    if let Background::Imports(_) = background {
        value["import_still_running_when_timed"] = json!(overlapped);
        value["messages_imported"] = json!(COLD_ROUNDS * probes.len() * BACKGROUND_BATCH);
    }
    (value, engine)
}

/// The warm sample: `WARM_SAMPLE` first-50 opens cycling the query set,
/// also split by whether the open scanned coverage's scope or reused its
/// counts from the same generation (spec/07).
fn warm(engine: &Engine, probes: &[Probe]) -> Value {
    let mut samples: Vec<Vec<Result<f64, String>>> = probes.iter().map(|_| Vec::new()).collect();
    let (mut scanned, mut reused) = (Vec::new(), Vec::new());
    for n in 0..WARM_SAMPLE {
        let index = n % probes.len();
        let scans = engine.search_memo().coverage_scans();
        let sample = first_page(engine, &probes[index]);
        if engine.search_memo().coverage_scans() > scans {
            scanned.push(sample.clone());
        } else {
            reused.push(sample.clone());
        }
        samples[index].push(sample);
    }
    let all: Vec<Result<f64, String>> = samples.iter().flatten().cloned().collect();
    json!({
        "all": summary(&all),
        "coverage_scanned": summary(&scanned),
        "coverage_reused": summary(&reused),
        "queries": probes.iter().zip(&samples).map(|(probe, samples)| json!({
            "id": probe.id, "first50": summary(samples),
        })).collect::<Vec<_>>(),
    })
}

/// One first-50 open, closed again: milliseconds, or the failure.
fn first_page(engine: &Engine, probe: &Probe) -> Result<f64, String> {
    let started = Instant::now();
    let window = block_on(engine.open_window(probe.spec(), Anchor::Top, FIRST_PAGE));
    let elapsed = ms(started.elapsed());
    match window {
        Ok(window) => {
            block_on(engine.close_window(window.window_id()));
            Ok(elapsed)
        }
        Err(error) => Err(format!("{error:?}")),
    }
}

/// spec/07: the exact unknown count is part of every first page; timed
/// here on its own, beside the matching count.
fn counts_json(engine: &Engine, probes: &[Probe]) -> Vec<Value> {
    probes
        .iter()
        .step_by(2)
        .map(|probe| {
            let query = Query::parse_json(&probe.json).unwrap();
            let scope = probe.store_scope();
            let mut times = Vec::new();
            let mut last = None;
            for _ in 0..DETAIL_ROUNDS {
                let (query, scope) = (query.clone(), scope.clone());
                let started = Instant::now();
                let found = engine
                    .resources()
                    .read(move |tx| counts(tx, &query, scope))
                    .unwrap();
                times.push(ms(started.elapsed()));
                last = Some(found);
            }
            let found = last.unwrap();
            json!({
                "id": probe.id.split('/').next().unwrap(),
                "ms": stats(&times),
                "matching": found.matching_messages(),
                "excluded_unknown": found.excluded_unknown(),
            })
        })
        .collect()
}

/// Scoped coverage, which every Search read also takes: `ms` scans the
/// scope each time; `reused_ms` is the engine's read at an unchanged
/// generation, which reuses the scope counts.
fn coverage_timings(engine: &Engine, probes: &[Probe]) -> Value {
    let mut scopes: Vec<SearchScope> = Vec::new();
    for probe in probes {
        if !scopes.contains(&probe.scope) {
            scopes.push(probe.scope.clone());
        }
    }
    scopes
        .into_iter()
        .map(|scope| {
            let stored = scope.stored().unwrap();
            let scans: Vec<f64> = (0..DETAIL_ROUNDS)
                .map(|_| {
                    let stored = stored.clone();
                    let started = Instant::now();
                    engine
                        .resources()
                        .read(move |tx| read_search_coverage(tx, stored))
                        .unwrap();
                    ms(started.elapsed())
                })
                .collect();
            // Prime the memo, so every timed read below reuses the counts.
            block_on(engine.coverage(scope.clone())).unwrap();
            let reused: Vec<f64> = (0..DETAIL_ROUNDS)
                .map(|_| {
                    let started = Instant::now();
                    block_on(engine.coverage(scope.clone())).unwrap();
                    ms(started.elapsed())
                })
                .collect();
            json!({"scope": format!("{scope:?}"), "ms": stats(&scans), "reused_ms": stats(&reused)})
        })
        .collect()
}

/// The id index and a 200-row page nine tenths down, idle only: under
/// writes every commit moves the generation and resets them.
fn ids_and_deep(engine: &Engine, probes: &[Probe]) -> Vec<Value> {
    probes
        .iter()
        .map(|probe| {
            let window = block_on(engine.open_window(probe.spec(), Anchor::Top, FIRST_PAGE)).unwrap();
            let mut ids_ms = Vec::new();
            let mut deep_ms = Vec::new();
            let mut hits = 0;
            for _ in 0..DETAIL_ROUNDS {
                let started = Instant::now();
                let index = block_on(engine.window_ids(window.window_id())).unwrap();
                ids_ms.push(ms(started.elapsed()));
                hits = index.ids().len();
                if hits > LATER_PAGE as usize {
                    let deep = index.ids()[hits * 9 / 10];
                    let started = Instant::now();
                    block_on(engine.read_window(
                        window.window_id(),
                        index.generation(),
                        Anchor::Around(deep),
                        LATER_PAGE,
                    ))
                    .unwrap();
                    deep_ms.push(ms(started.elapsed()));
                }
            }
            block_on(engine.close_window(window.window_id()));
            json!({"id": probe.id, "hits": hits, "ids_ms": stats(&ids_ms), "deep200_ms": stats(&deep_ms)})
        })
        .collect()
}

/// Commit-to-searchable: import one message with a unique word, then
/// search for it until it shows; the latency is the first hit's core
/// time minus the acquisition's committed `first_acquired_at`. A probe
/// that never shows within the deadline is a recorded failure.
fn freshness(engine: &Engine, dir: &Path, label: &str) -> Value {
    let mut samples = Vec::new();
    for n in 0..FRESHNESS_SAMPLES {
        let word = format!("fresh{label}{n}");
        let path = dir.join(format!("fresh-{label}-{n}.mbox"));
        fs::write(
            &path,
            message(
                &format!("<{word}@example.test>"),
                "probe@example.test",
                &format!("Freshness {word}"),
                0,
                &format!("Freshness probe {word}."),
            ),
        )
        .unwrap();
        let job = start(engine, vec![path]);
        let spec = WindowSpec::Search(SearchSpec::new(
            format!(r#"{{"v":1,"q":{{"text":"{word}"}}}}"#),
            SearchScope::All,
            SearchSort::Relevance,
        ));
        let deadline = Instant::now() + FRESHNESS_DEADLINE;
        let sample = loop {
            if let Ok(window) = block_on(engine.open_window(spec.clone(), Anchor::Top, 1)) {
                let hit = engine.resources().now().as_millis();
                let found = !window.rows().is_empty();
                block_on(engine.close_window(window.window_id()));
                if found {
                    let subject = format!("Freshness {word}");
                    let committed: i64 = engine
                        .resources()
                        .read(move |tx| {
                            tx.query_row(
                                "SELECT a.first_acquired_at FROM acquisitions a
                                 JOIN d_message_summary s ON s.acquisition_id = a.acquisition_id
                                 WHERE s.subject = ?1",
                                [subject],
                                |row| row.get(0),
                            )
                            .map_err(sql_error)
                        })
                        .unwrap();
                    break Ok((hit - committed) as f64);
                }
            }
            if Instant::now() >= deadline {
                break Err(format!("not searchable within {FRESHNESS_DEADLINE:?}"));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        samples.push(sample);
        finish(engine, &job);
    }
    json!({
        "ms": summary(&samples),
        "poll": "a one-row search each round, then a 2 ms sleep; a round costs a first page",
    })
}

/// A full rebuild of the archive, last: reset and enqueue make Search
/// unavailable and move every generation. The exact reference is found
/// again afterwards.
fn rebuild(engine: &Engine, probe: &Probe) -> Value {
    let started = Instant::now();
    // Start commits the new job in reset, so "complete" means this one.
    if let Err(error) = block_on(engine.start_search_rebuild(&engine.new_idempotency_key())) {
        return json!({"error": format!("{error:?}")});
    }
    settle(engine);
    let elapsed = ms(started.elapsed());
    let found = match block_on(engine.open_window(probe.spec(), Anchor::Top, FIRST_PAGE)) {
        Ok(window) => {
            let hits = window.rows().len();
            block_on(engine.close_window(window.window_id()));
            json!(hits)
        }
        Err(error) => json!(format!("{error:?}")),
    };
    json!({
        "ms": elapsed,
        "documents": value(engine, "SELECT count(*) FROM d_search_docs"),
        "reference_hits_after": found,
    })
}

/// Import a batch of new messages every `BACKGROUND_PAUSE` until
/// `stop`: real import and index writes at a steady sync rate.
fn background(engine: &Engine, dir: &Path, stop: &AtomicBool, imported: &AtomicU64) {
    while !stop.load(Ordering::SeqCst) {
        let job = start(engine, vec![batch(dir, BACKGROUND_BATCH)]);
        finish(engine, &job);
        imported.fetch_add(BACKGROUND_BATCH as u64, Ordering::SeqCst);
        let resume = Instant::now() + BACKGROUND_PAUSE;
        while Instant::now() < resume && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The synthetic archive: seven tenths in Archive, the rest in Projects.
/// Senders come from four domains, subjects and bodies from a seeded
/// vocabulary; one message in twenty mentions the phrase, one in a
/// hundred carries a unique reference word.
fn write_corpus(dir: &Path, rows: usize) -> (PathBuf, PathBuf) {
    let mut rng = Rng(SEED);
    let split = rows - rows * 3 / 10;
    let mut archive = String::new();
    let mut projects = String::new();
    for n in 0..rows {
        let sender = rng.below(500);
        let domain = ["alpha", "beta", "gamma", "delta"][sender % 4];
        let mut subject = (0..4)
            .map(|_| word(rng.below(VOCABULARY)))
            .collect::<Vec<_>>()
            .join(" ");
        let mut body = (0..24)
            .map(|_| word(rng.below(VOCABULARY)))
            .collect::<Vec<_>>()
            .join(" ");
        if rng.below(20) == 0 {
            body.push_str(" The quarterly budget review is attached.");
        }
        // "budget" without "review": hits for the negative body query.
        if rng.below(30) == 0 {
            body.push_str(" The budget draft is pending.");
        }
        if n % 100 == 0 {
            subject.push_str(&format!(" ref{}", n / 100));
            body.push_str(&format!(" Reference ref{} follows.", n / 100));
        }
        // Three to a timestamp, so date pages cross ties, spread over
        // the same 240 days from 2024-01-01 whatever the archive's size.
        let date = 1_704_067_200 + (n as i64 / 3) * (DATE_SPAN_SECONDS * 3 / rows as i64);
        let text = message(
            &format!("<timing-{n}@example.test>"),
            &format!("sender{sender}@{domain}.example"),
            &subject,
            date,
            &body,
        );
        if n < split {
            archive.push_str(&text)
        } else {
            projects.push_str(&text)
        }
    }
    let archive_path = dir.join("Archive.mbox");
    let projects_path = dir.join("Projects.mbox");
    fs::write(&archive_path, archive).unwrap();
    fs::write(&projects_path, projects).unwrap();
    (archive_path, projects_path)
}

/// A batch of `count` new messages, for background writes.
fn batch(dir: &Path, count: usize) -> PathBuf {
    let label = format!("background-{}", BATCHES.fetch_add(1, Ordering::SeqCst));
    let text: String = (0..count)
        .map(|n| {
            message(
                &format!("<{label}-{n}@example.test>"),
                "busy@alpha.example",
                &format!("Background {label} {n}"),
                1_800_000_000 + n as i64,
                "Background import body.",
            )
        })
        .collect();
    let path = dir.join(format!("{label}.mbox"));
    fs::write(&path, text).unwrap();
    path
}

fn message(id: &str, from: &str, subject: &str, date: i64, body: &str) -> String {
    let date = format_date(date);
    format!(
        "From {from} Mon Jan  1 00:00:00 2024\nMessage-ID: {id}\nFrom: Sender <{from}>\nTo: Owner <owner@example.test>\nDate: {date}\nSubject: {subject}\n\n{body}\n\n"
    )
}

/// RFC 5322 date for seconds since the epoch, UTC.
fn format_date(seconds: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        rest / 3_600,
        rest / 60 % 60,
        rest % 60
    )
}

const VOCABULARY: usize = 1_200;
/// The archive's dates span 240 days.
const DATE_SPAN_SECONDS: i64 = 240 * 86_400;

/// A pronounceable synthetic word for `n` (two or three syllables).
fn word(n: usize) -> String {
    const ONSETS: [&str; 12] = ["b", "d", "f", "g", "k", "l", "m", "n", "p", "r", "s", "t"];
    const VOWELS: [&str; 5] = ["a", "e", "i", "o", "u"];
    let syllable = |k: usize| format!("{}{}", ONSETS[k % 12], VOWELS[k / 12 % 5]);
    let mut text = syllable(n % 60) + &syllable(n / 60 % 60);
    if n >= 600 {
        text += &syllable(n / 7 % 60);
    }
    text
}

/// xorshift64*: deterministic for a seed, on every platform.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % bound as u64) as usize
    }
}

/// Mark one document in fifty failed, as an extraction failure leaves
/// it: out of the full-text index, its body unknown to queries. Nothing
/// re-queues them, so coverage holds still while timings run.
fn fail_every_fiftieth(engine: &Engine) -> i64 {
    block_on(engine.resources().write(sb_resources::WritePriority::User, |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO d_search_fts (d_search_fts, rowid, subject, from_text, to_text, cc_text,
               body_authored, body_quoted, body_signature, attachment_names)
               SELECT 'delete', doc_id, coalesce(subject, ''), coalesce(from_text, ''),
                 coalesce(to_text, ''), coalesce(cc_text, ''), coalesce(body_authored, ''),
                 coalesce(body_quoted, ''), coalesce(body_signature, ''), coalesce(attachment_names, '')
               FROM d_search_docs WHERE doc_id % {FAILED_EVERY} = 0 AND state IN ('indexed','partial');
             UPDATE d_search_docs SET state = 'failed', truncated_fields = '[]'
               WHERE doc_id % {FAILED_EVERY} = 0;
             UPDATE store_meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)
               WHERE key = 'search_generation';"
        ))
        .map_err(sql_error)?;
        tx.query_row("SELECT count(*) FROM d_search_docs WHERE state = 'failed'", [], |row| row.get(0))
            .map_err(sql_error)
    }))
    .unwrap()
}

/// The archive is what the timings claim: every message counted, the
/// failed ones unknown, the queue empty, the reference found once and
/// the phrase often enough for deep pages.
fn check_corpus(engine: &Engine, probes: &[Probe], rows: usize, failed: i64) {
    let coverage = block_on(engine.coverage(SearchScope::All)).unwrap();
    assert_eq!(coverage.total_messages, rows as u64);
    assert_eq!(coverage.failed, failed as u64);
    assert_eq!(
        coverage.indexed + coverage.partial + coverage.failed,
        rows as u64
    );
    assert_eq!(coverage.queue_length, 0);
    let first = |probe: &Probe| {
        let window = block_on(engine.open_window(probe.spec(), Anchor::Top, FIRST_PAGE)).unwrap();
        block_on(engine.close_window(window.window_id()));
        window
    };
    assert_eq!(first(&probes[0]).rows().len(), 1, "one exact reference");
    // Every query class finds something, so pages carry rows.
    for probe in probes.iter().step_by(2) {
        assert!(
            !first(probe).rows().is_empty(),
            "{} finds nothing",
            probe.id
        );
    }
    match first(&probes[4]).total() {
        Count::Exact(n) | Count::AtLeast(n) => assert!(n >= FIRST_PAGE as u64, "a common phrase"),
        Count::Unknown => panic!("the phrase count is unknown"),
    }
}

fn coverage_json(engine: &Engine) -> Value {
    let c = block_on(engine.coverage(SearchScope::All)).unwrap();
    json!({
        "generation": c.generation, "total_messages": c.total_messages, "indexed": c.indexed,
        "partial": c.partial, "failed": c.failed, "pending": c.pending,
        "queue_length": c.queue_length, "index_state": format!("{:?}", c.index_state),
    })
}

fn environment(engine: &Engine) -> Value {
    let sqlite: String = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT sqlite_version()", [], |row| row.get(0))
                .map_err(sql_error)
        })
        .unwrap();
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": cpu(),
        "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "sqlite_version": sqlite,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "rustc": std::env::var("SB_RUSTC_VERSION").unwrap_or_else(|_| "unrecorded".to_owned()),
        "commit": std::env::var("GITHUB_SHA").unwrap_or_else(|_| "unrecorded".to_owned()),
        "runner": std::env::var("ImageOS").unwrap_or_else(|_| "local".to_owned()),
        "os_cache_state": "not flushed: one process, the archive's pages cached since the import",
    })
}

fn cpu() -> String {
    if let Ok(info) = fs::read_to_string("/proc/cpuinfo")
        && let Some(line) = info.lines().find(|line| line.starts_with("model name"))
    {
        return line.split(':').nth(1).unwrap_or("").trim().to_owned();
    }
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Percentiles of successful samples, with every failure listed.
fn summary(samples: &[Result<f64, String>]) -> Value {
    let ok: Vec<f64> = samples
        .iter()
        .filter_map(|s| s.as_ref().ok().copied())
        .collect();
    let failures: Vec<&String> = samples.iter().filter_map(|s| s.as_ref().err()).collect();
    let mut value = stats(&ok);
    value["samples"] = json!(samples.len());
    value["failures"] = json!(failures.len());
    value["failure_examples"] = json!(failures.iter().take(5).collect::<Vec<_>>());
    value
}

/// Nearest-rank p50/p95/p99 and the extremes, in milliseconds.
fn stats(samples: &[f64]) -> Value {
    if samples.is_empty() {
        return json!({"count": 0});
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank =
        |q: f64| sorted[((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1];
    json!({
        "count": sorted.len(), "min": sorted[0], "p50": rank(0.50), "p95": rank(0.95),
        "p99": rank(0.99), "max": sorted[sorted.len() - 1],
    })
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

/// Start an import of `paths` and confirm it: its writes then run.
fn start(engine: &Engine, paths: Vec<PathBuf>) -> String {
    let id = block_on(engine.start_import(&engine.new_idempotency_key(), paths))
        .unwrap()
        .job_id()
        .to_owned();
    wait_for(engine, &id, ImportJobState::AwaitingConfirmation);
    block_on(engine.confirm_import(&engine.new_idempotency_key(), &id, Vec::new())).unwrap();
    id
}

fn finish(engine: &Engine, id: &str) {
    wait_for(engine, id, ImportJobState::Completed);
}

fn wait_for(engine: &Engine, id: &str, state: ImportJobState) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let job = block_on(engine.import_job(id)).unwrap();
        if job.state() == state {
            return;
        }
        assert!(
            !job.state().ended(),
            "import {id} ended {:?} before {state:?}",
            job.state()
        );
        assert!(
            Instant::now() < deadline,
            "import {id} never reached {state:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until indexing, any rebuild and thread building are done.
fn settle(engine: &Engine) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let settled = engine
            .resources()
            .read(|tx| {
                Ok(tx
                    .query_row("SELECT count(*) FROM d_index_queue", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(sql_error)?
                    == 0
                    && latest_rebuild(tx)?.is_none_or(|job| job.phase() == RebuildPhase::Complete)
                    && !sb_resources::thread_revisions(tx)?.stale())
            })
            .unwrap();
        if settled {
            return;
        }
        assert!(Instant::now() < deadline, "the index never settled");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether an import job reached a final state.
trait Ended {
    fn ended(&self) -> bool;
}

impl Ended for ImportJobState {
    fn ended(&self) -> bool {
        matches!(
            self,
            ImportJobState::Completed | ImportJobState::Cancelled | ImportJobState::Failed
        )
    }
}
