//! The backtest harness (contract §7.3): replays people's triage decisions in time order and
//! asks, for each one, what the memory folded from the decisions before it would have
//! suggested, with the same scoring and bands as the live path ([`crate::score`]). The report
//! is the precision per band (did the rank-1 suggestion name the type the person chose) and
//! the coverage. It is deterministic and does not depend on how the events are paged: the same
//! events give the same report on the server, offline from an export, and on synthetic data.
//!
//! Rule and signal candidates are outside the backtest: they are not learned from decisions.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{
    Candidate, ClusterFacts, DecisionEvent, MEMORY_BRANCHES, MEMORY_ENV_KEYS, MEMORY_TEST_CASES,
    MemoryRow, PRECISION_MIN_DECISIONS, SuggestionBand,
};
use crate::score::{MIN_MESSAGE_SIMILARITY, RankOptions, rank, score};

/// Confusions listed in a report, at most.
pub const MAX_CONFUSIONS: usize = 10;
/// Bytes the memory of one backtest may hold, about (messages, trigram keys, frames); the
/// events after it is full only mark the report `truncated`.
pub const MAX_MEMORY_BYTES: usize = 512 << 20;
/// Bytes of the similar-rows cache, about; it is dropped beyond (the answers do not change).
const MAX_CACHE_BYTES: usize = 128 << 20;

/// Options of a backtest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BacktestOptions {
    pub rank: RankOptions,
    /// Events replayed, at most (`FASTPROOF_ANALYSIS_BACKTEST_MAX_EVENTS`); later ones only
    /// mark the report `truncated`.
    pub max_events: u64,
}

impl Default for BacktestOptions {
    fn default() -> Self {
        Self {
            rank: RankOptions::default(),
            max_events: 200_000,
        }
    }
}

/// Precision of one band.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BandStats {
    pub band: SuggestionBand,
    /// Decisions whose rank-1 suggestion was in this band.
    pub suggested: u64,
    /// ... and named the type the person chose.
    pub correct: u64,
    /// ... and named a type of the group the person chose.
    pub correct_group: u64,
    /// `correct / suggested` (3 decimals), `None` without suggestions.
    pub precision: Option<f64>,
    /// `suggested` reached [`PRECISION_MIN_DECISIONS`] (the UI shows the percentage).
    pub shown: bool,
}

/// A frequent mistake: the group people chose and the group suggested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confusion {
    pub expected: String,
    pub suggested: String,
    pub count: u64,
}

/// What a backtest found (the store adds id, source and author to make a `BacktestReport`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacktestSummary {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub decisions: u64,
    /// Share of decisions that had a suggestion (3 decimals).
    pub coverage: f64,
    pub no_suggestion: u64,
    /// `same`, `high`, `low`, always all three.
    pub bands: Vec<BandStats>,
    pub confusions: Vec<Confusion>,
    pub truncated: bool,
}

/// A trigram of a normalized message packed into an integer: exactly the trigrams of
/// [`crate::trigram::similarity`], so the prefilter below never drops a candidate it keeps.
fn trigram_keys(s: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for word in s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let chars: Vec<char> = "  "
            .chars()
            .chain(word.chars())
            .chain(std::iter::once(' '))
            .collect();
        for w in chars.windows(3) {
            out.push(((w[0] as u64) << 42) | ((w[1] as u64) << 21) | (w[2] as u64));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn shared(a: &[u64], b: &[u64]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

fn similar(a: &[u64], b: &[u64]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let s = shared(a, b);
    s as f64 / (a.len() + b.len() - s) as f64 >= MIN_MESSAGE_SIMILARITY - 1e-9
}

/// Rows whose message is similar to one message: computed once, then extended with the rows
/// added or changed since.
struct SimilarCache {
    keys: Vec<u64>,
    /// Rows checked: indexes below this, plus the version of each changed row.
    checked: usize,
    version: u64,
    rows: Vec<usize>,
}

/// The triage memory of a backtest, in memory: the fold of §7.2 over decision events.
#[derive(Default)]
pub struct Memory {
    rows: Vec<MemoryRow>,
    keys: Vec<Vec<u64>>,
    by_key: BTreeMap<(String, Uuid), usize>,
    by_fingerprint: HashMap<String, Vec<usize>>,
    /// Rows whose message changed, with the version they changed at.
    changed: Vec<(u64, usize)>,
    version: u64,
    similar: HashMap<String, SimilarCache>,
    /// Bytes of the rows and their keys, about.
    bytes: usize,
    /// Bytes of `similar`, about.
    cache_bytes: usize,
}

fn row_bytes(row: &MemoryRow, keys: &[u64]) -> usize {
    256 + row.fingerprint.len()
        + row.message_norm.len()
        + keys.len() * 8
        + row.frames.iter().map(|f| f.len() + 24).sum::<usize>()
        + row.last_decided_by.len()
}

fn push_front_capped<T: PartialEq + Clone>(list: &mut Vec<T>, v: T, cap: usize) {
    list.retain(|x| *x != v);
    list.insert(0, v);
    list.truncate(cap);
}

impl Memory {
    /// The rows folded so far, in the order they were first decided.
    pub fn rows(&self) -> &[MemoryRow] {
        &self.rows
    }

    /// Bytes the rows hold, about.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Folds one decision (as the live fold does, through [`new_row`] and [`fold_row`]): the
    /// row of (fingerprint, type) gains a confirmation, the newest test case, environment and
    /// branch, the message and frames and the author; the row of the previous type, when the
    /// decision replaced one, an override.
    pub fn fold(&mut self, ev: &DecisionEvent) {
        let key = (ev.fingerprint.clone(), ev.defect_type_id);
        match self.by_key.get(&key) {
            Some(&idx) => {
                let old = row_bytes(&self.rows[idx], &self.keys[idx]);
                let before = self.rows[idx].message_norm.clone();
                fold_row(&mut self.rows[idx], ev);
                if self.rows[idx].message_norm != before {
                    self.keys[idx] = trigram_keys(&self.rows[idx].message_norm);
                    self.version += 1;
                    self.changed.push((self.version, idx));
                }
                self.bytes =
                    self.bytes.saturating_sub(old) + row_bytes(&self.rows[idx], &self.keys[idx]);
            }
            None => {
                let i = self.rows.len();
                let row = new_row(ev);
                let keys = trigram_keys(&row.message_norm);
                self.bytes += row_bytes(&row, &keys);
                self.keys.push(keys);
                self.rows.push(row);
                self.by_key.insert(key, i);
                self.by_fingerprint
                    .entry(ev.fingerprint.clone())
                    .or_default()
                    .push(i);
            }
        }
        if let Some(prev) = ev.previous_type_id
            && prev != ev.defect_type_id
            && let Some(i) = self.by_key.get(&(ev.fingerprint.clone(), prev))
        {
            self.rows[*i].overridden += 1;
        }
    }

    /// Rows that may be candidates for a cluster: the same fingerprint, and rows whose
    /// message is at least 40% similar by trigrams.
    pub fn candidates(&mut self, cluster: &ClusterFacts) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .by_fingerprint
            .get(&cluster.fingerprint)
            .cloned()
            .unwrap_or_default();
        if !cluster.message_norm.is_empty() {
            let n = self.rows.len();
            let version = self.version;
            if self.cache_bytes > MAX_CACHE_BYTES {
                self.similar.clear();
                self.cache_bytes = 0;
            }
            let cache_bytes = &mut self.cache_bytes;
            let entry = self
                .similar
                .entry(cluster.message_norm.clone())
                .or_insert_with(|| {
                    let keys = trigram_keys(&cluster.message_norm);
                    *cache_bytes += 128 + cluster.message_norm.len() + keys.len() * 8;
                    SimilarCache {
                        keys,
                        checked: 0,
                        version,
                        rows: Vec::new(),
                    }
                });
            for (i, k) in self.keys.iter().enumerate().take(n).skip(entry.checked) {
                if similar(&entry.keys, k) {
                    entry.rows.push(i);
                    *cache_bytes += 8;
                }
            }
            entry.checked = n;
            if entry.version < self.version {
                for &(v, i) in self.changed.iter().rev() {
                    if v <= entry.version {
                        break;
                    }
                    if i < n {
                        entry.rows.retain(|r| *r != i);
                        if similar(&entry.keys, &self.keys[i]) {
                            entry.rows.push(i);
                        }
                    }
                }
                entry.version = self.version;
            }
            out.extend(entry.rows.iter().copied());
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The ranked suggestions of the memory for a cluster at a time.
    pub fn suggest(
        &mut self,
        cluster: &ClusterFacts,
        now: DateTime<Utc>,
        opts: &RankOptions,
    ) -> Vec<Candidate> {
        let idx = self.candidates(cluster);
        let cands: Vec<Candidate> = idx
            .into_iter()
            .filter_map(|i| score(cluster, &self.rows[i], now))
            .collect();
        rank(cands, opts, &[])
    }
}

/// A new memory row from a person's decision (one confirmation).
pub fn new_row(ev: &DecisionEvent) -> MemoryRow {
    let mut row = MemoryRow {
        fingerprint: ev.fingerprint.clone(),
        defect_type_id: ev.defect_type_id,
        group: ev.group.clone(),
        message_norm: ev.message_norm.clone(),
        frames: ev.frames.clone(),
        test_case_ids: Vec::new(),
        env_keys: Vec::new(),
        branches: Vec::new(),
        comment: None,
        issue_url: None,
        issue_key: None,
        sample_result_id: None,
        sample_launch_id: None,
        confirmed: 0,
        overridden: 0,
        first_decided_at: ev.at,
        last_decided_at: ev.at,
        last_decided_by: ev.by.clone(),
    };
    fold_row(&mut row, ev);
    row
}

/// One more person's decision on a memory row of the same fingerprint and type: a
/// confirmation, the newest test case, environment and branch first (capped), the latest
/// message and frames when given, the author of the newest decision.
pub fn fold_row(row: &mut MemoryRow, ev: &DecisionEvent) {
    row.confirmed += 1;
    if ev.at >= row.last_decided_at {
        row.last_decided_at = ev.at;
        row.last_decided_by = ev.by.clone();
    }
    row.first_decided_at = row.first_decided_at.min(ev.at);
    row.group = ev.group.clone();
    if !ev.frames.is_empty() {
        row.frames = ev.frames.clone();
    }
    if !ev.message_norm.is_empty() {
        row.message_norm = ev.message_norm.clone();
    }
    if let Some(t) = ev.test_case_id {
        push_front_capped(&mut row.test_case_ids, t, MEMORY_TEST_CASES);
    }
    if let Some(e) = &ev.env_key {
        push_front_capped(&mut row.env_keys, e.clone(), MEMORY_ENV_KEYS);
    }
    if let Some(b) = &ev.branch {
        push_front_capped(&mut row.branches, b.clone(), MEMORY_BRANCHES);
    }
}

/// The trigram keys of a message, for [`similar_keys`].
pub fn message_keys(s: &str) -> Vec<u64> {
    trigram_keys(s)
}

/// Whether two messages (as [`message_keys`]) are at least 40% similar by trigrams, exactly
/// as [`crate::trigram::similarity`] measures it.
pub fn similar_keys(a: &[u64], b: &[u64]) -> bool {
    similar(a, b)
}

/// The cluster a decision event stands for.
pub fn cluster_of(ev: &DecisionEvent) -> ClusterFacts {
    ClusterFacts {
        fingerprint: ev.fingerprint.clone(),
        aliases: Vec::new(),
        message_norm: ev.message_norm.clone(),
        frames: ev.frames.clone(),
        test_case_ids: ev.test_case_id.into_iter().collect(),
        env_key: ev.env_key.clone(),
        branch: ev.branch.clone(),
        results: ev.results,
        sample_result_id: None,
    }
}

#[derive(Clone, Copy, Default)]
struct Tally {
    suggested: u64,
    correct: u64,
    correct_group: u64,
}

/// The harness: [`Backtest::feed`] pages of events in time order, then [`Backtest::finish`].
pub struct Backtest {
    opts: BacktestOptions,
    memory: Memory,
    decisions: u64,
    no_suggestion: u64,
    tallies: [Tally; 3],
    confusions: BTreeMap<(String, String), u64>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    truncated: bool,
}

fn band_index(b: SuggestionBand) -> usize {
    match b {
        SuggestionBand::Same => 0,
        SuggestionBand::High => 1,
        SuggestionBand::Low => 2,
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

impl Backtest {
    pub fn new(opts: &BacktestOptions) -> Self {
        Self {
            opts: *opts,
            memory: Memory::default(),
            decisions: 0,
            no_suggestion: 0,
            tallies: [Tally::default(); 3],
            confusions: BTreeMap::new(),
            from: None,
            to: None,
            truncated: false,
        }
    }

    /// Replays a page of events (in time order; pages of any size give the same report).
    pub fn feed(&mut self, events: &[DecisionEvent]) {
        for ev in events {
            self.step(ev);
        }
    }

    /// [`Backtest::feed`] until `deadline`: the events left when it passes are not replayed
    /// and the report is `truncated`. Returns whether the whole page was replayed.
    pub fn feed_until(&mut self, events: &[DecisionEvent], deadline: std::time::Instant) -> bool {
        for ev in events {
            if std::time::Instant::now() >= deadline {
                self.truncated = true;
                return false;
            }
            self.step(ev);
        }
        true
    }

    /// Whether the report is truncated so far (event cap, memory cap or deadline).
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Replays one event: what the memory so far suggests (the rank-1 candidate, returned),
    /// counted against the decision, then the decision folded. `None` also when the event is
    /// beyond `max_events` (it is then only counted as truncation).
    pub fn step(&mut self, ev: &DecisionEvent) -> Option<Candidate> {
        if self.decisions >= self.opts.max_events || self.memory.bytes() > MAX_MEMORY_BYTES {
            self.truncated = true;
            return None;
        }
        self.decisions += 1;
        self.from = Some(self.from.map_or(ev.at, |f| f.min(ev.at)));
        self.to = Some(self.to.map_or(ev.at, |t| t.max(ev.at)));
        let top = self
            .memory
            .suggest(&cluster_of(ev), ev.at, &self.opts.rank)
            .into_iter()
            .next();
        match &top {
            None => self.no_suggestion += 1,
            Some(c) => {
                let band = c.band.unwrap_or(SuggestionBand::Low);
                let t = &mut self.tallies[band_index(band)];
                t.suggested += 1;
                if c.defect_type_id == ev.defect_type_id {
                    t.correct += 1;
                }
                if c.group == ev.group {
                    t.correct_group += 1;
                } else {
                    *self
                        .confusions
                        .entry((ev.group.clone(), c.group.clone()))
                        .or_default() += 1;
                }
            }
        }
        self.memory.fold(ev);
        top
    }

    /// The memory folded so far.
    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub fn finish(self) -> BacktestSummary {
        let bands = [
            SuggestionBand::Same,
            SuggestionBand::High,
            SuggestionBand::Low,
        ]
        .into_iter()
        .map(|b| {
            let t = self.tallies[band_index(b)];
            BandStats {
                band: b,
                suggested: t.suggested,
                correct: t.correct,
                correct_group: t.correct_group,
                precision: (t.suggested > 0).then(|| round3(t.correct as f64 / t.suggested as f64)),
                shown: t.suggested >= PRECISION_MIN_DECISIONS as u64,
            }
        })
        .collect();
        let mut confusions: Vec<Confusion> = self
            .confusions
            .into_iter()
            .map(|((expected, suggested), count)| Confusion {
                expected,
                suggested,
                count,
            })
            .collect();
        confusions.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.expected.cmp(&b.expected))
                .then_with(|| a.suggested.cmp(&b.suggested))
        });
        confusions.truncate(MAX_CONFUSIONS);
        let covered = self.decisions - self.no_suggestion;
        BacktestSummary {
            from: self.from,
            to: self.to,
            decisions: self.decisions,
            coverage: if self.decisions == 0 {
                0.0
            } else {
                round3(covered as f64 / self.decisions as f64)
            },
            no_suggestion: self.no_suggestion,
            bands,
            confusions,
            truncated: self.truncated,
        }
    }
}

/// Runs a backtest over events in one call.
pub fn run(events: &[DecisionEvent], opts: &BacktestOptions) -> BacktestSummary {
    let mut b = Backtest::new(opts);
    b.feed(events);
    b.finish()
}

/// Reads NDJSON decision events (the export format), one per line; blank lines are skipped.
/// Lines longer than `max_line` bytes (found without reading them whole) or of another
/// format version are errors that name the line.
pub fn read_events(
    mut r: impl std::io::BufRead,
    max_line: usize,
    mut each: impl FnMut(DecisionEvent) -> crate::Result<()>,
) -> crate::Result<u64> {
    use std::io::BufRead as _;
    let mut n = 0;
    let mut i = 0usize;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let got = std::io::Read::take(&mut r, max_line as u64 + 2).read_until(b'\n', &mut buf)?;
        if got == 0 {
            break;
        }
        i += 1;
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        if buf.len() > max_line {
            anyhow::bail!("line {i}: longer than {max_line} bytes");
        }
        let line = std::str::from_utf8(&buf).map_err(|_| anyhow::anyhow!("line {i}: not UTF-8"))?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: DecisionEvent = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("line {i}: not a decision event: {e}"))?;
        if ev.v != 1 {
            anyhow::bail!("line {i}: format version {} is not 1", ev.v);
        }
        each(ev)?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minutes)
    }

    fn ev(n: i64, fp: &str, msg: &str, ty: u128, group: &str, test: u128) -> DecisionEvent {
        DecisionEvent {
            v: 1,
            at: at(n),
            fingerprint: fp.into(),
            message_norm: msg.into(),
            frames: vec![],
            test_case_id: Some(Uuid::from_u128(test)),
            env_key: Some("staging".into()),
            branch: Some("main".into()),
            defect_type_id: Uuid::from_u128(ty),
            group: group.into(),
            previous_type_id: None,
            by: "ann".into(),
            results: 1,
        }
    }

    fn events() -> Vec<DecisionEvent> {
        let mut out = Vec::new();
        for i in 0..40 {
            out.push(ev(
                i * 10,
                "aaaa",
                "expected total <n> but was <n>",
                1,
                "productBug",
                1,
            ));
            out.push(ev(
                i * 10 + 1,
                "bbbb",
                "connection refused to db port <n>",
                2,
                "systemIssue",
                (i % 5) as u128 + 10,
            ));
            if i % 7 == 0 {
                out.push(ev(
                    i * 10 + 2,
                    "cccc",
                    "connection refused to cache port <n>",
                    2,
                    "systemIssue",
                    20,
                ));
            }
            if i == 20 {
                out.push(ev(
                    i * 10 + 3,
                    "aaaa",
                    "expected total <n> but was <n>",
                    3,
                    "automationBug",
                    1,
                ));
            }
        }
        out
    }

    #[test]
    fn trigram_keys_match_the_similarity_measure() {
        for (a, b) in [
            (
                "connection refused to db port <n>",
                "connection refused to cache port <n>",
            ),
            ("word", "two words"),
            ("Ünïcode Straße", "unicode strasse"),
            ("", "x"),
        ] {
            let (x, y) = (trigram_keys(a), trigram_keys(b));
            let s = if x.is_empty() || y.is_empty() {
                0.0
            } else {
                let n = shared(&x, &y);
                n as f64 / (x.len() + y.len() - n) as f64
            };
            assert!(
                (s - crate::trigram::similarity(a, b)).abs() < 1e-12,
                "{a} / {b}"
            );
        }
    }

    #[test]
    fn the_report_is_deterministic_and_independent_of_paging() {
        let evs = events();
        let one = run(&evs, &BacktestOptions::default());
        assert_eq!(one, run(&evs, &BacktestOptions::default()));
        for page in [1, 2, 3, 7, 50, 1000] {
            let mut b = Backtest::new(&BacktestOptions::default());
            for chunk in evs.chunks(page) {
                b.feed(chunk);
            }
            assert_eq!(b.finish(), one, "page {page}");
        }
        assert_eq!(one.decisions, evs.len() as u64);
        // The first decision of each error has nothing to learn from; "cccc" is similar to
        // "bbbb" so even its first decision gets a suggestion.
        assert_eq!(one.no_suggestion, 2);
        let same = &one.bands[0];
        assert_eq!(same.band, SuggestionBand::Same);
        assert!(same.suggested > 50 && same.shown);
        assert!(same.precision.unwrap() > 0.95, "{one:?}");
        assert_eq!(one.bands.len(), 3);
        assert_eq!(one.from, Some(at(0)));
        // The one automationBug decision was suggested productBug: one confusion.
        assert_eq!(
            one.confusions[0],
            Confusion {
                expected: "automationBug".into(),
                suggested: "productBug".into(),
                count: 1
            }
        );
        assert!(!one.truncated);
        let cut = run(
            &evs,
            &BacktestOptions {
                max_events: 10,
                ..Default::default()
            },
        );
        assert!(cut.truncated);
        assert_eq!(cut.decisions, 10);
    }

    #[test]
    fn the_fold_counts_confirmations_overrides_and_keeps_newest_lists() {
        let mut m = Memory::default();
        for i in 0..25 {
            m.fold(&ev(i, "aaaa", "msg one two", 1, "productBug", i as u128));
        }
        let mut correction = ev(30, "aaaa", "msg one two", 2, "systemIssue", 99);
        correction.previous_type_id = Some(Uuid::from_u128(1));
        m.fold(&correction);
        let rows = m.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].confirmed, 25);
        assert_eq!(rows[0].overridden, 1);
        assert_eq!(rows[0].test_case_ids.len(), MEMORY_TEST_CASES);
        assert_eq!(rows[0].test_case_ids[0], Uuid::from_u128(24));
        assert_eq!(rows[1].confirmed, 1);
        assert_eq!(rows[1].overridden, 0);
        // Similar messages are found once and then kept up to date as rows are added.
        let c = cluster_of(&ev(40, "zzzz", "msg one two three", 1, "productBug", 1));
        assert_eq!(m.candidates(&c), vec![0, 1]);
        m.fold(&ev(
            41,
            "yyyy",
            "msg one two three four",
            1,
            "productBug",
            1,
        ));
        assert_eq!(m.candidates(&c), vec![0, 1, 2]);
        m.fold(&ev(
            42,
            "yyyy",
            "completely different words",
            1,
            "productBug",
            1,
        ));
        assert_eq!(m.candidates(&c), vec![0, 1]);
    }

    #[test]
    fn events_are_read_from_ndjson() {
        let evs = events();
        let text: String = evs
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n\n")
            .collect();
        let mut got = Vec::new();
        let n = read_events(text.as_bytes(), 64 * 1024, |e| {
            got.push(e);
            Ok(())
        })
        .unwrap();
        assert_eq!(n, evs.len() as u64);
        assert_eq!(got, evs);
        let bad = read_events("{\"v\":2}\n".as_bytes(), 1024, |_| Ok(()));
        assert!(bad.unwrap_err().to_string().contains("line 1"));
        let long = read_events("x".repeat(100).as_bytes(), 10, |_| Ok(()));
        assert!(long.unwrap_err().to_string().contains("longer"));
    }

    #[test]
    fn an_endless_line_is_refused_without_reading_it_whole() {
        let endless = std::io::BufReader::new(std::io::Read::take(std::io::repeat(b'x'), 1 << 40));
        let r = read_events(endless, 1024, |_| Ok(()));
        assert!(r.unwrap_err().to_string().contains("line 1: longer"));
        let crlf = format!("{}\r\n", serde_json::to_string(&events()[0]).unwrap());
        assert_eq!(read_events(crlf.as_bytes(), 4096, |_| Ok(())).unwrap(), 1);
    }

    #[test]
    fn a_backtest_stops_at_its_deadline_and_its_memory_cap() {
        let evs = events();
        let mut b = Backtest::new(&BacktestOptions::default());
        assert!(!b.feed_until(&evs, std::time::Instant::now()));
        assert!(b.truncated());
        let s = b.finish();
        assert!(s.truncated);
        assert_eq!(s.decisions, 0);
        let mut b = Backtest::new(&BacktestOptions::default());
        let far = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        assert!(b.feed_until(&evs, far));
        assert_eq!(b.finish(), run(&evs, &BacktestOptions::default()));
        // Every row costs its message, its trigram keys and its frames.
        let mut m = Memory::default();
        let long: String = (0..150).map(|i| format!("w{i} ")).collect();
        m.fold(&ev(0, "aaaa", &long, 1, "productBug", 1));
        assert!(m.bytes() > long.len() * 2, "{}", m.bytes());
        let one = m.bytes();
        m.fold(&ev(1, "aaaa", &long, 1, "productBug", 2));
        assert_eq!(
            m.bytes(),
            one,
            "a confirmation of the same row costs nothing more"
        );
        let mut b = Backtest::new(&BacktestOptions::default());
        b.memory.bytes = MAX_MEMORY_BYTES + 1;
        b.feed(&evs);
        let s = b.finish();
        assert!(s.truncated);
        assert_eq!(s.decisions, 0);
    }
}
