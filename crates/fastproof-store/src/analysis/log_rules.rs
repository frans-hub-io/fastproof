//! Log rules (contract §7.6): rules with `match.log`, `match.signals` or `action.pattern`,
//! evaluated after a launch by the analysis role (never at ingest), on demand as a dry run, and
//! retroactively by the `log_rule.apply` task; their pattern marks in `result_patterns`.
//!
//! What a rule reads is bounded at every level: the failed and broken attempts of the launch,
//! newest first, at most `log_attempts`; of each, the last `lastBytes` of its log (the live log
//! chunks, decoded one at a time with a size cap, else the captured `stdout`/`stderr` of the
//! uploaded result); every line cut to [`rules::MATCH_INPUT_BYTES`]; patterns compiled by
//! [`rules::compile_regex`] (the `regex` crate: linear time, size limits) and charged to a
//! [`Budget`]; the time spent decoding and matching bounded by `log_cpu_ms` per launch. A run
//! that stops at a budget is `partial` and says which budget ran out.
//!
//! Effects other than the pattern mark go through the retroactive path: one journal batch per
//! rule (`via = retro`, `source = rule`), results a person or an agent triaged are left alone,
//! and the batch can be undone.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_model::Status;
use serde::Deserialize;
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::hook::{add_retro_mutes, is_protected, rule_defect, type_refs};
use super::rules::{self, Budget, BudgetExhausted, CompiledRule, MATCH_INPUT_BYTES, Rule, Subject};
use super::tasks::{DryRun, MAX_DRY_RUN_IDS};
use super::triage::recount_muted;
use super::types::{
    AnalysisLimits, LaunchCtx, LogMatch, PatternCount, ResultPattern, SignalSource, StepOutcome,
};
use crate::logs::{LogLevel, LogStream};
use crate::{Defect, Store, json_no_nul, no_nul};

/// Characters of a pattern mark's excerpt, at most.
pub const EXCERPT_CHARS: usize = 300;
/// Largest decoded log chunk read (chunks are written at most 1 MiB of lines).
const CHUNK_DECODED_MAX: u64 = 8 << 20;
/// Largest decoded captured `stdout`/`stderr` document of an attempt read; larger ones are
/// skipped (and counted).
pub const LOGS_Z_DECODED_MAX: u64 = 16 << 20;
/// Attempts whose logs are fetched and matched together.
const ATTEMPT_BATCH: usize = 20;
/// Pattern marks listed per result, at most.
const MAX_RESULT_PATTERNS: i64 = 100;
/// Signals of a launch read for `match.signals`, at most.
const MAX_LAUNCH_SIGNALS: i64 = 200_000;
/// Lines of one attempt's tail, at most (a log of very short lines is cut to its newest ones).
pub const MAX_TAIL_LINES: usize = 131_072;

/// Narrows a results query (alias `r`) to results marked with `pattern`.
pub fn result_pattern_filter(qb: &mut QueryBuilder<Postgres>, pattern: &str) {
    qb.push(
        " and exists(select 1 from result_patterns rp where rp.launch_id = r.launch_id \
         and rp.result_id = r.id and rp.pattern = ",
    )
    .push_bind(no_nul(pattern))
    .push(")");
}

/// A line of a stored log chunk (the fields the rules read).
#[derive(Deserialize)]
struct ChunkLine {
    q: i64,
    l: LogLevel,
    s: LogStream,
    m: String,
    #[serde(default)]
    p: Option<String>,
}

/// The captured output of an uploaded attempt.
#[derive(Deserialize, Default)]
struct CapturedLogs {
    #[serde(default)]
    stdout: Option<Captured>,
    #[serde(default)]
    stderr: Option<Captured>,
}

#[derive(Deserialize)]
struct Captured {
    text: String,
}

/// zstd then JSON, refusing documents that decode to more than `cap` bytes.
fn decode_capped<T: serde::de::DeserializeOwned>(z: &[u8], cap: u64) -> Option<T> {
    let mut out = Vec::new();
    let dec = zstd::stream::read::Decoder::new(z).ok()?;
    let n = dec.take(cap + 1).read_to_end(&mut out).ok()?;
    if n as u64 > cap {
        return None;
    }
    serde_json::from_slice(&out).ok()
}

/// A line of the tail of an attempt's log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailLine {
    /// Sequence number of a live log line; none for captured output.
    pub seq: Option<i64>,
    /// Retry number the line was sent with.
    pub attempt: Option<i32>,
    /// None for captured output (it has no levels).
    pub level: Option<LogLevel>,
    pub stream: LogStream,
    pub text: String,
}

/// The tail of an attempt's log: lines newest first, `bytes` counting the text and step name
/// of each plus one byte for its end (an empty line costs a byte too, so a log of empty lines
/// cannot grow a tail without bound), at most the size it was built for and at most
/// [`MAX_TAIL_LINES`] lines. The oldest line kept is cut to fit (its last bytes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    pub lines: Vec<TailLine>,
    pub bytes: usize,
}

impl Tail {
    /// Adds the next older line; false once the tail is full.
    fn push_older(&mut self, mut line: TailLine, step_bytes: usize, max: usize) -> bool {
        if self.bytes >= max || self.lines.len() >= MAX_TAIL_LINES {
            return false;
        }
        let left = max - self.bytes;
        let size = line.text.len() + step_bytes + 1;
        if size > left {
            let keep = left.saturating_sub(step_bytes + 1).min(line.text.len());
            let mut start = line.text.len() - keep;
            while !line.text.is_char_boundary(start) {
                start += 1;
            }
            line.text.drain(..start);
            self.bytes = max;
        } else {
            self.bytes += size;
        }
        self.lines.push(line);
        true
    }

    /// The tail of captured text (one stream, no levels): its last `max` bytes as lines.
    fn of_captured(text: &str, stream: LogStream, max: usize) -> Tail {
        let mut t = Tail::default();
        for l in text.trim_end_matches(['\n', '\r']).rsplit('\n') {
            let line = TailLine {
                seq: None,
                attempt: None,
                level: None,
                stream,
                text: l.strip_suffix('\r').unwrap_or(l).to_string(),
            };
            if !t.push_older(line, 0, max) {
                break;
            }
        }
        t
    }
}

/// The text of a mark: control characters and ANSI escapes dropped, tabs as spaces, at most
/// [`EXCERPT_CHARS`] characters, starting shortly before the match (`at`, a byte offset in
/// `line`) when it lies further in.
pub fn excerpt(line: &str, at: Option<usize>) -> String {
    let start = match at {
        Some(a) if a > 100 => {
            let mut s = (a - 100).min(line.len());
            while !line.is_char_boundary(s) {
                s -= 1;
            }
            s
        }
        _ => 0,
    };
    let mut out = String::new();
    let mut n = 0usize;
    let mut chars = line[start..].chars().peekable();
    while let Some(c) = chars.next() {
        if n >= EXCERPT_CHARS {
            break;
        }
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\t' || c == '\u{2028}' || c == '\u{2029}' {
            out.push(' ');
        } else if c.is_control() || invisible(c) {
            continue;
        } else {
            out.push(c);
        }
        n += 1;
    }
    out.trim().to_string()
}

/// Characters that change how text around them is shown without showing themselves: the
/// bidirectional embeddings, overrides and isolates and the zero-width characters. An excerpt
/// drops them so a log line cannot reorder or hide what a reader sees.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// What a matched result got from a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub attempt: i32,
    pub seq: Option<i64>,
    pub excerpt: String,
}

/// Scans a tail (newest line first) for the first line a rule's `match.log` matches within
/// its own `last` bytes: the line's index and the byte offset of the match in it.
pub fn scan_tail(
    log: &rules::CompiledLog,
    tail: &Tail,
    last: usize,
    budget: &mut Budget,
) -> Result<Option<(usize, Option<usize>)>, BudgetExhausted> {
    scan_tail_until(log, tail, last, budget, None).map_err(|_| BudgetExhausted)
}

/// Why a scan of a tail stopped before its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanStop {
    /// The regex budget ran out.
    Budget,
    /// The time of the pass ran out (`deadline`).
    Time,
}

/// [`scan_tail`] that also stops at `deadline`, checked before every line: one attempt's tail
/// read by many rules is the largest unit of work of a pass, so the CPU budget of the pass
/// is enforced inside it, not only between attempts.
pub fn scan_tail_until(
    log: &rules::CompiledLog,
    tail: &Tail,
    last: usize,
    budget: &mut Budget,
    deadline: Option<Instant>,
) -> Result<Option<(usize, Option<usize>)>, ScanStop> {
    if !log.begin_scan(budget) {
        return Err(ScanStop::Budget);
    }
    let mut used = 0usize;
    for (i, l) in tail.lines.iter().enumerate() {
        if used >= last {
            break;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(ScanStop::Time);
        }
        let room = last - used;
        used += l.text.len() + 1;
        if !log.wants(l.level, l.stream) {
            continue;
        }
        let text = if l.text.len() > room {
            let mut s = l.text.len() - room;
            while !l.text.is_char_boundary(s) {
                s += 1;
            }
            &l.text[s..]
        } else {
            &l.text[..]
        };
        match log.line_matches(text, budget) {
            None => return Err(ScanStop::Budget),
            Some(false) => {}
            Some(true) => {
                let at = log.find(rules::match_input(text));
                let offset = l.text.len() - text.len();
                return Ok(Some((i, at.map(|a| a + offset))));
            }
        }
    }
    Ok(None)
}

/// The launch a pass works on.
#[derive(Clone, Debug)]
struct Scope {
    project_id: Uuid,
    project_key: String,
    launch_id: Uuid,
    created: DateTime<Utc>,
    env: Option<String>,
}

/// A failed or broken attempt with the result fields the rules look at.
#[derive(sqlx::FromRow, Clone, Debug)]
struct AttemptRow {
    id: Uuid,
    result_id: Uuid,
    client_key: String,
    attempt_no: i32,
    has_logs: bool,
    trace: Option<String>,
    status: String,
    test_case_id: Uuid,
    message: Option<String>,
    fingerprint: Option<String>,
}

/// Why a pass stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Cpu,
    Regex,
}

/// The outcome of evaluating rules on a launch, nothing written yet.
#[derive(Debug, Default)]
struct Evaluation {
    /// Matches per result, by rule index (the first, newest attempt that matched).
    hits: BTreeMap<Uuid, BTreeMap<usize, Hit>>,
    attempts: u64,
    attempts_truncated: bool,
    results: u64,
    skipped_large: u64,
    cpu: Duration,
    stopped: Option<Stop>,
}

impl Evaluation {
    fn complete(&self) -> bool {
        self.stopped.is_none() && !self.attempts_truncated
    }
}

/// Signals of the launch's results: `(source, kind)` per result.
type Signals = HashMap<Uuid, Vec<(String, String)>>;

fn signals_hold(rule: &CompiledRule, sig: Option<&Vec<(String, String)>>) -> bool {
    let Some(want) = &rule.rule.matcher.signals else {
        return true;
    };
    let have = sig.map(Vec::as_slice).unwrap_or(&[]);
    want.iter().all(|w| {
        have.iter()
            .any(|(s, k)| s == source_name(w.source) && w.kind.as_ref().is_none_or(|wk| wk == k))
    })
}

fn source_name(s: SignalSource) -> &'static str {
    s.as_str()
}

/// The rules (by index) whose result conditions and signals hold for an attempt's result and
/// that did not match that result yet; `Err` when the budget ran out.
fn candidates(
    rules: &[Arc<CompiledRule>],
    a: &AttemptRow,
    env: Option<&str>,
    signals: &Signals,
    done: Option<&BTreeMap<usize, Hit>>,
    budget: &mut Budget,
) -> Result<Vec<usize>, BudgetExhausted> {
    let s = Subject {
        status: &a.status,
        message: a.message.as_deref(),
        trace: a.trace.as_deref(),
        test_case_id: a.test_case_id,
        fingerprint: a.fingerprint.as_deref(),
        environment: env,
    };
    let mut out = Vec::new();
    for (i, r) in rules.iter().enumerate() {
        if done.is_some_and(|d| d.contains_key(&i)) {
            continue;
        }
        if !signals_hold(r, signals.get(&a.result_id)) {
            continue;
        }
        match r.result_matches(&s, budget) {
            None => return Err(BudgetExhausted),
            Some(true) => out.push(i),
            Some(false) => {}
        }
    }
    Ok(out)
}

/// The tail of one attempt from its compressed log chunks (newest first) or, without chunks,
/// from its captured output. `None` when a document is over its cap.
fn build_tail(chunks: &[(i32, Vec<u8>)], captured: Option<&[u8]>, max: usize) -> Option<Vec<Tail>> {
    if !chunks.is_empty() {
        let mut t = Tail::default();
        'chunks: for (attempt, z) in chunks {
            let lines: Vec<ChunkLine> = decode_capped(z, CHUNK_DECODED_MAX)?;
            for l in lines.into_iter().rev() {
                let step = l.p.as_ref().map_or(0, String::len);
                let line = TailLine {
                    seq: Some(l.q),
                    attempt: Some(*attempt),
                    level: Some(l.l),
                    stream: l.s,
                    text: l.m,
                };
                if !t.push_older(line, step, max) {
                    break 'chunks;
                }
            }
        }
        return Some(vec![t]);
    }
    let Some(z) = captured else {
        return Some(Vec::new());
    };
    let logs: CapturedLogs = decode_capped(z, LOGS_Z_DECODED_MAX)?;
    let mut out = Vec::new();
    if let Some(c) = &logs.stderr {
        out.push(Tail::of_captured(&c.text, LogStream::Stderr, max));
    }
    if let Some(c) = &logs.stdout {
        out.push(Tail::of_captured(&c.text, LogStream::Stdout, max));
    }
    Some(out)
}

/// Work of one attempt in a batch: its candidate rules and the log it is matched on.
struct AttemptWork {
    row: AttemptRow,
    rules: Vec<usize>,
    chunks: Vec<(i32, Vec<u8>)>,
    captured: Option<Vec<u8>>,
}

/// Results of matching a batch off the async threads.
struct BatchOutcome {
    hits: Vec<(Uuid, usize, Hit)>,
    skipped_large: u64,
    cpu: Duration,
    stopped: Option<Stop>,
}

fn match_batch(
    rules: &[Arc<CompiledRule>],
    work: Vec<AttemptWork>,
    default_last: usize,
    budget: &mut Budget,
    cpu_left: Duration,
) -> BatchOutcome {
    let started = Instant::now();
    let deadline = started.checked_add(cpu_left);
    let mut out = BatchOutcome {
        hits: Vec::new(),
        skipped_large: 0,
        cpu: Duration::ZERO,
        stopped: None,
    };
    let mut seen: HashSet<(Uuid, usize)> = HashSet::new();
    for w in work {
        if started.elapsed() >= cpu_left {
            out.stopped = Some(Stop::Cpu);
            break;
        }
        let row = &w.row;
        let mut with_log = Vec::new();
        for &i in &w.rules {
            if seen.contains(&(row.result_id, i)) {
                continue;
            }
            match rules[i].log() {
                None => {
                    seen.insert((row.result_id, i));
                    let text = row.message.as_deref().unwrap_or("");
                    let first = text.lines().next().unwrap_or("");
                    out.hits.push((
                        row.result_id,
                        i,
                        Hit {
                            attempt: row.attempt_no,
                            seq: None,
                            excerpt: excerpt(first, None),
                        },
                    ));
                }
                Some(_) => with_log.push(i),
            }
        }
        if with_log.is_empty() {
            continue;
        }
        let last_of = |i: usize| {
            rules[i]
                .log()
                .and_then(|l| l.last_bytes)
                .map_or(default_last, |b| b as usize)
        };
        let max = with_log.iter().map(|&i| last_of(i)).max().unwrap_or(0);
        let Some(tails) = build_tail(&w.chunks, w.captured.as_deref(), max) else {
            out.skipped_large += 1;
            continue;
        };
        for &i in &with_log {
            let Some(log) = rules[i].log() else { continue };
            let last = last_of(i);
            for t in &tails {
                match scan_tail_until(log, t, last, budget, deadline) {
                    Err(stop) => {
                        out.stopped = Some(match stop {
                            ScanStop::Budget => Stop::Regex,
                            ScanStop::Time => Stop::Cpu,
                        });
                        out.cpu = started.elapsed();
                        return out;
                    }
                    Ok(None) => {}
                    Ok(Some((li, at))) => {
                        let l = &t.lines[li];
                        seen.insert((row.result_id, i));
                        out.hits.push((
                            row.result_id,
                            i,
                            Hit {
                                attempt: l.attempt.unwrap_or(row.attempt_no),
                                seq: l.seq,
                                excerpt: excerpt(&l.text, at),
                            },
                        ));
                        break;
                    }
                }
            }
        }
    }
    out.cpu = started.elapsed();
    out
}

/// Counts of what a pass wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogRuleWrites {
    /// Pattern marks written.
    pub marked: u64,
    /// Results whose triage changed.
    pub updated: u64,
}

/// What the `log_rule.apply` task did on one launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetroLaunch {
    pub matched: u64,
    pub writes: LogRuleWrites,
    /// A budget stopped the evaluation before every attempt was read.
    pub partial: bool,
}

impl Store {
    async fn scope(&self, project_id: Uuid, launch_id: Uuid) -> anyhow::Result<Option<Scope>> {
        let r: Option<(DateTime<Utc>, Option<String>, String)> = sqlx::query_as(
            "select l.created_at, l.env_key, p.key from launches l join projects p on p.id = l.project_id \
             where l.project_id = $1 and l.id = $2",
        )
        .bind(project_id)
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|(created, env, key)| Scope {
            project_id,
            project_key: key,
            launch_id,
            created,
            env,
        }))
    }

    async fn launch_signals(&self, scope: &Scope) -> anyhow::Result<Signals> {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "select result_id, source, kind from attempt_signals where project_id = $1 and launch_id = $2 \
             and result_id is not null limit $3",
        )
        .bind(scope.project_id)
        .bind(scope.launch_id)
        .bind(MAX_LAUNCH_SIGNALS)
        .fetch_all(&self.pool)
        .await?;
        let mut out: Signals = HashMap::new();
        for (r, s, k) in rows {
            out.entry(r).or_default().push((s, k));
        }
        Ok(out)
    }

    /// Evaluates rules on the logs of a launch's failed and broken attempts; writes nothing.
    async fn evaluate_logs(
        &self,
        scope: &Scope,
        rules: &[Arc<CompiledRule>],
        limits: &AnalysisLimits,
        cpu_budget: Duration,
    ) -> anyhow::Result<Evaluation> {
        let mut ev = Evaluation::default();
        if rules.is_empty() {
            return Ok(ev);
        }
        let needs_trace = rules.iter().any(|r| r.needs_trace());
        let cut = MATCH_INPUT_BYTES as i32;
        let max_attempts = i64::from(limits.log_attempts.max(1));
        let mut rows: Vec<AttemptRow> = sqlx::query_as(
            "select a.id, a.result_id, a.client_key, \
               coalesce(case when jsonb_typeof(a.detail->'attempt') = 'number' \
                 then least(greatest(floor((a.detail->>'attempt')::numeric), 0), 100000)::int end, 0) as attempt_no, \
               a.logs_z is not null as has_logs, case when $4 then left(a.trace, $5) end as trace, \
               r.status, r.test_case_id, left(r.message, $5) as message, r.fingerprint \
             from attempts a join results r on r.id = a.result_id and r.launch_created_at = a.launch_created_at \
             where a.launch_id = $1 and a.launch_created_at = $2 and a.status in ('failed', 'broken') \
             order by a.started_at desc nulls last, a.seq desc, a.id limit $3",
        )
        .bind(scope.launch_id)
        .bind(scope.created)
        .bind(max_attempts + 1)
        .bind(needs_trace)
        .bind(cut)
        .fetch_all(&self.pool)
        .await?;
        if rows.len() as i64 > max_attempts {
            rows.truncate(max_attempts as usize);
            ev.attempts_truncated = true;
        }
        ev.attempts = rows.len() as u64;
        ev.results = rows
            .iter()
            .map(|r| r.result_id)
            .collect::<HashSet<_>>()
            .len() as u64;
        let signals = if rules.iter().any(|r| r.rule.matcher.signals.is_some()) {
            self.launch_signals(scope).await?
        } else {
            HashMap::new()
        };
        let default_last = limits.log_last_bytes as usize;
        let mut budget = Budget::launch();
        for batch in rows.chunks(ATTEMPT_BATCH) {
            if ev.cpu >= cpu_budget {
                ev.stopped = Some(Stop::Cpu);
                break;
            }
            let started = Instant::now();
            let mut work = Vec::new();
            for a in batch {
                let rs = match candidates(
                    rules,
                    a,
                    scope.env.as_deref(),
                    &signals,
                    ev.hits.get(&a.result_id),
                    &mut budget,
                ) {
                    Ok(v) => v,
                    Err(BudgetExhausted) => {
                        ev.stopped = Some(Stop::Regex);
                        break;
                    }
                };
                if !rs.is_empty() {
                    work.push(AttemptWork {
                        row: a.clone(),
                        rules: rs,
                        chunks: Vec::new(),
                        captured: None,
                    });
                }
            }
            ev.cpu += started.elapsed();
            if ev.stopped.is_some() {
                break;
            }
            self.fetch_logs(scope, rules, default_last, &mut work)
                .await?;
            let left = cpu_budget.saturating_sub(ev.cpu);
            let rules_owned: Vec<Arc<CompiledRule>> = rules.to_vec();
            let b = std::mem::replace(&mut budget, Budget::new(0, 0));
            let (b, outcome) = tokio::task::spawn_blocking(move || {
                let mut b = b;
                let o = match_batch(&rules_owned, work, default_last, &mut b, left);
                (b, o)
            })
            .await?;
            budget = b;
            ev.cpu += outcome.cpu;
            ev.skipped_large += outcome.skipped_large;
            for (r, i, h) in outcome.hits {
                ev.hits.entry(r).or_default().entry(i).or_insert(h);
            }
            if outcome.stopped.is_some() {
                ev.stopped = outcome.stopped;
                break;
            }
        }
        Ok(ev)
    }

    /// Fills the compressed logs of the attempts that need them: the newest chunks of each
    /// attempt's client id holding at least the largest `lastBytes` of its rules, and the
    /// captured output of attempts without chunks.
    async fn fetch_logs(
        &self,
        scope: &Scope,
        rules: &[Arc<CompiledRule>],
        default_last: usize,
        work: &mut [AttemptWork],
    ) -> anyhow::Result<()> {
        let mut keys = Vec::new();
        let mut lims = Vec::new();
        let mut idx: HashMap<String, Vec<usize>> = HashMap::new();
        for (n, w) in work.iter().enumerate() {
            let max = w
                .rules
                .iter()
                .filter_map(|&i| rules[i].log())
                .map(|l| l.last_bytes.map_or(default_last, |b| b as usize))
                .max();
            let Some(max) = max else { continue };
            let Some(key) = w.row.client_key.strip_prefix("r:") else {
                continue;
            };
            if !idx.contains_key(key) {
                keys.push(no_nul(key));
                lims.push(max as i64);
            }
            idx.entry(key.to_string()).or_default().push(n);
        }
        if !keys.is_empty() {
            let rows: Vec<(String, i32, Vec<u8>)> = sqlx::query_as(
                "select k.key, c.attempt, c.lines_z from unnest($2::text[], $3::bigint[]) as k(key, lim) \
                 cross join lateral (select x.attempt, x.first_seq, x.lines_z from (select c.attempt, c.first_seq, \
                     c.lines_z, (greatest(c.bytes - c.line_count * (32 + octet_length(c.result_key)), 0) + c.line_count)::bigint as est, \
                     sum((greatest(c.bytes - c.line_count * (32 + octet_length(c.result_key)), 0) + c.line_count)) \
                       over (order by c.attempt desc, c.first_seq desc) as acc \
                   from attempt_log_chunks c where c.launch_id = $1 and c.result_key = k.key) x \
                   where x.acc - x.est < k.lim order by x.attempt desc, x.first_seq desc) c",
            )
            .bind(scope.launch_id)
            .bind(&keys)
            .bind(&lims)
            .fetch_all(&self.pool)
            .await?;
            for (key, attempt, z) in rows {
                if let Some(ns) = idx.get(&key) {
                    for &n in ns {
                        work[n].chunks.push((attempt, z.clone()));
                    }
                }
            }
        }
        let captured: Vec<Uuid> = work
            .iter()
            .filter(|w| {
                w.chunks.is_empty()
                    && w.row.has_logs
                    && w.rules.iter().any(|&i| rules[i].log().is_some())
            })
            .map(|w| w.row.id)
            .collect();
        if !captured.is_empty() {
            let rows: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
                "select id, logs_z from attempts where launch_id = $1 and launch_created_at = $2 \
                 and id = any($3) and logs_z is not null",
            )
            .bind(scope.launch_id)
            .bind(scope.created)
            .bind(&captured)
            .fetch_all(&self.pool)
            .await?;
            let by_id: HashMap<Uuid, Vec<u8>> = rows.into_iter().collect();
            for w in work.iter_mut() {
                if w.chunks.is_empty()
                    && let Some(z) = by_id.get(&w.row.id)
                {
                    w.captured = Some(z.clone());
                }
            }
        }
        Ok(())
    }

    /// Writes the outcome of a pass: pattern marks (replacing the launch's earlier marks of
    /// these rules when the pass read everything) and, per rule with other effects, one retro
    /// batch over the results it was the first matching rule for. `by` names who applies the
    /// rule (`None`: the rule itself, `rule:<id>`).
    async fn write_evaluation(
        &self,
        scope: &Scope,
        rules: &[Arc<CompiledRule>],
        ev: &Evaluation,
        by: Option<&str>,
    ) -> anyhow::Result<LogRuleWrites> {
        let mut writes = LogRuleWrites::default();
        let mut marks: BTreeMap<(Uuid, String), (Uuid, &Hit)> = BTreeMap::new();
        let mut effective: BTreeMap<usize, Vec<Uuid>> = BTreeMap::new();
        let mut hits_of: HashMap<Uuid, i64> = HashMap::new();
        for (result, by_rule) in &ev.hits {
            let mut effect_taken = false;
            for (&i, h) in by_rule {
                let r = &rules[i].rule;
                *hits_of.entry(r.id).or_default() += 1;
                if let Some(p) = &r.action.pattern {
                    marks.entry((*result, p.clone())).or_insert((r.id, h));
                }
                if !effect_taken && has_effects(r) {
                    effect_taken = true;
                    effective.entry(i).or_default().push(*result);
                }
            }
        }
        let rule_ids: Vec<Uuid> = rules
            .iter()
            .filter(|r| r.rule.action.pattern.is_some())
            .map(|r| r.rule.id)
            .collect();
        if !marks.is_empty() || (ev.complete() && !rule_ids.is_empty()) {
            let mut tx = self.pool.begin().await?;
            let live: Option<i32> =
                sqlx::query_scalar("select 1 from launches where id = $1 for key share")
                    .bind(scope.launch_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if live.is_none() {
                return Ok(writes);
            }
            let mut m_result = Vec::with_capacity(marks.len());
            let mut m_pattern = Vec::with_capacity(marks.len());
            let mut m_rule = Vec::with_capacity(marks.len());
            let mut m_attempt = Vec::with_capacity(marks.len());
            let mut m_seq = Vec::with_capacity(marks.len());
            let mut m_excerpt = Vec::with_capacity(marks.len());
            for ((result, pattern), (rule, h)) in &marks {
                m_result.push(*result);
                m_pattern.push(pattern.clone());
                m_rule.push(*rule);
                m_attempt.push(h.attempt);
                m_seq.push(h.seq);
                m_excerpt.push(no_nul(&h.excerpt));
            }
            if ev.complete() {
                sqlx::query(
                    "delete from result_patterns p where p.launch_id = $1 and p.rule_id = any($2) \
                     and not exists (select 1 from unnest($3::uuid[], $4::text[]) as t(r, n) \
                       where t.r = p.result_id and t.n = p.pattern)",
                )
                .bind(scope.launch_id)
                .bind(&rule_ids)
                .bind(&m_result)
                .bind(&m_pattern)
                .execute(&mut *tx)
                .await?;
            }
            if !marks.is_empty() {
                let n = sqlx::query(
                    "insert into result_patterns (result_id, launch_id, launch_created_at, project_id, pattern, \
                       rule_id, attempt, line_seq, excerpt) \
                     select t.r, $1, $2, $3, t.n, t.u, t.a, t.q, t.e \
                     from unnest($4::uuid[], $5::text[], $6::uuid[], $7::int[], $8::bigint[], $9::text[]) \
                       as t(r, n, u, a, q, e) \
                     where exists (select 1 from results r where r.id = t.r and r.launch_created_at = $2 \
                       and r.launch_id = $1) \
                     on conflict (result_id, pattern) do update set rule_id = excluded.rule_id, \
                       attempt = excluded.attempt, line_seq = excluded.line_seq, excerpt = excluded.excerpt",
                )
                .bind(scope.launch_id)
                .bind(scope.created)
                .bind(scope.project_id)
                .bind(&m_result)
                .bind(&m_pattern)
                .bind(&m_rule)
                .bind(&m_attempt)
                .bind(&m_seq)
                .bind(&m_excerpt)
                .execute(&mut *tx)
                .await?;
                writes.marked = n.rows_affected();
            }
            if !hits_of.is_empty() {
                rules::add_hits(&mut tx, &hits_of).await?;
            }
            tx.commit().await?;
        } else if !hits_of.is_empty() {
            let mut conn = self.pool.acquire().await?;
            rules::add_hits(&mut conn, &hits_of).await?;
        }
        for (i, results) in effective {
            let rule = &rules[i];
            let rule_by = format!("rule:{}", rule.rule.id);
            writes.updated += self
                .apply_effects(scope, rule, &results, by.unwrap_or(&rule_by))
                .await?;
        }
        Ok(writes)
    }

    /// Applies a rule's triage effects (defect, comment, issue, mute, accept) to matched results
    /// of a launch that no person or agent triaged, as one `retro` batch; returns how many
    /// results changed. The lock order of every writer of results (`triage::lock_launches`).
    async fn apply_effects(
        &self,
        scope: &Scope,
        rule: &Arc<CompiledRule>,
        results: &[Uuid],
        by: &str,
    ) -> anyhow::Result<u64> {
        if results.is_empty() {
            return Ok(0);
        }
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let live: Option<i32> =
            sqlx::query_scalar("select 1 from launches where id = $1 for key share")
                .bind(scope.launch_id)
                .fetch_optional(&mut *tx)
                .await?;
        if live.is_none() {
            return Ok(0);
        }
        let mut ids: Vec<Uuid> = results.to_vec();
        ids.sort();
        let rows: Vec<(Uuid, String, Option<serde_json::Value>, Uuid, bool)> = sqlx::query_as(
            "select id, status, defect, series_id, muted from results where launch_id = $1 and launch_created_at = $2 \
             and id = any($3) order by id for no key update",
        )
        .bind(scope.launch_id)
        .bind(scope.created)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        let (by_id, by_group) = type_refs(&mut tx, scope.project_id).await?;
        let mut upd = Vec::new();
        let mut defects = Vec::new();
        let mut series = BTreeSet::new();
        for (id, status, defect, s, muted) in rows {
            let existing: Option<Defect> = defect.and_then(|d| serde_json::from_value(d).ok());
            if is_protected(existing.as_ref()) {
                continue;
            }
            let d = rule_defect(
                rule,
                Status::parse_lenient(&status),
                existing.as_ref(),
                &by_id,
                &by_group,
                now,
            );
            // A recompute that would set the same decision again changes nothing.
            if super::decisions::same_decision(existing.as_ref(), d.as_ref())
                && (rule.rule.action.mute.is_none() || muted)
            {
                continue;
            }
            upd.push(id);
            defects.push(d.map(serde_json::to_value).transpose()?.map(json_no_nul));
            series.insert(s);
        }
        if upd.is_empty() {
            return Ok(0);
        }
        let created = vec![scope.created; upd.len()];
        let order: Vec<(Uuid, DateTime<Utc>)> = upd.iter().map(|i| (*i, scope.created)).collect();
        let before = super::decisions::read_states(&mut tx, &order).await?;
        sqlx::query(
            "update results r set defect = t.d, updated_at = now() \
             from unnest($1::uuid[], $2::timestamptz[], $3::jsonb[]) as t(i, c, d) \
             where r.id = t.i and r.launch_created_at = t.c",
        )
        .bind(&upd)
        .bind(&created)
        .bind(&defects)
        .execute(&mut *tx)
        .await?;
        if let Some(m) = &rule.rule.action.mute {
            let until = m.days.map(|d| now + chrono::Duration::days(i64::from(d)));
            let mutes: BTreeMap<Uuid, (Option<DateTime<Utc>>, String)> = series
                .iter()
                .map(|s| (*s, (until, format!("rule {}", rule.rule.name))))
                .collect();
            add_retro_mutes(&mut tx, scope.project_id, &mutes, by).await?;
            sqlx::query(
                "update results r set muted = true, \
                   mute_until = (select m.until from series_mutes m where m.series_id = r.series_id), updated_at = now() \
                 from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                 where r.id = t.i and r.launch_created_at = t.c",
            )
            .bind(&upd)
            .bind(&created)
            .execute(&mut *tx)
            .await?;
            recount_muted(&mut tx, &[scope.launch_id]).await?;
        }
        let after = super::decisions::read_states(&mut tx, &order).await?;
        super::decisions::record(
            &mut tx,
            &super::decisions::Batch {
                project_id: scope.project_id,
                project_key: &scope.project_key,
                via: super::types::DecisionVia::Retro,
                source: super::types::DecisionSource::Rule,
                by,
                suggestion_id: None,
                reverts: None,
            },
            &before,
            &after,
        )
        .await?;
        crate::events::notify_launch(&mut tx, &scope.project_key, scope.launch_id).await?;
        tx.commit().await?;
        Ok(upd.len() as u64)
    }

    /// Step `log_rules` of the analysis of a launch: evaluates the project's enabled
    /// after-launch rules (`rules`, at most `max_log_rules` of them) on the launch's logs
    /// within `budget` of matching time, writes pattern marks and applies the other effects
    /// as retro batches. `partial` when a budget ran out (`detail.reason`: `log_cpu`, `regex`
    /// or `attempts`).
    pub async fn evaluate_log_rules(
        &self,
        ctx: &LaunchCtx<'_>,
        rules: &[Arc<CompiledRule>],
        budget: Duration,
    ) -> anyhow::Result<StepOutcome> {
        let rules: Vec<Arc<CompiledRule>> = rules
            .iter()
            .filter(|r| r.after_launch() && r.rule.readable())
            .take(ctx.limits.max_log_rules as usize)
            .cloned()
            .collect();
        if rules.is_empty() {
            return Ok(StepOutcome::skipped("no log rules"));
        }
        let Some(scope) = self.scope(ctx.project_id, ctx.launch.id).await? else {
            return Ok(StepOutcome::skipped("the launch is gone"));
        };
        let ev = self
            .evaluate_logs(&scope, &rules, ctx.limits, budget)
            .await?;
        let writes = self.write_evaluation(&scope, &rules, &ev, None).await?;
        Ok(outcome(&ev, rules.len(), writes))
    }

    /// The `log_rule.apply` task on one launch: evaluates one after-launch rule within
    /// `budget` and applies it as `by` (the person who queued the task).
    pub async fn apply_log_rule_to_launch(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        rule: &Arc<CompiledRule>,
        limits: &AnalysisLimits,
        budget: Duration,
        by: &str,
    ) -> anyhow::Result<RetroLaunch> {
        let Some(scope) = self.scope(project_id, launch_id).await? else {
            return Ok(RetroLaunch::default());
        };
        let rules = [rule.clone()];
        let ev = self.evaluate_logs(&scope, &rules, limits, budget).await?;
        let writes = self.write_evaluation(&scope, &rules, &ev, Some(by)).await?;
        Ok(RetroLaunch {
            matched: ev.hits.len() as u64,
            writes,
            partial: !ev.complete(),
        })
    }

    /// Applies one after-launch rule to log lines another backend found (ClickHouse retro):
    /// each match is resolved to its result in the project, the rule's result conditions and
    /// signals are checked here, and the marks and effects are written as by the task (`by`).
    /// Matches of launches outside the project or results that are gone are ignored.
    pub async fn apply_log_matches(
        &self,
        project_id: Uuid,
        rule: &Arc<CompiledRule>,
        matches: &[LogMatch],
        by: &str,
    ) -> anyhow::Result<RetroLaunch> {
        let mut by_launch: BTreeMap<Uuid, Vec<&LogMatch>> = BTreeMap::new();
        for m in matches {
            by_launch.entry(m.launch_id).or_default().push(m);
        }
        let rules = [rule.clone()];
        let mut total = RetroLaunch::default();
        let mut budget = Budget::launch();
        for (launch_id, ms) in by_launch {
            let Some(scope) = self.scope(project_id, launch_id).await? else {
                continue;
            };
            let keys: Vec<String> = ms
                .iter()
                .map(|m| format!("r:{}", no_nul(&m.result_key)))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let cut = MATCH_INPUT_BYTES as i32;
            let rows: Vec<AttemptRow> = sqlx::query_as(
                "select a.id, a.result_id, a.client_key, 0 as attempt_no, false as has_logs, \
                   left(a.trace, $4) as trace, r.status, r.test_case_id, left(r.message, $4) as message, r.fingerprint \
                 from attempts a join results r on r.id = a.result_id and r.launch_created_at = a.launch_created_at \
                 where a.launch_id = $1 and a.launch_created_at = $2 and a.client_key = any($3)",
            )
            .bind(launch_id)
            .bind(scope.created)
            .bind(&keys)
            .bind(cut)
            .fetch_all(&self.pool)
            .await?;
            let signals = if rule.rule.matcher.signals.is_some() {
                self.launch_signals(&scope).await?
            } else {
                HashMap::new()
            };
            let by_key: HashMap<&str, &AttemptRow> =
                rows.iter().map(|r| (r.client_key.as_str(), r)).collect();
            let mut ev = Evaluation::default();
            for m in ms {
                let key = format!("r:{}", m.result_key);
                let Some(row) = by_key.get(key.as_str()) else {
                    continue;
                };
                if ev.hits.contains_key(&row.result_id) {
                    continue;
                }
                match candidates(
                    &rules,
                    row,
                    scope.env.as_deref(),
                    &signals,
                    None,
                    &mut budget,
                ) {
                    Err(BudgetExhausted) => {
                        ev.stopped = Some(Stop::Regex);
                        break;
                    }
                    Ok(v) if v.is_empty() => {}
                    Ok(_) => {
                        ev.hits.entry(row.result_id).or_default().insert(
                            0,
                            Hit {
                                attempt: i32::try_from(m.attempt).unwrap_or(i32::MAX),
                                seq: i64::try_from(m.seq).ok(),
                                excerpt: excerpt(&m.excerpt, None),
                            },
                        );
                    }
                }
            }
            // Only marks are added: the other backend's pass says nothing about results it
            // did not return, so earlier marks stay.
            ev.attempts_truncated = true;
            let writes = self.write_evaluation(&scope, &rules, &ev, Some(by)).await?;
            total.matched += ev.hits.len() as u64;
            total.writes.marked += writes.marked;
            total.writes.updated += writes.updated;
            if ev.stopped.is_some() {
                total.partial = true;
                break;
            }
        }
        Ok(total)
    }

    /// Dry run of a rule on a launch: for an after-launch rule, the results whose logs it
    /// matches (nothing is written); `None` when the launch is not in the project.
    pub async fn log_rule_dry_run(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        rule: Rule,
        limits: &AnalysisLimits,
    ) -> anyhow::Result<Option<Result<DryRun, String>>> {
        let compiled = match CompiledRule::compile(rule) {
            Ok(c) => Arc::new(c),
            Err(e) => return Ok(Some(Err(e))),
        };
        let Some(scope) = self.scope(project_id, launch_id).await? else {
            return Ok(None);
        };
        let ev = self
            .evaluate_logs(
                &scope,
                &[compiled],
                limits,
                Duration::from_millis(limits.log_cpu_ms),
            )
            .await?;
        let truncated = !ev.complete() || ev.hits.len() > MAX_DRY_RUN_IDS;
        Ok(Some(Ok(DryRun {
            matches: ev.hits.keys().take(MAX_DRY_RUN_IDS).copied().collect(),
            evaluated: ev.results,
            truncated,
        })))
    }

    /// Queued or running tasks of a kind in a project.
    pub async fn open_tasks_of_kind(&self, project_id: Uuid, kind: &str) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "select count(*) from analysis_tasks where project_id = $1 and kind = $2 \
             and status in ('queued', 'running')",
        )
        .bind(project_id)
        .bind(kind)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Pattern marks of a result.
    pub async fn result_patterns(&self, result_id: Uuid) -> anyhow::Result<Vec<ResultPattern>> {
        let rows: Vec<(String, Uuid, i32, String)> = sqlx::query_as(
            "select pattern, rule_id, attempt, excerpt from result_patterns where result_id = $1 \
             order by pattern limit $2",
        )
        .bind(result_id)
        .bind(MAX_RESULT_PATTERNS)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(pattern, rule_id, attempt, excerpt)| ResultPattern {
                pattern,
                rule_id,
                attempt,
                excerpt,
            })
            .collect())
    }

    /// The patterns of a project marked since a time, most marked results first: results,
    /// launches and the newest launch's time of each.
    pub async fn pattern_counts(
        &self,
        project_id: Uuid,
        since: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<Vec<PatternCount>> {
        let rows: Vec<(String, i64, i64, DateTime<Utc>)> = sqlx::query_as(
            "select pattern, count(*)::bigint, count(distinct launch_id)::bigint, max(launch_created_at) \
             from result_patterns where project_id = $1 and created_at >= $2 \
             group by pattern order by count(*) desc, max(launch_created_at) desc, pattern limit $3",
        )
        .bind(project_id)
        .bind(since)
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(pattern, results, launches, last_seen_at)| PatternCount {
                pattern,
                results,
                launches,
                last_seen_at,
            })
            .collect())
    }
}

fn has_effects(r: &Rule) -> bool {
    let a = &r.action;
    a.defect_type_id.is_some()
        || a.comment.is_some()
        || a.issue.is_some()
        || a.mute.is_some()
        || a.accept.is_some()
}

fn outcome(ev: &Evaluation, rules: usize, writes: LogRuleWrites) -> StepOutcome {
    let mut detail = serde_json::json!({
        "rules": rules,
        "attempts": ev.attempts,
        "results": ev.results,
        "matched": ev.hits.len(),
        "marked": writes.marked,
        "updated": writes.updated,
        "cpuMs": ev.cpu.as_millis() as u64,
    });
    if ev.skipped_large > 0 {
        detail["skippedLarge"] = ev.skipped_large.into();
    }
    let reason = match ev.stopped {
        Some(Stop::Cpu) => Some("log_cpu"),
        Some(Stop::Regex) => Some("regex"),
        None if ev.attempts_truncated => Some("attempts"),
        None => None,
    };
    match reason {
        Some(r) => {
            if r != "attempts" {
                metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => r)
                    .increment(1);
            }
            detail["reason"] = r.into();
            StepOutcome::partial(detail)
        }
        None => StepOutcome::done(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::rules::RuleMatch;
    use crate::analysis::types::{LogStreamName, RuleLogMatch};

    fn compiled(log: RuleLogMatch) -> CompiledRule {
        CompiledRule::compile(Rule {
            id: Uuid::now_v7(),
            name: "r".into(),
            enabled: true,
            order: 0,
            matcher: RuleMatch {
                log: Some(log),
                ..Default::default()
            },
            action: rules::RuleAction {
                pattern: Some("p".into()),
                ..Default::default()
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            hits: 0,
            phase: Default::default(),
            note: None,
        })
        .unwrap()
    }

    fn line(seq: i64, level: LogLevel, stream: LogStream, text: &str) -> TailLine {
        TailLine {
            seq: Some(seq),
            attempt: Some(0),
            level: Some(level),
            stream,
            text: text.into(),
        }
    }

    fn tail(lines: Vec<TailLine>, max: usize) -> Tail {
        let mut t = Tail::default();
        for l in lines.into_iter().rev() {
            if !t.push_older(l, 0, max) {
                break;
            }
        }
        t
    }

    #[test]
    fn levels_streams_and_last_bytes() {
        let lines = vec![
            line(0, LogLevel::Error, LogStream::Stderr, "FATAL disk full"),
            line(
                1,
                LogLevel::Info,
                LogStream::Stdout,
                "x".repeat(2000).as_str(),
            ),
            line(2, LogLevel::Info, LogStream::Stdout, "retrying"),
        ];
        let t = tail(lines, 1 << 20);
        assert_eq!(t.lines[0].seq, Some(2), "newest first");
        let mut b = Budget::launch();
        let r = compiled(RuleLogMatch {
            contains: Some("disk FULL".into()),
            ..Default::default()
        });
        let log = r.log().unwrap();
        assert_eq!(scan_tail(log, &t, 1 << 20, &mut b).unwrap().unwrap().0, 2);
        // The line is older than the last 1024 bytes.
        assert_eq!(scan_tail(log, &t, 1024, &mut b).unwrap(), None);
        let r = compiled(RuleLogMatch {
            regex: Some("disk full".into()),
            levels: Some(vec![LogLevel::Warn]),
            ..Default::default()
        });
        assert_eq!(
            scan_tail(r.log().unwrap(), &t, 1 << 20, &mut b).unwrap(),
            None
        );
        let r = compiled(RuleLogMatch {
            regex: Some("disk full".into()),
            streams: Some(vec![LogStreamName::Stdout]),
            ..Default::default()
        });
        assert_eq!(
            scan_tail(r.log().unwrap(), &t, 1 << 20, &mut b).unwrap(),
            None
        );
        let r = compiled(RuleLogMatch {
            regex: Some("disk full".into()),
            levels: Some(vec![LogLevel::Error]),
            streams: Some(vec![LogStreamName::Stderr]),
            ..Default::default()
        });
        assert!(
            scan_tail(r.log().unwrap(), &t, 1 << 20, &mut b)
                .unwrap()
                .is_some()
        );
        // A tail built for fewer bytes cuts the oldest line to its last bytes.
        let t = tail(
            vec![
                line(0, LogLevel::Info, LogStream::Stdout, "abcdefghij"),
                line(1, LogLevel::Info, LogStream::Stdout, "klm"),
            ],
            7,
        );
        assert_eq!(t.bytes, 7);
        // "klm" and its line end take 4 bytes, the older line keeps 2 and its end.
        assert_eq!(t.lines[1].text, "ij");
    }

    #[test]
    fn captured_output_has_no_levels() {
        let t = Tail::of_captured("one\ntwo\r\nOutOfMemoryError\n", LogStream::Stdout, 1 << 20);
        assert_eq!(t.lines[0].text, "OutOfMemoryError");
        assert_eq!(t.lines[1].text, "two");
        let mut b = Budget::launch();
        let any = compiled(RuleLogMatch {
            contains: Some("outofmemory".into()),
            ..Default::default()
        });
        assert!(
            scan_tail(any.log().unwrap(), &t, 1 << 20, &mut b)
                .unwrap()
                .is_some()
        );
        let leveled = compiled(RuleLogMatch {
            contains: Some("outofmemory".into()),
            levels: Some(vec![LogLevel::Error]),
            ..Default::default()
        });
        assert!(
            scan_tail(leveled.log().unwrap(), &t, 1 << 20, &mut b)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn redos_shaped_patterns_stay_linear_on_log_tails() {
        let big = format!("{}!", "a".repeat(MATCH_INPUT_BYTES * 4));
        let lines: Vec<TailLine> = (0..64)
            .map(|i| line(i, LogLevel::Error, LogStream::Stdout, &big))
            .collect();
        let t = tail(lines, 1 << 20);
        for p in [
            r"(a+)+$",
            r"(a|aa)+$",
            r"(.*a){20}$",
            r"^(\w+\s?)*$",
            r"(a*)*b",
        ] {
            let r = compiled(RuleLogMatch {
                regex: Some(p.into()),
                ..Default::default()
            });
            let started = Instant::now();
            let mut b = Budget::launch();
            let _ = scan_tail(r.log().unwrap(), &t, 1 << 20, &mut b);
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{p} took {:?}",
                started.elapsed()
            );
            // Every line is cut to the match input size.
            assert!(rules::LAUNCH_SCAN_BYTES - b.bytes <= 64 * MATCH_INPUT_BYTES as u64 * 2);
        }
        let r = compiled(RuleLogMatch {
            regex: Some("never".into()),
            ..Default::default()
        });
        let mut b = Budget::new(u64::MAX, 3 * MATCH_INPUT_BYTES as u64);
        assert!(scan_tail(r.log().unwrap(), &t, 1 << 20, &mut b).is_err());
        assert!(b.exhausted);
    }

    #[test]
    fn excerpts_are_short_clean_and_around_the_match() {
        assert_eq!(
            excerpt("\u{1b}[31mERROR\u{1b}[0m\tboom\u{7}", None),
            "ERROR boom"
        );
        let long = format!("{}needle{}", "x".repeat(1000), "y".repeat(1000));
        let at = long.find("needle");
        let e = excerpt(&long, at);
        assert_eq!(e.chars().count(), EXCERPT_CHARS);
        assert!(e.contains("needle"), "{e}");
        let e = excerpt(&"é".repeat(1000), None);
        assert_eq!(e.chars().count(), EXCERPT_CHARS);
    }

    fn zstd_json(v: &serde_json::Value) -> Vec<u8> {
        zstd::encode_all(serde_json::to_vec(v).unwrap().as_slice(), 3).unwrap()
    }

    #[test]
    fn empty_lines_do_not_grow_a_tail_without_bound() {
        // Each line costs a byte at least: 50 chunks of 1000 empty lines fill a 1024-byte tail
        // with 1024 lines, not 50 000.
        let chunk: Vec<serde_json::Value> = (0..1000)
            .map(|q| serde_json::json!({"q": q, "l": "info", "s": "stdout", "m": ""}))
            .collect();
        let z = zstd_json(&serde_json::Value::Array(chunk));
        let chunks: Vec<(i32, Vec<u8>)> = (0..50).map(|_| (0, z.clone())).collect();
        let tails = build_tail(&chunks, None, 1024).unwrap();
        assert_eq!(tails[0].lines.len(), 1024);
        assert_eq!(tails[0].bytes, 1024);
        // Captured output of empty lines is capped by lines too.
        let t = Tail::of_captured(&"x\n".repeat(1 << 20), LogStream::Stdout, 1 << 20);
        assert!(t.lines.len() <= MAX_TAIL_LINES, "{}", t.lines.len());
        assert!(t.bytes <= 1 << 20);
    }

    #[test]
    fn the_cpu_budget_holds_inside_one_attempt() {
        // One attempt read by many rules: the pass stops at its time even though the time is
        // checked between attempts too.
        let text = format!("{}\n", "abcdefghij ".repeat(1400)).repeat(64);
        let captured = zstd_json(&serde_json::json!({"stdout": {"text": text}}));
        let rules: Vec<Arc<CompiledRule>> = (0..10)
            .map(|i| {
                Arc::new(compiled(RuleLogMatch {
                    regex: Some(format!("[a-z]+ never{i}$")),
                    last_bytes: Some(1 << 20),
                    ..Default::default()
                }))
            })
            .collect();
        let row = AttemptRow {
            id: Uuid::now_v7(),
            result_id: Uuid::now_v7(),
            client_key: "x".into(),
            attempt_no: 0,
            has_logs: true,
            trace: None,
            status: "failed".into(),
            test_case_id: Uuid::now_v7(),
            message: None,
            fingerprint: None,
        };
        let work = || {
            vec![AttemptWork {
                row: row.clone(),
                rules: (0..rules.len()).collect(),
                chunks: Vec::new(),
                captured: Some(captured.clone()),
            }]
        };
        let mut b = Budget::launch();
        let out = match_batch(&rules, work(), 1 << 20, &mut b, Duration::from_millis(1));
        assert_eq!(out.stopped, Some(Stop::Cpu));
        assert!(out.hits.is_empty());
        let mut b = Budget::launch();
        let out = match_batch(&rules, work(), 1 << 20, &mut b, Duration::from_secs(600));
        assert_eq!(
            out.stopped, None,
            "with time enough every rule reads the tail"
        );
    }

    #[test]
    fn contains_is_a_linear_case_insensitive_search() {
        let needle = format!("{}b", "a".repeat(rules::MAX_CONTAINS_BYTES - 1));
        let r = compiled(RuleLogMatch {
            contains: Some(needle.to_uppercase()),
            ..Default::default()
        });
        let log = r.log().unwrap();
        let line = "a".repeat(MATCH_INPUT_BYTES);
        let mut b = Budget::launch();
        let started = Instant::now();
        for _ in 0..256 {
            assert_eq!(log.line_matches(&line, &mut b), Some(false));
        }
        // Comparing the text at every offset would take 256 × 16 KiB × 1 KiB steps.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let hit = format!("xx{}", needle.to_lowercase());
        assert_eq!(log.line_matches(&hit, &mut b), Some(true));
        assert_eq!(log.find(&hit), Some(2));
        let r = compiled(RuleLogMatch {
            contains: Some("Größe (x)".into()),
            ..Default::default()
        });
        let log = r.log().unwrap();
        assert_eq!(log.line_matches("die GRÖSSE (x)", &mut b), Some(false));
        assert_eq!(log.line_matches("die GRÖßE (X) ist", &mut b), Some(true));
        assert_eq!(log.find("die GRÖßE (X) ist"), Some(4));
        assert_eq!(log.line_matches("Größe x", &mut b), Some(false));
    }

    #[test]
    fn excerpts_drop_bidi_controls_and_invisible_characters() {
        let e = excerpt(
            "user\u{202e}gnp.exe\u{202c} ok\u{200b}\u{2066}x\u{2069}\u{feff}\u{2028}end",
            None,
        );
        assert_eq!(e, "usergnp.exe okx end");
        assert!(e.chars().all(|c| !invisible(c) && !c.is_control()));
    }

    #[test]
    fn signal_conditions() {
        let mut r = compiled(RuleLogMatch {
            contains: Some("x".into()),
            ..Default::default()
        });
        r.rule.matcher.signals = Some(vec![crate::analysis::types::RuleSignalMatch {
            source: SignalSource::Grid,
            kind: Some("node-lost".into()),
        }]);
        assert!(!signals_hold(&r, None));
        assert!(!signals_hold(
            &r,
            Some(&vec![("grid".into(), "other".into())])
        ));
        assert!(!signals_hold(
            &r,
            Some(&vec![("k8s".into(), "node-lost".into())])
        ));
        assert!(signals_hold(
            &r,
            Some(&vec![("grid".into(), "node-lost".into())])
        ));
    }
}
