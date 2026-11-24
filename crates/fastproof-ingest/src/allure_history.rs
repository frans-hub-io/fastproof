//! Parsers of the history files of Allure reports (untrusted input).
//!
//! Allure 2 keeps `history/history.json` (per test, the last statuses newest first) and four
//! trend files, one entry per past report. Allure 3 appends one JSON line per report to a
//! history file, with every test of that report. Both become [`HistoryPoint`]s: a past
//! report with its tests, which the server stores as a launch.
//!
//! Every input is bounded by [`HistoryLimits`]: file sizes, documents are scanned before they
//! are parsed (depth, value count, string length), the outer structures are read by visitors
//! that stop at their caps, and the points kept in memory are counted.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::BufRead;

use fastproof_model::identity::md5_hex;
use fastproof_model::{
    HistoryPoint, HistoryResult, HistoryTotals, HistoryTrend, Status, split_series_key,
};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::error::IngestError;
use crate::json::{JsonBounds, scan_json};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// Oldest and newest timestamp (ms) a past report may carry: 2000-01-01 and 2100-01-01.
pub const MIN_TIMESTAMP_MS: i64 = 946_684_800_000;
pub const MAX_TIMESTAMP_MS: i64 = 4_102_444_800_000;
/// Longest duration (ms) kept: ten years.
const MAX_DURATION_MS: i64 = 315_360_000_000;
/// Warnings kept in a [`HistoryReport`].
const MAX_WARNINGS: usize = 20;
/// Spread of start times (ms) beyond which a position-aligned group of Allure 2 is suspect.
const MISALIGNED_SPREAD_MS: i64 = 3 * 24 * 3600 * 1000;

/// Limits of one history import. Every limit error names the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLimits {
    /// `history.json` of Allure 2.
    pub max_history_bytes: u64,
    /// Each Allure 2 trend file.
    pub max_trend_bytes: u64,
    /// One line of an Allure 3 history file.
    pub max_line_bytes: u64,
    /// An Allure 3 history file.
    pub max_total_bytes: u64,
    /// Past reports of one import.
    pub max_points: u64,
    /// Tests in `history.json`.
    pub max_series: u64,
    /// Past runs of one test in `history.json`.
    pub max_items_per_series: u64,
    /// Results of all past reports of one import.
    pub max_results: u64,
    /// Results of one past report.
    pub max_results_per_point: u64,
    /// Earlier attempts kept per result.
    pub max_retries: u64,
    /// Kept length of a test name.
    pub max_name_bytes: u64,
    /// Kept length of a failure message.
    pub max_message_bytes: u64,
    /// Categories and metrics kept per past report.
    pub max_trend_entries: u64,
    pub max_json_depth: u32,
    /// JSON values in one document or line.
    pub max_json_values: u64,
    /// One JSON string.
    pub max_string_bytes: u64,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        HistoryLimits {
            max_history_bytes: 32 * MIB,
            max_trend_bytes: 4 * MIB,
            max_line_bytes: 32 * MIB,
            max_total_bytes: 256 * MIB,
            max_points: 1000,
            max_series: 200_000,
            max_items_per_series: 1000,
            max_results: 500_000,
            max_results_per_point: 100_000,
            max_retries: 20,
            max_name_bytes: 4 * KIB,
            max_message_bytes: KIB,
            max_trend_entries: 50,
            max_json_depth: 32,
            max_json_values: 5_000_000,
            max_string_bytes: MIB,
        }
    }
}

impl HistoryLimits {
    fn bounds(&self) -> JsonBounds {
        JsonBounds {
            max_depth: self.max_json_depth,
            max_values: self.max_json_values,
            max_string_bytes: self.max_string_bytes,
        }
    }
}

/// What a parse skipped or could not align.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryReport {
    pub points_read: u64,
    pub results_read: u64,
    pub skipped_points: u64,
    pub skipped_results: u64,
    /// Allure 2 groups whose start times are far apart: the tests did not run in the same
    /// past reports, so aligning them by position may have mixed reports.
    pub misaligned_points: u64,
    pub warnings: Vec<String>,
}

impl HistoryReport {
    pub fn warn(&mut self, message: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(message.into());
        }
    }
}

fn invalid(message: impl Into<String>) -> IngestError {
    IngestError::Unsupported(message.into())
}

fn limit(name: &'static str, max: u64, context: &str) -> IngestError {
    IngestError::LimitExceeded {
        limit: name,
        max,
        context: Some(context.to_string()),
    }
}

fn number(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && f.abs() < 9.0e15)
            .map(|f| f.round() as i64)
    })
}

fn duration_of(v: Option<&Value>) -> Option<i64> {
    v.and_then(number)
        .filter(|d| (0..=MAX_DURATION_MS).contains(d))
}

fn time_of(v: Option<&Value>) -> Option<i64> {
    v.and_then(number)
        .filter(|t| (MIN_TIMESTAMP_MS..=MAX_TIMESTAMP_MS).contains(t))
}

fn clip(s: &str, max: u64) -> String {
    let max = max as usize;
    let plain = |c: char| {
        if c.is_control() && c != '\n' && c != '\t' {
            ' '
        } else {
            c
        }
    };
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].chars().map(plain).collect()
}

fn text(v: Option<&Value>, max: u64) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| clip(s, max).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn scan(bytes: &[u8], limits: &HistoryLimits, context: &str) -> Result<u64, IngestError> {
    scan_json(bytes, &limits.bounds(), context)
}

/// One past run of a test in `history.json`.
struct Item {
    status: Status,
    start: Option<i64>,
    stop: Option<i64>,
    duration: Option<i64>,
    message: Option<String>,
    url: Option<String>,
}

fn item_of(v: &Value, limits: &HistoryLimits) -> Item {
    let time = v.get("time");
    let details = v.get("statusDetails");
    let message = match details {
        Some(s @ Value::String(_)) => text(Some(s), limits.max_message_bytes),
        Some(o @ Value::Object(_)) => text(o.get("message"), limits.max_message_bytes),
        _ => None,
    };
    Item {
        status: Status::parse_lenient(v.get("status").and_then(Value::as_str).unwrap_or("")),
        start: time_of(time.and_then(|t| t.get("start"))),
        stop: time_of(time.and_then(|t| t.get("stop"))),
        duration: duration_of(time.and_then(|t| t.get("duration"))),
        message,
        url: text(v.get("reportUrl"), 512),
    }
}

impl Item {
    fn span(&self) -> Option<(i64, i64)> {
        match (self.start, self.stop, self.duration) {
            (Some(a), Some(b), _) if b >= a => Some((a, b)),
            (Some(a), _, Some(d)) => Some((a, a.saturating_add(d))),
            (Some(a), _, None) => Some((a, a)),
            (None, Some(b), Some(d)) => Some((b.saturating_sub(d), b)),
            (None, Some(b), None) => Some((b, b)),
            _ => None,
        }
    }
}

struct Collector {
    limits: HistoryLimits,
    series: Vec<(String, Vec<Item>)>,
    items: u64,
    skipped_keys: u64,
    tripped: Option<(&'static str, u64)>,
}

impl Collector {
    fn trip<E: de::Error>(&mut self, name: &'static str, max: u64) -> E {
        self.tripped = Some((name, max));
        E::custom(format!("limit {name} exceeded"))
    }
}

struct MapSeed<'a>(&'a RefCell<Collector>);

impl<'de> DeserializeSeed<'de> for MapSeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for MapSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an object of tests")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            let max = self.0.borrow().limits.max_series;
            if split_series_key(&key).is_none() {
                self.0.borrow_mut().skipped_keys += 1;
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            if self.0.borrow().series.len() as u64 >= max {
                return Err(self.0.borrow_mut().trip("max_series", max));
            }
            let idx = {
                let mut c = self.0.borrow_mut();
                c.series.push((key, Vec::new()));
                c.series.len() - 1
            };
            map.next_value_seed(EntrySeed { c: self.0, idx })?;
        }
        Ok(())
    }
}

struct EntrySeed<'a> {
    c: &'a RefCell<Collector>,
    idx: usize,
}

impl<'de> DeserializeSeed<'de> for EntrySeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for EntrySeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a history entry")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "items" {
                map.next_value_seed(ItemsSeed {
                    c: self.c,
                    idx: self.idx,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

struct ItemsSeed<'a> {
    c: &'a RefCell<Collector>,
    idx: usize,
}

impl<'de> DeserializeSeed<'de> for ItemsSeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for ItemsSeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a list of past runs")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut n = 0u64;
        while let Some(v) = seq.next_element::<Value>()? {
            let (per, total_max, limits) = {
                let c = self.c.borrow();
                (
                    c.limits.max_items_per_series,
                    c.limits.max_results,
                    c.limits.clone(),
                )
            };
            n += 1;
            if n > per {
                return Err(self.c.borrow_mut().trip("max_items_per_series", per));
            }
            let mut c = self.c.borrow_mut();
            c.items += 1;
            if c.items > total_max {
                return Err(c.trip("max_results", total_max));
            }
            let item = item_of(&v, &limits);
            c.series[self.idx].1.push(item);
        }
        Ok(())
    }
}

/// The files of an Allure 2 `history` directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct Allure2Files<'a> {
    pub history: &'a [u8],
    pub history_trend: Option<&'a [u8]>,
    pub duration_trend: Option<&'a [u8]>,
    pub categories_trend: Option<&'a [u8]>,
    pub retry_trend: Option<&'a [u8]>,
}

/// Past reports with what the parse skipped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Oldest first.
    pub points: Vec<HistoryPoint>,
    pub report: HistoryReport,
}

#[derive(Default)]
struct TrendEntry {
    data: Value,
    name: Option<String>,
    url: Option<String>,
}

fn parse_trend(
    bytes: Option<&[u8]>,
    file: &'static str,
    limits: &HistoryLimits,
    report: &mut HistoryReport,
) -> Result<Vec<TrendEntry>, IngestError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    if bytes.len() as u64 > limits.max_trend_bytes {
        return Err(limit("max_trend_bytes", limits.max_trend_bytes, file));
    }
    scan(bytes, limits, file)?;
    let v: Value =
        match serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)) {
            Ok(v) => v,
            Err(e) => {
                report.warn(format!("{file}: not valid JSON ({e}); ignored"));
                return Ok(Vec::new());
            }
        };
    let Value::Array(items) = v else {
        report.warn(format!("{file}: not a list; ignored"));
        return Ok(Vec::new());
    };
    if items.len() as u64 > limits.max_points {
        return Err(limit("max_points", limits.max_points, file));
    }
    Ok(items
        .into_iter()
        .map(|e| TrendEntry {
            name: text(e.get("reportName"), limits.max_name_bytes),
            url: text(e.get("reportUrl"), 512),
            data: e.get("data").cloned().unwrap_or(Value::Null),
        })
        .collect())
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Group {
    Url(String),
    Position(usize),
}

/// Parses an Allure 2 history directory. The tests of `history.json` carry no names: a result
/// has only its series key, so the store joins it to tests it already knows.
///
/// `history.json` lists, per test, its past runs newest first without saying which report
/// each belonged to. Runs are put together into a past report by the report URL when they
/// have one, else by their position (the n-th newest run of every test belongs to the n-th
/// newest report), which holds while the tests run in every report.
pub fn parse_allure2(
    files: &Allure2Files<'_>,
    limits: &HistoryLimits,
) -> Result<Parsed, IngestError> {
    let mut report = HistoryReport::default();
    if files.history.len() as u64 > limits.max_history_bytes {
        return Err(limit(
            "max_history_bytes",
            limits.max_history_bytes,
            "history.json",
        ));
    }
    scan(files.history, limits, "history.json")?;
    let collector = RefCell::new(Collector {
        limits: limits.clone(),
        series: Vec::new(),
        items: 0,
        skipped_keys: 0,
        tripped: None,
    });
    let bytes = files
        .history
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(files.history);
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let parsed = MapSeed(&collector)
        .deserialize(&mut de)
        .and_then(|_| de.end());
    if let Err(e) = parsed {
        if let Some((name, max)) = collector.borrow().tripped {
            return Err(limit(name, max, "history.json"));
        }
        return Err(invalid(format!(
            "history.json is not an Allure 2 history: {e}"
        )));
    }
    let c = collector.into_inner();
    if c.skipped_keys > 0 {
        report.warn(format!(
            "history.json: {} entries have a key that is not a test history id and were skipped",
            c.skipped_keys
        ));
    }
    report.results_read = c.items;

    let mut groups: BTreeMap<Group, Vec<(usize, &Item)>> = BTreeMap::new();
    let mut undated = 0u64;
    for (si, (_, items)) in c.series.iter().enumerate() {
        for (pos, item) in items.iter().enumerate() {
            if item.span().is_none() {
                undated += 1;
                continue;
            }
            let g = match &item.url {
                Some(u) => Group::Url(u.clone()),
                None => Group::Position(pos),
            };
            groups.entry(g).or_default().push((si, item));
        }
    }
    report.skipped_results += undated;
    if undated > 0 {
        report.warn(format!(
            "history.json: {undated} past runs have no usable time and were skipped"
        ));
    }
    if groups.len() as u64 > limits.max_points {
        return Err(limit("max_points", limits.max_points, "history.json"));
    }

    let totals = parse_trend(
        files.history_trend,
        "history-trend.json",
        limits,
        &mut report,
    )?;
    let durations = parse_trend(
        files.duration_trend,
        "duration-trend.json",
        limits,
        &mut report,
    )?;
    let categories = parse_trend(
        files.categories_trend,
        "categories-trend.json",
        limits,
        &mut report,
    )?;
    let retries = parse_trend(files.retry_trend, "retry-trend.json", limits, &mut report)?;
    let trend_for = |entries: &[TrendEntry], g: &Group| -> Option<usize> {
        match g {
            Group::Position(i) => (*i < entries.len()).then_some(*i),
            Group::Url(u) => entries.iter().position(|e| e.url.as_deref() == Some(u)),
        }
    };

    let mut points = Vec::with_capacity(groups.len());
    for (g, members) in &groups {
        let min_start = members
            .iter()
            .filter_map(|(_, i)| i.span().map(|s| s.0))
            .min()
            .unwrap_or(0);
        let max_start = members
            .iter()
            .filter_map(|(_, i)| i.span().map(|s| s.0))
            .max()
            .unwrap_or(0);
        if matches!(g, Group::Position(_)) && max_start - min_start > MISALIGNED_SPREAD_MS {
            report.misaligned_points += 1;
        }
        let mut seen = std::collections::HashSet::new();
        let results: Vec<HistoryResult> = members
            .iter()
            .filter(|(si, _)| seen.insert(*si))
            .map(|(si, item)| {
                let (start, stop) = item.span().unwrap_or((min_start, min_start));
                HistoryResult {
                    series_key: c.series[*si].0.clone(),
                    full_name: None,
                    name: None,
                    status: item.status,
                    start_ms: Some(start),
                    stop_ms: Some(stop),
                    duration_ms: item.duration.or(Some(stop - start)),
                    message: item.message.clone(),
                    retries: Vec::new(),
                    environment: None,
                }
            })
            .collect();
        let mut trend = HistoryTrend::default();
        let mut name = None;
        if let Some(i) = trend_for(&totals, g) {
            let d = &totals[i].data;
            let n = |k: &str| d.get(k).and_then(number).unwrap_or(0).max(0);
            trend.totals = Some(HistoryTotals {
                passed: n("passed"),
                failed: n("failed"),
                broken: n("broken"),
                skipped: n("skipped"),
                unknown: n("unknown"),
                total: n("total"),
            });
            name = totals[i].name.clone();
        }
        if let Some(i) = trend_for(&durations, g) {
            trend.duration_ms = durations[i]
                .data
                .get("duration")
                .and_then(number)
                .filter(|d| (0..=MAX_DURATION_MS).contains(d));
            name = name.or_else(|| durations[i].name.clone());
        }
        if let Some(i) = trend_for(&retries, g) {
            let d = &retries[i].data;
            trend.retries = d.get("retry").and_then(number).filter(|v| *v >= 0);
            trend.runs = d.get("run").and_then(number).filter(|v| *v >= 0);
        }
        if let Some(i) = trend_for(&categories, g)
            && let Value::Object(m) = &categories[i].data
        {
            trend.categories = m
                .iter()
                .filter_map(|(k, v)| {
                    number(v)
                        .filter(|n| *n >= 0)
                        .map(|n| (clip(k, limits.max_name_bytes.min(100)), n))
                })
                .take(limits.max_trend_entries as usize)
                .collect();
        }
        let key = match g {
            Group::Url(u) => format!("a2:{}", md5_hex(u)),
            Group::Position(_) => format!("a2:{min_start}"),
        };
        points.push(HistoryPoint {
            key,
            name: name.unwrap_or_else(|| "Allure report".to_string()),
            timestamp_ms: min_start,
            url: match g {
                Group::Url(u) => Some(u.clone()),
                _ => None,
            },
            trend,
            results,
        });
    }
    points.sort_by(|a, b| a.timestamp_ms.cmp(&b.timestamp_ms).then(a.key.cmp(&b.key)));
    points.dedup_by(|b, a| {
        if a.key == b.key {
            a.results.append(&mut b.results);
            true
        } else {
            false
        }
    });
    if report.misaligned_points > 0 {
        report.warn(format!(
            "{} past reports group runs that started more than 3 days apart: tests that did not run in every report shift the alignment (use report URLs or import the history of one pipeline only)",
            report.misaligned_points
        ));
    }
    report.points_read = points.len() as u64;
    Ok(Parsed { points, report })
}

/// Reads an Allure 3 history file (one JSON line per past report) point by point.
pub struct Allure3Reader<R> {
    reader: R,
    limits: HistoryLimits,
    bytes: u64,
    results: Cell<u64>,
    pub report: HistoryReport,
}

impl<R: BufRead> Allure3Reader<R> {
    pub fn new(reader: R, limits: HistoryLimits) -> Self {
        Allure3Reader {
            reader,
            limits,
            bytes: 0,
            results: Cell::new(0),
            report: HistoryReport::default(),
        }
    }

    fn line(&mut self) -> Result<Option<Vec<u8>>, IngestError> {
        let max = self.limits.max_line_bytes;
        let mut buf = Vec::new();
        let n = std::io::Read::take(&mut self.reader, max + 1)
            .read_until(b'\n', &mut buf)
            .map_err(IngestError::from_io)?;
        if n == 0 {
            return Ok(None);
        }
        self.bytes += n as u64;
        if self.bytes > self.limits.max_total_bytes {
            return Err(limit(
                "max_total_bytes",
                self.limits.max_total_bytes,
                "the history file",
            ));
        }
        if buf.last() != Some(&b'\n') && n as u64 > max {
            return Err(limit("max_line_bytes", max, "a line of the history file"));
        }
        Ok(Some(buf))
    }

    /// The next past report; `None` at the end. A line that is not a past report is skipped
    /// with a warning. A limit is an error.
    pub fn next_point(&mut self) -> Result<Option<HistoryPoint>, IngestError> {
        loop {
            let Some(line) = self.line()? else {
                return Ok(None);
            };
            let trimmed = line.trim_ascii();
            if trimmed.is_empty() {
                continue;
            }
            if self.report.points_read + self.report.skipped_points >= self.limits.max_points {
                return Err(limit(
                    "max_points",
                    self.limits.max_points,
                    "the history file",
                ));
            }
            scan(trimmed, &self.limits, "a line of the history file")?;
            let no = self.report.points_read + self.report.skipped_points + 1;
            match self.point(trimmed) {
                Ok(Some(p)) => {
                    self.report.points_read += 1;
                    return Ok(Some(p));
                }
                Ok(None) => {
                    self.report.skipped_points += 1;
                    self.report
                        .warn(format!("line {no}: not a past report; skipped"));
                }
                Err(PointError::Limit(e)) => return Err(e),
                Err(PointError::Invalid(m)) => {
                    self.report.skipped_points += 1;
                    self.report.warn(format!("line {no}: {m}; skipped"));
                }
            }
        }
    }

    fn point(&mut self, line: &[u8]) -> Result<Option<HistoryPoint>, PointError> {
        let state = RefCell::new(PointState {
            limits: self.limits.clone(),
            results_total: &self.results,
            results: Vec::new(),
            skipped: 0,
            uuid: None,
            name: None,
            url: None,
            timestamp: None,
            metrics: Vec::new(),
            tripped: None,
        });
        let mut de = serde_json::Deserializer::from_slice(line);
        let r = PointSeed(&state)
            .deserialize(&mut de)
            .and_then(|_| de.end());
        let st = state.into_inner();
        if let Err(e) = r {
            if let Some((name, max)) = st.tripped {
                return Err(PointError::Limit(limit(name, max, "the history file")));
            }
            return Err(PointError::Invalid(format!("invalid JSON ({e})")));
        }
        self.report.skipped_results += st.skipped;
        self.report.results_read += st.results.len() as u64;
        let Some(uuid) = st
            .uuid
            .filter(|u| !u.is_empty() && u.len() <= 128 && u.chars().all(|c| !c.is_control()))
        else {
            return Ok(None);
        };
        let Some(ts) = st
            .timestamp
            .filter(|t| (MIN_TIMESTAMP_MS..=MAX_TIMESTAMP_MS).contains(t))
        else {
            return Err(PointError::Invalid(
                "timestamp missing or out of range".into(),
            ));
        };
        Ok(Some(HistoryPoint {
            key: format!("a3:{uuid}"),
            name: st.name.unwrap_or_else(|| "Allure report".to_string()),
            timestamp_ms: ts,
            url: st.url,
            trend: HistoryTrend {
                metrics: st.metrics,
                ..Default::default()
            },
            results: st.results,
        }))
    }
}

enum PointError {
    Limit(IngestError),
    Invalid(String),
}

struct PointState<'a> {
    limits: HistoryLimits,
    results_total: &'a Cell<u64>,
    results: Vec<HistoryResult>,
    skipped: u64,
    uuid: Option<String>,
    name: Option<String>,
    url: Option<String>,
    timestamp: Option<i64>,
    metrics: Vec<(String, f64)>,
    tripped: Option<(&'static str, u64)>,
}

struct PointSeed<'a, 'b>(&'a RefCell<PointState<'b>>);

impl<'de> DeserializeSeed<'de> for PointSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for PointSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a history data point")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "testResults" => map.next_value_seed(ResultsSeed(self.0))?,
                "uuid" | "name" | "url" | "timestamp" | "metrics" => {
                    let v: Value = map.next_value()?;
                    let mut st = self.0.borrow_mut();
                    let max = st.limits.max_name_bytes;
                    match key.as_str() {
                        "uuid" => st.uuid = text(Some(&v), 128),
                        "name" => st.name = text(Some(&v), max),
                        "url" => st.url = text(Some(&v), 512),
                        "timestamp" => st.timestamp = number(&v),
                        _ => {
                            if let Value::Object(m) = &v {
                                let cap = st.limits.max_trend_entries as usize;
                                st.metrics = m
                                    .iter()
                                    .filter_map(|(k, x)| {
                                        x.as_f64()
                                            .filter(|f| f.is_finite())
                                            .map(|f| (clip(k, 100), f))
                                    })
                                    .take(cap)
                                    .collect();
                            }
                        }
                    }
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

struct ResultsSeed<'a, 'b>(&'a RefCell<PointState<'b>>);

impl<'de> DeserializeSeed<'de> for ResultsSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ResultsSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an object of test results")
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            let v: Value = map.next_value()?;
            let mut st = self.0.borrow_mut();
            let (per_point, total_max) = (st.limits.max_results_per_point, st.limits.max_results);
            if st.results.len() as u64 >= per_point {
                st.tripped = Some(("max_results_per_point", per_point));
                return Err(de::Error::custom("limit max_results_per_point exceeded"));
            }
            let total = st.results_total.get() + 1;
            if total > total_max {
                st.tripped = Some(("max_results", total_max));
                return Err(de::Error::custom("limit max_results exceeded"));
            }
            st.results_total.set(total);
            match result_of(&key, &v, &st.limits) {
                Some(r) => st.results.push(r),
                None => st.skipped += 1,
            }
        }
        Ok(())
    }
}

fn result_of(key: &str, v: &Value, limits: &HistoryLimits) -> Option<HistoryResult> {
    let series_key = if split_series_key(key).is_some() {
        key.to_string()
    } else {
        let h = v.get("retryHash").and_then(Value::as_str)?;
        split_series_key(h)?;
        h.to_string()
    };
    let env = text(v.get("environment"), limits.max_name_bytes).filter(|e| e != "default");
    let retries: Vec<Status> = match v.get("retries") {
        Some(Value::Array(a)) => a
            .iter()
            .take(limits.max_retries as usize)
            .map(|s| Status::parse_lenient(s.as_str().unwrap_or("")))
            .collect(),
        _ => Vec::new(),
    };
    let start = time_of(v.get("start"));
    let stop = time_of(v.get("stop"));
    let duration = duration_of(v.get("duration")).or(match (start, stop) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    });
    Some(HistoryResult {
        series_key,
        full_name: text(v.get("fullName"), limits.max_name_bytes),
        name: text(v.get("name"), limits.max_name_bytes),
        status: Status::parse_lenient(v.get("status").and_then(Value::as_str).unwrap_or("")),
        start_ms: start,
        stop_ms: stop,
        duration_ms: duration,
        message: text(v.get("message"), limits.max_message_bytes),
        retries,
        environment: env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "d41d8cd98f00b204e9800998ecf8427e";

    fn key(n: u32) -> String {
        format!("{n:032x}.{H}")
    }

    fn history(tests: u32, runs: u32) -> String {
        let mut m = serde_json::Map::new();
        for t in 0..tests {
            let items: Vec<Value> = (0..runs)
                .map(|r| {
                    let start = 1_700_000_000_000i64 + (runs - r) as i64 * 86_400_000 + t as i64;
                    serde_json::json!({"status": if r % 2 == 0 { "passed" } else { "failed" },
                        "statusDetails": "boom", "time": {"start": start, "stop": start + 10, "duration": 10}})
                })
                .collect();
            m.insert(key(t), serde_json::json!({"statistic": {}, "items": items}));
        }
        Value::Object(m).to_string()
    }

    #[test]
    fn allure2_groups_by_position_oldest_first() {
        let h = history(3, 4);
        let p = parse_allure2(
            &Allure2Files {
                history: h.as_bytes(),
                ..Default::default()
            },
            &HistoryLimits::default(),
        )
        .unwrap();
        assert_eq!(p.points.len(), 4);
        assert!(
            p.points
                .windows(2)
                .all(|w| w[0].timestamp_ms < w[1].timestamp_ms)
        );
        assert!(p.points.iter().all(|x| x.results.len() == 3));
        assert_eq!(p.points[3].results[0].status, Status::Passed);
        assert_eq!(p.points[2].results[0].status, Status::Failed);
        assert_eq!(p.points[0].results[0].message.as_deref(), Some("boom"));
        assert_eq!(p.report.misaligned_points, 0);
    }

    #[test]
    fn allure2_trends_follow_the_position() {
        let h = history(1, 2);
        let totals =
            br#"[{"data":{"passed":5,"failed":1,"total":6}},{"data":{"passed":3,"total":3}}]"#;
        let dur = br#"[{"data":{"duration":900}},{"data":{"duration":700}}]"#;
        let cats = br#"[{"data":{"Product defects":2}},{"data":{}}]"#;
        let retry = br#"[{"data":{"run":6,"retry":1}},{"data":{"run":3,"retry":0}}]"#;
        let p = parse_allure2(
            &Allure2Files {
                history: h.as_bytes(),
                history_trend: Some(totals),
                duration_trend: Some(dur),
                categories_trend: Some(cats),
                retry_trend: Some(retry),
            },
            &HistoryLimits::default(),
        )
        .unwrap();
        let newest = &p.points[1].trend;
        assert_eq!(newest.totals.unwrap().total, 6);
        assert_eq!(newest.duration_ms, Some(900));
        assert_eq!(newest.retries, Some(1));
        assert_eq!(newest.categories, vec![("Product defects".to_string(), 2)]);
        assert_eq!(p.points[0].trend.totals.unwrap().passed, 3);
    }

    #[test]
    fn allure2_skips_foreign_keys_and_undated_runs() {
        let h = format!(
            r#"{{"not-a-key":{{"items":[]}},"{}":{{"items":[{{"status":"passed"}},{{"status":"passed","time":{{"start":1700000000000,"stop":1700000000010}}}}]}}}}"#,
            key(1)
        );
        let p = parse_allure2(
            &Allure2Files {
                history: h.as_bytes(),
                ..Default::default()
            },
            &HistoryLimits::default(),
        )
        .unwrap();
        assert_eq!(p.points.len(), 1);
        assert_eq!(p.report.skipped_results, 1);
        assert_eq!(p.report.warnings.len(), 2);
    }

    #[test]
    fn allure2_limits_name_the_field() {
        let h = history(5, 3);
        let tight = |f: fn(&mut HistoryLimits)| {
            let mut l = HistoryLimits::default();
            f(&mut l);
            parse_allure2(
                &Allure2Files {
                    history: h.as_bytes(),
                    ..Default::default()
                },
                &l,
            )
            .unwrap_err()
            .limit_name()
        };
        assert_eq!(tight(|l| l.max_series = 4), Some("max_series"));
        assert_eq!(
            tight(|l| l.max_items_per_series = 2),
            Some("max_items_per_series")
        );
        assert_eq!(tight(|l| l.max_results = 10), Some("max_results"));
        assert_eq!(
            tight(|l| l.max_history_bytes = 100),
            Some("max_history_bytes")
        );
        assert_eq!(tight(|l| l.max_points = 2), Some("max_points"));
        assert_eq!(tight(|l| l.max_json_depth = 2), Some("max_json_depth"));
    }

    #[test]
    fn allure2_malformed() {
        let parse = |b: &[u8]| {
            parse_allure2(
                &Allure2Files {
                    history: b,
                    ..Default::default()
                },
                &HistoryLimits::default(),
            )
        };
        assert!(parse(b"").is_err());
        assert!(parse(b"[1,2]").is_err());
        assert!(parse(b"{\"a\":").is_err());
        assert!(parse(b"{} trailing").is_err());
        assert_eq!(parse(b"{}").unwrap().points.len(), 0);
        let h = history(1, 1);
        let p = parse_allure2(
            &Allure2Files {
                history: h.as_bytes(),
                history_trend: Some(b"{\"a\":1}"),
                duration_trend: Some(b"not json"),
                ..Default::default()
            },
            &HistoryLimits::default(),
        )
        .unwrap();
        assert_eq!(p.points.len(), 1);
        assert_eq!(p.report.warnings.len(), 2);
    }

    fn line(uuid: &str, ts: i64, tests: u32) -> String {
        let mut m = serde_json::Map::new();
        for t in 0..tests {
            m.insert(
                key(t),
                serde_json::json!({"id": "x", "name": format!("t{t}"), "fullName": format!("a.b.t{t}"),
                    "environment": "default", "status": "failed", "retries": ["failed", "passed"],
                    "message": "m", "start": ts, "stop": ts + 5, "duration": 5, "trace": "x".repeat(50), "retryHash": key(t)}),
            );
        }
        serde_json::json!({"uuid": uuid, "name": "Demo", "timestamp": ts, "url": "", "knownTestCaseIds": [],
            "testResults": m, "metrics": {"coverage": 0.5}})
        .to_string()
    }

    fn read(
        text: &str,
        l: HistoryLimits,
    ) -> (Vec<HistoryPoint>, HistoryReport, Option<IngestError>) {
        let mut r = Allure3Reader::new(text.as_bytes(), l);
        let mut out = Vec::new();
        loop {
            match r.next_point() {
                Ok(Some(p)) => out.push(p),
                Ok(None) => return (out, r.report, None),
                Err(e) => return (out, r.report, Some(e)),
            }
        }
    }

    #[test]
    fn allure3_points() {
        let t = format!(
            "{}\n\n{}\n",
            line("u1", 1_700_000_000_000, 2),
            line("u2", 1_700_086_400_000, 3)
        );
        let (p, rep, err) = read(&t, HistoryLimits::default());
        assert!(err.is_none());
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].key, "a3:u1");
        assert_eq!(p[1].results.len(), 3);
        let r = &p[0].results[0];
        assert_eq!(r.full_name.as_deref(), Some("a.b.t0"));
        assert_eq!(r.environment, None);
        assert_eq!(r.retries, vec![Status::Failed, Status::Passed]);
        assert_eq!(p[0].trend.metrics, vec![("coverage".to_string(), 0.5)]);
        assert_eq!(rep.points_read, 2);
        assert_eq!(rep.results_read, 5);
    }

    #[test]
    fn allure3_bad_lines_are_skipped() {
        let t = format!(
            "not json\n{}\n{{\"uuid\":\"u\",\"timestamp\":5,\"testResults\":{{}}}}\n[1]\n{}\n",
            line("ok", 1_700_000_000_000, 1),
            line("ok2", 1_700_000_100_000, 1)
        );
        let (p, rep, err) = read(&t, HistoryLimits::default());
        assert!(err.is_none());
        assert_eq!(p.len(), 2);
        assert_eq!(rep.skipped_points, 3);
    }

    #[test]
    fn allure3_results_with_foreign_keys() {
        let t = format!(
            r#"{{"uuid":"u","timestamp":1700000000000,"testResults":{{"legacy-id":{{"status":"passed","retryHash":"{}"}},"other":{{"status":"passed"}}}}}}"#,
            key(7)
        );
        let (p, rep, _) = read(&t, HistoryLimits::default());
        assert_eq!(p[0].results.len(), 1);
        assert_eq!(p[0].results[0].series_key, key(7));
        assert_eq!(rep.skipped_results, 1);
    }

    #[test]
    fn allure3_limits() {
        let t = format!(
            "{}\n{}\n",
            line("a", 1_700_000_000_000, 3),
            line("b", 1_700_000_100_000, 3)
        );
        let lim = |f: fn(&mut HistoryLimits)| {
            let mut l = HistoryLimits::default();
            f(&mut l);
            read(&t, l).2.and_then(|e| e.limit_name())
        };
        assert_eq!(lim(|l| l.max_points = 1), Some("max_points"));
        assert_eq!(lim(|l| l.max_results = 4), Some("max_results"));
        assert_eq!(
            lim(|l| l.max_results_per_point = 2),
            Some("max_results_per_point")
        );
        assert_eq!(lim(|l| l.max_line_bytes = 50), Some("max_line_bytes"));
        assert_eq!(lim(|l| l.max_total_bytes = 600), Some("max_total_bytes"));
        assert_eq!(lim(|l| l.max_json_depth = 1), Some("max_json_depth"));
    }

    #[test]
    fn control_characters_become_spaces_in_names_and_keys() {
        let t = format!(
            r#"{{"uuid":"u","name":"a\u0000b","timestamp":1700000000000,"metrics":{{"m\u0001x":1}},"testResults":{{"{}":{{"status":"failed","name":"n\u0007","message":"l1\nl2"}}}}}}"#,
            key(1)
        );
        let (p, _, _) = read(&t, HistoryLimits::default());
        assert_eq!(p[0].name, "a b");
        assert_eq!(p[0].trend.metrics[0].0, "m x");
        assert_eq!(p[0].results[0].name.as_deref(), Some("n"));
        assert_eq!(p[0].results[0].message.as_deref(), Some("l1\nl2"));
    }

    #[test]
    fn long_texts_are_clipped_on_a_character_boundary() {
        let name = "é".repeat(5000);
        let t = format!(
            r#"{{"uuid":"u","timestamp":1700000000000,"testResults":{{"{}":{{"status":"failed","name":"{name}","message":"{name}"}}}}}}"#,
            key(1)
        );
        let (p, _, _) = read(&t, HistoryLimits::default());
        let r = &p[0].results[0];
        assert!(r.name.as_ref().unwrap().len() <= 4096);
        assert!(r.message.as_ref().unwrap().len() <= 1024);
    }
}
