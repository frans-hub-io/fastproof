//! Code sync: launches finished with `caseSync.enabled` are queued (`case_sync_queue`); the
//! `cases.sync` job takes them with a lease and, for a trusted launch only, links the code
//! identities of its results to cases (`ALLURE_ID` key, TMS keys when enabled, else a new case
//! with `origin = code`) and applies the values that code owns (`caseSync.fields`) when their
//! hash changed: one version with `source = code` and the launch, `revision + 1`,
//! `case.updated`. Untrusted launches change nothing: their identities stay unlinked.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use fastproof_model::identity::md5_hex;
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use super::cases::{
    CaseCtx, Commit, MAX_LINKS, MAX_URL_BYTES, NewCaseMeta, case_event, case_sync_settings, commit,
    create_case_in, reserve_numbers, valid_link_url,
};
use super::fields::field_defs;
use super::folders::ensure_folder_path;
use super::keys::{case_by_key, set_case_key};
use super::lookup::{CASE_COLS, case_row};
use super::versions::snapshot_of;
use super::{
    CaseBody, CaseFieldDef, CaseKeyKind, CaseLink, CaseLinkType, CaseOrigin, CaseRow, CaseSnapshot,
    CaseStatus, FieldType, FieldValue, MAX_TAG_BYTES, TmError, TmLimits, TmResult, VersionSource,
};
use crate::Store;
use crate::analysis::settings::{CaseSyncFields, CaseSyncSettings, FieldPolicy, FolderFrom};

/// Identities handled per transaction.
pub const SYNC_BATCH: usize = 1000;
/// Attempts of one queued launch before it is dropped.
pub const MAX_SYNC_ATTEMPTS: i32 = 5;
/// Who code sync writes as.
pub const SYNC_ACTOR: &str = "code-sync";

/// What a launch tells about its trust.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchTrust {
    pub debug: bool,
    pub branch: Option<String>,
    /// `vcs.defaultBranch` as the CLI reported it.
    pub default_branch: Option<String>,
    /// The creating token had `cases:sync`, and every caller that added results to the
    /// launch had it too (`launches.sync_blocked` is false).
    pub can_sync: bool,
}

/// Whether code sync may change cases from this launch: sync is on, the launch is not debug
/// (unless `includeDebug`), its branch matches `branches` or is the CI default branch (when
/// `useCiDefaultBranch`), and its creator had `cases:sync`.
pub fn launch_trusted(s: &CaseSyncSettings, l: &LaunchTrust) -> bool {
    s.enabled
        && (!l.debug || s.include_debug)
        && l.can_sync
        && s.branch_trusted(l.branch.as_deref(), l.default_branch.as_deref())
}

/// What one result of a code identity says about its test.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodeData {
    pub full_name: String,
    pub name: String,
    pub description: Option<String>,
    pub links: Vec<CaseLink>,
    pub labels: Vec<(String, String)>,
    pub suite_path: Vec<String>,
    /// `ALLURE_ID` of the result, else the explicit id of the identity when it is one.
    pub allure_id: Option<String>,
    /// Links as sent, with their Allure link type (at most [`MAX_LINKS`]); requirement links
    /// come from them.
    pub raw_links: Vec<RawLink>,
}

/// A link of a result with the Allure link type as sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawLink {
    pub link_type: String,
    pub name: String,
    pub url: String,
}

impl CodeData {
    fn label_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.labels
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.trim())
            .filter(|v| !v.is_empty())
    }

    /// TestRail or Qase keys from `tms` labels and TMS links.
    pub fn tms_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in ["tms", "TestRail", "testrail", "Qase", "qase"] {
            out.extend(self.label_values(n).map(str::to_string));
        }
        for l in &self.links {
            if l.link_type == CaseLinkType::Tms && !l.name.trim().is_empty() {
                out.push(l.name.trim().to_string());
            }
        }
        out.sort();
        out.dedup();
        out.truncate(20);
        out
    }
}

/// The values code proposes for a case, after the limits.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CodeValues {
    pub name: String,
    pub description: String,
    pub links: Vec<CaseLink>,
    pub tags: Vec<String>,
    /// Fields with a source label, by key (absent = no value).
    pub fields: BTreeMap<String, FieldValue>,
}

fn cut(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end().to_string()
}

fn cut_markdown(s: &str, max: usize) -> String {
    let s = s.replace('\0', "");
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn label_value(def: &CaseFieldDef, values: &[&str], limits: &TmLimits) -> FieldValue {
    let active = |v: &str| {
        def.options
            .iter()
            .find(|o| !o.archived && o.value.eq_ignore_ascii_case(v))
            .map(|o| o.value.clone())
    };
    match def.field_type {
        FieldType::Select => values
            .iter()
            .find_map(|v| active(v))
            .map(FieldValue::Text)
            .unwrap_or_default(),
        FieldType::Multiselect => {
            let mut out: Vec<String> = Vec::new();
            for v in values {
                if let Some(o) = active(v)
                    && !out.contains(&o)
                {
                    out.push(o);
                }
            }
            if out.is_empty() {
                FieldValue::Null
            } else {
                FieldValue::Many(out)
            }
        }
        FieldType::Text => values
            .first()
            .map(|v| FieldValue::Text(cut(v, limits.max_markdown_bytes)))
            .unwrap_or_default(),
        FieldType::Number => values
            .iter()
            .find_map(|v| v.parse::<f64>().ok().filter(|n| n.is_finite()))
            .map(FieldValue::Number)
            .unwrap_or_default(),
        FieldType::Date => values
            .iter()
            .find(|v| super::fields::valid_date(v))
            .map(|v| FieldValue::Text(v.to_string()))
            .unwrap_or_default(),
    }
}

/// The values code proposes, from a result: name, description, http(s) links, `tag` labels
/// and one value per field with a source label (select values must be options).
pub fn code_values(data: &CodeData, defs: &[CaseFieldDef], limits: &TmLimits) -> CodeValues {
    let mut name = cut(&data.name, limits.max_name_bytes);
    if name.is_empty() {
        name = cut(&data.full_name, limits.max_name_bytes);
    }
    if name.is_empty() {
        name = "Unnamed test".into();
    }
    let links = data
        .links
        .iter()
        .filter(|l| {
            let u = l.url.trim().to_ascii_lowercase();
            (u.starts_with("https://") || u.starts_with("http://")) && valid_link_url(l.url.trim())
        })
        .take(MAX_LINKS)
        .map(|l| CaseLink {
            name: cut(&l.name, limits.max_name_bytes),
            url: l.url.trim().to_string(),
            link_type: l.link_type,
        })
        .collect();
    let mut tags: Vec<String> = Vec::new();
    for t in data.label_values("tag") {
        if t.len() <= MAX_TAG_BYTES
            && !t.chars().any(char::is_control)
            && !tags.iter().any(|x| x == t)
        {
            tags.push(t.to_string());
        }
        if tags.len() >= limits.max_tags {
            break;
        }
    }
    let mut fields = BTreeMap::new();
    for d in defs.iter().filter(|d| !d.archived) {
        let Some(label) = d.source_label.as_deref() else {
            continue;
        };
        let values: Vec<&str> = data.label_values(label).collect();
        let v = label_value(d, &values, limits);
        if !v.is_null() {
            fields.insert(d.key.clone(), v);
        }
    }
    CodeValues {
        name,
        description: data
            .description
            .as_deref()
            .map(|d| cut_markdown(d, limits.max_markdown_bytes))
            .unwrap_or_default(),
        links,
        tags,
        fields,
    }
}

/// Hash of the code values and the field policy (a policy change re-applies the values).
pub fn code_hash(v: &CodeValues, policy: &CaseSyncFields) -> String {
    let doc = serde_json::json!({"values": v, "policy": policy});
    md5_hex(&doc.to_string())
}

fn policy_of(policy: &CaseSyncFields, def: &CaseFieldDef) -> FieldPolicy {
    def.source_label
        .as_deref()
        .and_then(|l| policy.policy(l))
        .unwrap_or(FieldPolicy::Code)
}

/// The content after sync: a new case takes every code value; an existing one takes only the
/// values whose policy is `code`, and an empty or missing code value never clears the case's
/// value. With `fill_only` (a case written by people that code links for the first time) code
/// fills only the values the case does not have, so the manual text wins. Steps,
/// preconditions, folder, status and fields without a source label always stay with the case.
pub fn synced_snapshot(
    cur: Option<&CaseSnapshot>,
    v: &CodeValues,
    policy: &CaseSyncFields,
    defs: &[CaseFieldDef],
    fill_only: bool,
) -> CaseSnapshot {
    let Some(cur) = cur else {
        return CaseSnapshot {
            name: v.name.clone(),
            status: CaseStatus::Active,
            folder_id: None,
            body: CaseBody {
                description: v.description.clone(),
                preconditions: String::new(),
                steps: Vec::new(),
                links: v.links.clone(),
                parameters: None,
            },
            fields: v.fields.clone(),
            tags: v.tags.clone(),
        };
    };
    let code = |k: &str| policy.policy(k) == Some(FieldPolicy::Code);
    let mut s = cur.clone();
    if code("name") && !v.name.trim().is_empty() && !(fill_only && !s.name.trim().is_empty()) {
        s.name = v.name.clone();
    }
    if code("description")
        && !v.description.trim().is_empty()
        && !(fill_only && !s.body.description.trim().is_empty())
    {
        s.body.description = v.description.clone();
    }
    if code("links") && !v.links.is_empty() && !(fill_only && !s.body.links.is_empty()) {
        s.body.links = v.links.clone();
    }
    if code("tags") && !v.tags.is_empty() && !(fill_only && !s.tags.is_empty()) {
        s.tags = v.tags.clone();
    }
    for d in defs
        .iter()
        .filter(|d| !d.archived && d.source_label.is_some())
    {
        if policy_of(policy, d) != FieldPolicy::Code {
            continue;
        }
        let Some(val) = v.fields.get(&d.key).filter(|v| !value_empty(v)) else {
            continue;
        };
        if fill_only && s.fields.get(&d.key).is_some_and(|c| !value_empty(c)) {
            continue;
        }
        s.fields.insert(d.key.clone(), val.clone());
    }
    s
}

fn value_empty(v: &FieldValue) -> bool {
    match v {
        FieldValue::Null => true,
        FieldValue::Text(t) => t.trim().is_empty(),
        FieldValue::Many(m) => m.is_empty(),
        FieldValue::Number(_) => false,
    }
}

/// What sync does for one identity: nothing for an untrusted launch, else the content to
/// store (create when there is no case yet).
pub fn sync_plan(
    trusted: bool,
    cur: Option<&CaseSnapshot>,
    v: &CodeValues,
    policy: &CaseSyncFields,
    defs: &[CaseFieldDef],
    fill_only: bool,
) -> Option<CaseSnapshot> {
    trusted.then(|| synced_snapshot(cur, v, policy, defs, fill_only))
}

/// The package of a full name (`com.shop.CartTest.adds` → `com.shop`), when it has one.
pub fn package_of(full_name: &str) -> Option<String> {
    let class = match full_name.find('#') {
        Some(i) => &full_name[..i],
        None => full_name.rsplit_once('.').map(|(c, _)| c)?,
    };
    let pkg = class.rsplit_once('.').map(|(p, _)| p)?;
    (!pkg.is_empty() && !pkg.contains(' ')).then(|| pkg.to_string())
}

/// Folder names of a new case under `rootFolder` (at most `maxFolderDepth` levels).
pub fn folder_names(s: &CaseSyncSettings, data: &CodeData, limits: &TmLimits) -> Vec<String> {
    let mut out = vec![cut(&s.root_folder, limits.max_name_bytes)];
    match s.folder_from {
        FolderFrom::Suite => out.extend(
            data.suite_path
                .iter()
                .map(|p| cut(p, limits.max_name_bytes)),
        ),
        FolderFrom::Package => out.extend(
            package_of(&data.full_name)
                .iter()
                .map(|p| cut(p, limits.max_name_bytes)),
        ),
        FolderFrom::None => {}
    }
    out.retain(|n| !n.is_empty());
    out.truncate(limits.max_folder_depth);
    out
}

/// One identity to sync.
#[derive(Clone, Debug)]
pub struct SyncItem {
    pub test_case_id: Uuid,
    pub data: CodeData,
    /// The launch whose result the values come from (`case_versions.launch_id`).
    pub launch_id: Option<Uuid>,
}

/// Most relinks one sync run reports for the audit log (the count is always exact).
pub const MAX_REPORTED_RELINKS: usize = 10_000;

/// A linked code identity moved by its `ALLURE_ID` to the case that id names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityRelink {
    pub project_key: String,
    pub test_case_id: Uuid,
    /// The case the identity was linked to.
    pub from_number: i64,
    /// The case its `ALLURE_ID` names.
    pub to_number: i64,
    /// `from_number` when that case was archived as a duplicate.
    pub archived_duplicate: Option<i64>,
    pub launch_id: Option<Uuid>,
}

/// What a sync batch did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub created: u64,
    pub linked: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub skipped: u64,
    /// Linked identities moved to the case their `ALLURE_ID` names.
    pub relinked: u64,
    /// The first [`MAX_REPORTED_RELINKS`] of them, for the audit log.
    pub relinks: Vec<IdentityRelink>,
}

impl SyncStats {
    /// Adds the counts and relinks of another batch.
    pub fn add(&mut self, o: SyncStats) {
        self.created += o.created;
        self.linked += o.linked;
        self.updated += o.updated;
        self.unchanged += o.unchanged;
        self.skipped += o.skipped;
        self.relinked += o.relinked;
        let room = MAX_REPORTED_RELINKS.saturating_sub(self.relinks.len());
        self.relinks.extend(o.relinks.into_iter().take(room));
    }
}

enum Target {
    Case(Uuid),
    /// A linked identity whose `ALLURE_ID` names another live case.
    Relink {
        from: Uuid,
        to: Uuid,
    },
    Create,
    Skip,
}

/// Live cases named by `ALLURE_ID` keys, by key value (one query for a batch).
async fn allure_targets(
    conn: &mut PgConnection,
    project_id: Uuid,
    values: &[String],
) -> sqlx::Result<HashMap<String, Uuid>> {
    if values.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, Uuid)> = sqlx::query_as(
        "select k.value, k.case_id from case_keys k join cases c on c.id = k.case_id \
         where k.project_id = $1 and k.kind = 'allure_id' and k.value = any($2) and c.archived_at is null",
    )
    .bind(project_id)
    .bind(values)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// After its identity moved to the case its `ALLURE_ID` names: the old case is archived with a
/// link to the target when code made it and nothing else holds it (no other code identity, no
/// manual runs, no versions from anything but code); otherwise it stays, automated only while
/// it has another code identity. Returns the row and whether it was archived.
async fn settle_relinked(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    old: &CaseRow,
    target_number: i64,
    launch_id: Option<Uuid>,
) -> TmResult<(CaseRow, bool)> {
    let (others, runs, edited): (bool, bool, bool) = sqlx::query_as(
        "select exists (select 1 from test_cases where case_id = $1 and kind = 'code'), \
         exists (select 1 from run_items where case_id = $1), \
         exists (select 1 from case_versions where case_id = $1 and source <> 'code')",
    )
    .bind(old.id)
    .fetch_one(&mut *conn)
    .await?;
    if old.origin == CaseOrigin::Code && old.archived_at.is_none() && !others && !runs && !edited {
        let mut snap = snapshot_of(old);
        let link = CaseLink {
            name: format!("Merged into TC-{target_number}"),
            url: format!("/p/{}/cases/{target_number}", ctx.project_key),
            link_type: CaseLinkType::Link,
        };
        if snap.body.links.len() < MAX_LINKS && !snap.body.links.contains(&link) {
            snap.body.links.push(link);
        }
        let row = commit(
            conn,
            ctx,
            old,
            snap,
            Commit {
                source: VersionSource::Code,
                launch_id,
                force_revision: true,
            },
        )
        .await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update cases set automated = false, archived_at = coalesce(archived_at, now()), \
             revision = revision + 1 where id = $1 returning {CASE_COLS}"
        )))
        .bind(row.id)
        .fetch_one(&mut *conn)
        .await?;
        let row = case_row(&r)?;
        case_event(
            conn,
            ctx.project_key,
            "case.archived",
            serde_json::json!({"number": row.number}),
        )
        .await?;
        return Ok((row, true));
    }
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update cases set automated = $2, revision = revision + 1, updated_at = now(), updated_by = $3 \
         where id = $1 returning {CASE_COLS}"
    )))
    .bind(old.id)
    .bind(others)
    .bind(crate::no_nul(ctx.by))
    .fetch_one(&mut *conn)
    .await?;
    let row = case_row(&r)?;
    case_event(
        conn,
        ctx.project_key,
        "case.updated",
        serde_json::json!({"number": row.number, "revision": row.revision, "source": "code"}),
    )
    .await?;
    Ok((row, false))
}

async fn live(conn: &mut PgConnection, project_id: Uuid, id: Uuid) -> sqlx::Result<bool> {
    let v: Option<bool> = sqlx::query_scalar(
        "select archived_at is null from cases where id = $1 and project_id = $2",
    )
    .bind(id)
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(v == Some(true))
}

async fn find_match(
    conn: &mut PgConnection,
    project_id: Uuid,
    s: &CaseSyncSettings,
    data: &CodeData,
) -> sqlx::Result<Option<Uuid>> {
    if let Some(a) = data.allure_id.as_deref()
        && let Some(id) = case_by_key(conn, project_id, CaseKeyKind::AllureId, a).await?
        && live(conn, project_id, id).await?
    {
        return Ok(Some(id));
    }
    if s.match_tms_links {
        for k in data.tms_keys() {
            for kind in [
                CaseKeyKind::Testrail,
                CaseKeyKind::Qase,
                CaseKeyKind::External,
            ] {
                if let Some(id) = case_by_key(conn, project_id, kind, &k).await?
                    && live(conn, project_id, id).await?
                {
                    return Ok(Some(id));
                }
            }
        }
    }
    Ok(None)
}

/// Syncs identities of a trusted source in one transaction: locks the identities, matches the
/// unlinked ones, creates cases (numbers reserved up front in identity order), links them and
/// applies the code values whose hash changed. `pool` reserves the numbers outside the
/// transaction so the project row is not held.
pub async fn sync_items(
    pool: &PgPool,
    ctx: &CaseCtx<'_>,
    s: &CaseSyncSettings,
    items: &[SyncItem],
) -> TmResult<SyncStats> {
    let mut stats = SyncStats::default();
    if items.is_empty() {
        return Ok(stats);
    }
    let mut pairs: Vec<(Uuid, &CodeData)> = Vec::with_capacity(items.len());
    let mut relinked: Vec<Uuid> = Vec::new();
    let mut tx = pool.begin().await?;
    let defs = field_defs(&mut tx, ctx.project_id, false).await?;
    let mut ids: Vec<Uuid> = items.iter().map(|i| i.test_case_id).collect();
    ids.sort();
    ids.dedup();
    let linked: HashMap<Uuid, Option<Uuid>> = sqlx::query_as::<_, (Uuid, Option<Uuid>)>(
        "select id, case_id from test_cases where project_id = $1 and id = any($2) and kind = 'code' \
         order by id for no key update",
    )
    .bind(ctx.project_id)
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    let mut explicit: Vec<String> = items
        .iter()
        .filter(|it| matches!(linked.get(&it.test_case_id), Some(Some(_))))
        .filter_map(|it| it.data.allure_id.clone())
        .collect();
    explicit.sort();
    explicit.dedup();
    let named = allure_targets(&mut tx, ctx.project_id, &explicit).await?;
    let mut targets = Vec::with_capacity(items.len());
    for it in items {
        let t = match linked.get(&it.test_case_id) {
            None => Target::Skip,
            Some(Some(c)) => match it.data.allure_id.as_deref().and_then(|a| named.get(a)) {
                Some(to) if to != c => Target::Relink { from: *c, to: *to },
                _ => Target::Case(*c),
            },
            Some(None) => match find_match(&mut tx, ctx.project_id, s, &it.data).await? {
                Some(c) => Target::Case(c),
                None if s.create_cases => Target::Create,
                None => Target::Skip,
            },
        };
        targets.push(t);
    }
    let creates = targets
        .iter()
        .filter(|t| matches!(t, Target::Create))
        .count() as i64;
    let mut next_number = if creates > 0 {
        match reserve_numbers(pool, ctx.project_id, creates, ctx.limits).await {
            Ok(n) => Some(n),
            Err(TmError::Conflict { .. }) => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    let mut case_ids: Vec<Uuid> = targets
        .iter()
        .flat_map(|t| match t {
            Target::Case(c) => vec![*c],
            Target::Relink { from, to } => vec![*from, *to],
            _ => Vec::new(),
        })
        .collect();
    case_ids.sort();
    case_ids.dedup();
    let mut cases: HashMap<Uuid, CaseRow> = HashMap::new();
    if !case_ids.is_empty() {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {CASE_COLS} from cases where project_id = $1 and id = any($2) order by id for update"
        )))
        .bind(ctx.project_id)
        .bind(&case_ids)
        .fetch_all(&mut *tx)
        .await?;
        for r in &rows {
            let c = case_row(r)?;
            cases.insert(c.id, c);
        }
    }
    for (it, target) in items.iter().zip(targets) {
        let values = code_values(&it.data, &defs, ctx.limits);
        let hash = code_hash(&values, &s.fields);
        let case_id =
            match target {
                Target::Skip => {
                    stats.skipped += 1;
                    continue;
                }
                Target::Create => {
                    let Some(n) = next_number else {
                        stats.skipped += 1;
                        continue;
                    };
                    next_number = Some(n + 1);
                    let mut snap = synced_snapshot(None, &values, &s.fields, &defs, false);
                    let names = folder_names(s, &it.data, ctx.limits);
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    snap.folder_id =
                        match ensure_folder_path(&mut tx, ctx.project_id, None, &refs, ctx.limits)
                            .await
                        {
                            Ok(f) => f,
                            Err(TmError::Other(e)) => return Err(TmError::Other(e)),
                            Err(_) => ensure_folder_path(
                                &mut tx,
                                ctx.project_id,
                                None,
                                &refs[..1.min(refs.len())],
                                ctx.limits,
                            )
                            .await
                            .unwrap_or(None),
                        };
                    let row = create_case_in(
                        &mut tx,
                        ctx,
                        snap,
                        &NewCaseMeta {
                            origin: CaseOrigin::Code,
                            source: VersionSource::Code,
                            launch_id: it.launch_id,
                            ai_unreviewed: false,
                            automated: true,
                            code_hash: Some(hash),
                            number: Some((n, true)),
                        },
                    )
                    .await?;
                    if let Some(a) = it.data.allure_id.as_deref()
                        && super::keys::check_key_value(a).is_ok()
                    {
                        set_case_key(&mut tx, ctx.project_id, row.id, CaseKeyKind::AllureId, a)
                            .await?;
                    }
                    link(&mut tx, it.test_case_id, row.id).await?;
                    relinked.push(row.id);
                    pairs.push((row.id, &it.data));
                    stats.created += 1;
                    continue;
                }
                Target::Case(case_id) => {
                    if !cases.contains_key(&case_id) {
                        stats.skipped += 1;
                        continue;
                    }
                    if linked.get(&it.test_case_id) == Some(&None) {
                        link(&mut tx, it.test_case_id, case_id).await?;
                        relinked.push(case_id);
                        stats.linked += 1;
                    }
                    case_id
                }
                Target::Relink { from, to } => {
                    let (Some(old), Some(new)) = (cases.get(&from).cloned(), cases.get(&to)) else {
                        stats.skipped += 1;
                        continue;
                    };
                    let to_number = new.number;
                    link(&mut tx, it.test_case_id, to).await?;
                    relinked.extend([from, to]);
                    let (old_row, archived) =
                        settle_relinked(&mut tx, ctx, &old, to_number, it.launch_id).await?;
                    stats.relinked += 1;
                    if stats.relinks.len() < MAX_REPORTED_RELINKS {
                        stats.relinks.push(IdentityRelink {
                            project_key: ctx.project_key.to_string(),
                            test_case_id: it.test_case_id,
                            from_number: old_row.number,
                            to_number,
                            archived_duplicate: archived.then_some(old_row.number),
                            launch_id: it.launch_id,
                        });
                    }
                    cases.insert(from, old_row);
                    to
                }
            };
        let Some(cur) = cases.get(&case_id).cloned() else {
            stats.skipped += 1;
            continue;
        };
        let mut row = cur.clone();
        if cur.code_hash.as_deref() != Some(hash.as_str()) || !cur.automated {
            let fill_only = cur.origin != CaseOrigin::Code && cur.code_hash.is_none();
            let snap = synced_snapshot(
                Some(&snapshot_of(&cur)),
                &values,
                &s.fields,
                &defs,
                fill_only,
            );
            row = commit(
                &mut tx,
                ctx,
                &cur,
                snap,
                Commit {
                    source: VersionSource::Code,
                    launch_id: it.launch_id,
                    force_revision: !cur.automated,
                },
            )
            .await?;
            if row.revision != cur.revision {
                stats.updated += 1;
            } else {
                stats.unchanged += 1;
            }
        } else {
            stats.unchanged += 1;
        }
        sqlx::query(
            "update cases set automated = true, code_hash = $2, last_trusted_at = now() where id = $1",
        )
        .bind(case_id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
        row.automated = true;
        row.code_hash = Some(hash);
        cases.insert(case_id, row);
        pairs.push((case_id, &it.data));
    }
    super::requirements::sync_code_links(&mut tx, ctx, s, &pairs).await?;
    relinked.sort();
    relinked.dedup();
    super::last_status::apply_manual(&mut tx, &relinked).await?;
    tx.commit().await?;
    Ok(stats)
}

async fn link(conn: &mut PgConnection, test_case_id: Uuid, case_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("update test_cases set case_id = $2 where id = $1")
        .bind(test_case_id)
        .bind(case_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Code data from a stored result: its name, suite path and labels, and the description and
/// links of its newest attempt.
pub fn code_data(
    full_name: &str,
    name: &str,
    suite_path: Vec<String>,
    labels: &Value,
    detail: Option<&Value>,
    explicit_id: Option<(&str, &str)>,
) -> CodeData {
    let labels: Vec<(String, String)> = labels
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    Some((
                        l.get("name")?.as_str()?.to_string(),
                        l.get("value")?.as_str()?.to_string(),
                    ))
                })
                .take(1000)
                .collect()
        })
        .unwrap_or_default();
    let allure_id = labels
        .iter()
        .find(|(n, v)| {
            (n == "ALLURE_ID" || n == "AS_ID") && !v.trim().is_empty() && v.trim() != "-1"
        })
        .map(|(_, v)| v.trim().to_string())
        .or_else(|| match explicit_id {
            Some((v, "allure_id")) if !v.trim().is_empty() => Some(v.trim().to_string()),
            _ => None,
        });
    let description = detail
        .and_then(|d| d.get("description"))
        .and_then(|d| d.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let raw_links: Vec<RawLink> = detail
        .and_then(|d| d.get("links"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let s = |k: &str| {
                        l.get(k)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .chars()
                            .take(MAX_URL_BYTES)
                            .collect::<String>()
                    };
                    let url = s("url");
                    let name = s("name");
                    (!url.is_empty() || !name.is_empty()).then(|| RawLink {
                        link_type: s("type").chars().take(64).collect(),
                        name,
                        url,
                    })
                })
                .take(MAX_LINKS)
                .collect()
        })
        .unwrap_or_default();
    let links = detail
        .and_then(|d| d.get("links"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let url = l.get("url")?.as_str()?.to_string();
                    let t = l.get("type").and_then(Value::as_str).unwrap_or("link");
                    Some(CaseLink {
                        name: l
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        url,
                        link_type: CaseLinkType::parse(t).unwrap_or(CaseLinkType::Link),
                    })
                })
                .take(MAX_LINKS)
                .collect()
        })
        .unwrap_or_default();
    CodeData {
        full_name: full_name.to_string(),
        name: name.to_string(),
        description,
        links,
        labels,
        suite_path,
        allure_id,
        raw_links,
    }
}

/// Queues a finished launch for the `cases.sync` job (always: the last result of cases follows
/// every launch; `caseSync.enabled` only decides the content sync, in `sync_launch`). Called by
/// `finish_launch` in its transaction.
pub async fn queue_launch(
    conn: &mut PgConnection,
    _project_id: Uuid,
    launch_id: Uuid,
) -> sqlx::Result<()> {
    sqlx::query(
        "insert into case_sync_queue (launch_id) values ($1) \
         on conflict (launch_id) do update set queued_at = now(), lease_owner = null, lease_until = null, attempts = 0",
    )
    .bind(launch_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

struct QueuedLaunch {
    launch_id: Uuid,
    attempts: i32,
}

impl Store {
    async fn claim_sync(&self, owner: &str) -> sqlx::Result<Option<QueuedLaunch>> {
        let r: Option<(Uuid, i32)> = sqlx::query_as(
            "update case_sync_queue set lease_owner = $1, lease_until = now() + interval '10 minutes', \
               attempts = attempts + 1 \
             where launch_id = (select launch_id from case_sync_queue \
               where lease_until is null or lease_until < now() order by queued_at limit 1 for update skip locked) \
             returning launch_id, attempts",
        )
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|(launch_id, attempts)| QueuedLaunch {
            launch_id,
            attempts,
        }))
    }

    /// Moves the last result of the launch's cases forward (every finished launch), then syncs
    /// the content when `caseSync` is enabled and the launch trusted; returns whether it was.
    pub async fn sync_launch(
        &self,
        launch_id: Uuid,
        limits: &TmLimits,
    ) -> TmResult<(bool, SyncStats)> {
        let mut conn = self.pool.acquire().await?;
        let Some(r) = sqlx::query(
            "select l.project_id, p.key, l.created_at, l.debug, l.branch, l.vcs ->> 'defaultBranch' as default_branch, \
             l.created_by_can_sync, l.sync_blocked, l.created_by from launches l \
             join projects p on p.id = l.project_id where l.id = $1 and p.deleted_at is null",
        )
        .bind(launch_id)
        .fetch_optional(&mut *conn)
        .await?
        else {
            return Ok((false, SyncStats::default()));
        };
        let project_id: Uuid = r.try_get("project_id")?;
        let key: String = r.try_get("key")?;
        let created: chrono::DateTime<chrono::Utc> = r.try_get("created_at")?;
        super::last_status::apply_launch(&mut conn, launch_id).await?;
        let trust = LaunchTrust {
            debug: r.try_get("debug")?,
            branch: r.try_get("branch")?,
            default_branch: r.try_get("default_branch")?,
            can_sync: r.try_get::<bool, _>("created_by_can_sync")?
                && !r.try_get::<bool, _>("sync_blocked")?,
        };
        let settings = case_sync_settings(&mut conn, project_id).await?;
        if !launch_trusted(&settings, &trust) {
            return Ok((false, SyncStats::default()));
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select distinct test_case_id from results where launch_id = $1 and launch_created_at = $2 \
             and run_item_id is null and not (select sync_blocked from launches where id = $1) \
             order by test_case_id",
        )
        .bind(launch_id)
        .bind(created)
        .fetch_all(&mut *conn)
        .await?;
        let ctx = CaseCtx {
            project_id,
            project_key: &key,
            by: SYNC_ACTOR,
            limits,
            can_review: false,
        };
        let mut stats = SyncStats::default();
        for chunk in ids.chunks(SYNC_BATCH) {
            let rows = sqlx::query(
                "select t.id, t.full_name, t.explicit_id, t.explicit_id_source, r.name, r.suite_path, r.labels, \
                 (select a.detail from attempts a where a.result_id = r.id and a.launch_created_at = r.launch_created_at \
                   order by a.seq desc limit 1) as detail \
                 from test_cases t cross join lateral (select r.id, r.name, r.suite_path, r.labels, r.launch_created_at \
                   from results r where r.test_case_id = t.id and r.launch_id = $1 and r.launch_created_at = $2 \
                   and r.run_item_id is null order by r.updated_at desc limit 1) r \
                 where t.project_id = $3 and t.id = any($4) and t.kind = 'code' \
                 and not (select sync_blocked from launches where id = $1) order by t.id",
            )
            .bind(launch_id)
            .bind(created)
            .bind(project_id)
            .bind(chunk)
            .fetch_all(&mut *conn)
            .await?;
            let mut items = Vec::with_capacity(rows.len());
            for r in &rows {
                items.push(item_of(r, Some(launch_id))?);
            }
            stats.add(sync_items(&self.pool, &ctx, &settings, &items).await?);
            sqlx::query("update case_sync_queue set lease_until = now() + interval '10 minutes' where launch_id = $1")
                .bind(launch_id)
                .execute(&mut *conn)
                .await?;
        }
        Ok((true, stats))
    }

    /// The `cases.sync` job: takes queued launches one at a time until the queue is empty or
    /// `budget` is spent. A failing launch is retried with a growing delay and dropped after
    /// [`MAX_SYNC_ATTEMPTS`].
    pub async fn run_case_sync(
        &self,
        owner: &str,
        limits: &TmLimits,
        budget: Duration,
    ) -> anyhow::Result<SyncStats> {
        let start = Instant::now();
        let mut total = SyncStats::default();
        let mut seen = HashSet::new();
        while start.elapsed() < budget {
            let Some(q) = self.claim_sync(owner).await? else {
                break;
            };
            if !seen.insert(q.launch_id) {
                sqlx::query("update case_sync_queue set lease_owner = null, lease_until = null where launch_id = $1")
                    .bind(q.launch_id)
                    .execute(&self.pool)
                    .await?;
                break;
            }
            match self.sync_launch(q.launch_id, limits).await {
                Ok((trusted, stats)) => {
                    tracing::debug!(launch = %q.launch_id, trusted, ?stats, "case sync done");
                    total.add(stats);
                    sqlx::query(
                        "delete from case_sync_queue where launch_id = $1 and lease_owner = $2",
                    )
                    .bind(q.launch_id)
                    .bind(owner)
                    .execute(&self.pool)
                    .await?;
                }
                Err(e) => {
                    let msg = format!("{e}");
                    if q.attempts >= MAX_SYNC_ATTEMPTS {
                        tracing::warn!(launch = %q.launch_id, error = %msg, "case sync dropped after repeated failures");
                        sqlx::query("delete from case_sync_queue where launch_id = $1")
                            .bind(q.launch_id)
                            .execute(&self.pool)
                            .await?;
                    } else {
                        tracing::warn!(launch = %q.launch_id, error = %msg, "case sync failed");
                        sqlx::query(
                            "update case_sync_queue set lease_owner = null, \
                             lease_until = now() + make_interval(mins => attempts), last_error = $2 where launch_id = $1",
                        )
                        .bind(q.launch_id)
                        .bind(msg.chars().take(2000).collect::<String>())
                        .execute(&self.pool)
                        .await?;
                    }
                }
            }
        }
        Ok(total)
    }
}

/// A [`SyncItem`] from a row with `id, full_name, explicit_id, explicit_id_source, name,
/// suite_path, labels, detail`.
pub fn item_of(r: &sqlx::postgres::PgRow, launch_id: Option<Uuid>) -> sqlx::Result<SyncItem> {
    let labels: Value = r.try_get("labels")?;
    let detail: Option<Value> = r.try_get("detail")?;
    let explicit: Option<String> = r.try_get("explicit_id")?;
    let explicit_src: Option<String> = r.try_get("explicit_id_source")?;
    let full_name: String = r.try_get("full_name")?;
    let suite_path: Option<Vec<String>> = r.try_get("suite_path")?;
    let name: Option<String> = r.try_get("name")?;
    Ok(SyncItem {
        test_case_id: r.try_get("id")?,
        data: code_data(
            &full_name,
            name.as_deref().unwrap_or(&full_name),
            suite_path.unwrap_or_default(),
            &labels,
            detail.as_ref(),
            explicit.as_deref().zip(explicit_src.as_deref()),
        ),
        launch_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tm::FieldOption;

    fn defs() -> Vec<CaseFieldDef> {
        let def = |key: &str, t: FieldType, opts: &[&str]| CaseFieldDef {
            id: Uuid::nil(),
            key: key.into(),
            name: key.into(),
            field_type: t,
            options: opts
                .iter()
                .map(|v| FieldOption {
                    value: v.to_string(),
                    color: None,
                    archived: false,
                })
                .collect(),
            required: false,
            default_value: FieldValue::Null,
            source_label: Some(key.into()),
            built_in: true,
            position: 0,
            archived: false,
        };
        vec![
            def("severity", FieldType::Select, &["critical", "normal"]),
            def("epic", FieldType::Text, &[]),
        ]
    }

    fn data() -> CodeData {
        CodeData {
            full_name: "shop.CartTest.adds".into(),
            name: "adds an item".into(),
            description: Some("From code".into()),
            links: vec![
                CaseLink {
                    name: "spec".into(),
                    url: "https://example.com/spec".into(),
                    link_type: CaseLinkType::Link,
                },
                CaseLink {
                    name: "x".into(),
                    url: "javascript:alert(1)".into(),
                    link_type: CaseLinkType::Link,
                },
            ],
            labels: vec![
                ("tag".into(), "smoke".into()),
                ("severity".into(), "Critical".into()),
                ("epic".into(), "Cart".into()),
                ("ALLURE_ID".into(), "77".into()),
            ],
            suite_path: vec!["Cart".into(), "Add".into()],
            allure_id: Some("77".into()),
            raw_links: Vec::new(),
        }
    }

    fn manual() -> CaseSnapshot {
        CaseSnapshot {
            name: "Manual name".into(),
            status: CaseStatus::Draft,
            folder_id: None,
            body: CaseBody {
                description: "Manual description".into(),
                preconditions: "pre".into(),
                steps: vec![super::super::CaseStep {
                    id: "s_1".into(),
                    action: "step".into(),
                    ..Default::default()
                }],
                links: vec![],
                parameters: None,
            },
            fields: BTreeMap::from([
                ("severity".into(), FieldValue::Text("normal".into())),
                ("priority".into(), FieldValue::Text("high".into())),
            ]),
            tags: vec!["manual".into()],
        }
    }

    #[test]
    fn code_values_respect_limits_and_options() {
        let v = code_values(&data(), &defs(), &TmLimits::default());
        assert_eq!(v.name, "adds an item");
        assert_eq!(v.links.len(), 1);
        assert_eq!(v.tags, vec!["smoke"]);
        assert_eq!(v.fields["severity"], FieldValue::Text("critical".into()));
        assert_eq!(v.fields["epic"], FieldValue::Text("Cart".into()));
        let mut d = data();
        d.labels.push(("severity".into(), "unknown".into()));
        d.labels
            .retain(|(n, v)| !(n == "severity" && v == "Critical"));
        assert!(
            !code_values(&d, &defs(), &TmLimits::default())
                .fields
                .contains_key("severity")
        );
        let mut d = data();
        d.name = "x".repeat(600);
        assert_eq!(
            code_values(&d, &defs(), &TmLimits::default()).name.len(),
            500
        );
    }

    /// Policy `code`/`case` × trusted/untrusted × create/update.
    #[test]
    fn policy_truth_table() {
        let defs = defs();
        let v = code_values(&data(), &defs, &TmLimits::default());
        for policy in [FieldPolicy::Code, FieldPolicy::Case] {
            let fields = CaseSyncFields {
                name: policy,
                description: policy,
                links: policy,
                tags: policy,
                severity: policy,
                epic: policy,
                ..Default::default()
            };
            for trusted in [true, false] {
                for create in [true, false] {
                    let cur = manual();
                    let plan = sync_plan(
                        trusted,
                        (!create).then_some(&cur),
                        &v,
                        &fields,
                        &defs,
                        false,
                    );
                    let case = format!("{policy:?} trusted={trusted} create={create}");
                    if !trusted {
                        assert!(plan.is_none(), "{case}");
                        continue;
                    }
                    let s = plan.unwrap();
                    if create || policy == FieldPolicy::Code {
                        assert_eq!(s.name, "adds an item", "{case}");
                        assert_eq!(s.body.description, "From code", "{case}");
                        assert_eq!(s.tags, vec!["smoke"], "{case}");
                        assert_eq!(
                            s.fields["severity"],
                            FieldValue::Text("critical".into()),
                            "{case}"
                        );
                        assert_eq!(s.body.links.len(), 1, "{case}");
                    } else {
                        assert_eq!(s.name, "Manual name", "{case}");
                        assert_eq!(s.body.description, "Manual description", "{case}");
                        assert_eq!(s.tags, vec!["manual"], "{case}");
                        assert_eq!(
                            s.fields["severity"],
                            FieldValue::Text("normal".into()),
                            "{case}"
                        );
                        assert!(s.body.links.is_empty(), "{case}");
                    }
                    if !create {
                        assert_eq!(s.body.steps, cur.body.steps, "{case}: steps stay");
                        assert_eq!(s.body.preconditions, "pre", "{case}");
                        assert_eq!(s.status, CaseStatus::Draft, "{case}: status stays");
                        assert_eq!(
                            s.fields["priority"],
                            FieldValue::Text("high".into()),
                            "{case}"
                        );
                    } else {
                        assert_eq!(s.status, CaseStatus::Active, "{case}");
                    }
                }
            }
        }
    }

    /// Empty or missing code values never clear the case; the first link of a case people
    /// wrote fills only what the case lacks.
    #[test]
    fn code_never_wipes_and_manual_text_wins_on_first_link() {
        let defs = defs();
        let fields = CaseSyncFields::default();
        let empty = CodeValues {
            name: "adds an item".into(),
            ..Default::default()
        };
        let s = synced_snapshot(Some(&manual()), &empty, &fields, &defs, false);
        assert_eq!(s.name, "adds an item", "name policy is code");
        assert_eq!(
            s.body.description, "Manual description",
            "empty description"
        );
        assert_eq!(s.tags, vec!["manual"], "no tags from code");
        assert_eq!(
            s.fields["severity"],
            FieldValue::Text("normal".into()),
            "no severity label"
        );
        let mut blank = empty.clone();
        blank.description = "  \n".into();
        blank
            .fields
            .insert("epic".into(), FieldValue::Text(" ".into()));
        let mut cur = manual();
        cur.fields
            .insert("epic".into(), FieldValue::Text("Shop".into()));
        let s = synced_snapshot(Some(&cur), &blank, &fields, &defs, false);
        assert_eq!(s.body.description, "Manual description");
        assert_eq!(s.fields["epic"], FieldValue::Text("Shop".into()));

        let v = code_values(&data(), &defs, &TmLimits::default());
        let s = synced_snapshot(Some(&manual()), &v, &fields, &defs, true);
        assert_eq!(s.name, "Manual name", "manual text wins on the first link");
        assert_eq!(s.body.description, "Manual description");
        assert_eq!(s.tags, vec!["manual"]);
        assert_eq!(s.fields["severity"], FieldValue::Text("normal".into()));
        assert_eq!(
            s.fields["epic"],
            FieldValue::Text("Cart".into()),
            "empty values are filled"
        );
        assert_eq!(s.body.links.len(), 1, "no links yet: filled");
        let mut bare = manual();
        bare.body.description.clear();
        bare.tags.clear();
        let s = synced_snapshot(Some(&bare), &v, &fields, &defs, true);
        assert_eq!(s.body.description, "From code");
        assert_eq!(s.tags, vec!["smoke"]);
        let s = synced_snapshot(Some(&manual()), &v, &fields, &defs, false);
        assert_eq!(s.name, "adds an item", "later syncs apply code values");
        assert_eq!(s.body.description, "From code");
    }

    #[test]
    fn trust_and_hash() {
        let s = CaseSyncSettings::default();
        let main = LaunchTrust {
            debug: false,
            branch: Some("main".into()),
            default_branch: None,
            can_sync: true,
        };
        assert!(launch_trusted(&s, &main));
        assert!(!launch_trusted(
            &s,
            &LaunchTrust {
                debug: true,
                ..main.clone()
            }
        ));
        assert!(!launch_trusted(
            &s,
            &LaunchTrust {
                can_sync: false,
                ..main.clone()
            }
        ));
        let feature = LaunchTrust {
            branch: Some("feature/x".into()),
            ..main.clone()
        };
        assert!(!launch_trusted(&s, &feature));
        assert!(launch_trusted(
            &s,
            &LaunchTrust {
                branch: Some("trunk".into()),
                default_branch: Some("trunk".into()),
                ..main.clone()
            }
        ));
        let off = CaseSyncSettings {
            enabled: false,
            ..CaseSyncSettings::default()
        };
        assert!(!launch_trusted(&off, &main));
        let v = code_values(&data(), &defs(), &TmLimits::default());
        let h = code_hash(&v, &s.fields);
        assert_eq!(h, code_hash(&v, &s.fields));
        let mut f = s.fields;
        f.name = FieldPolicy::Case;
        assert_ne!(h, code_hash(&v, &f));
    }

    #[test]
    fn folders_and_packages() {
        assert_eq!(
            package_of("com.shop.CartTest.adds").as_deref(),
            Some("com.shop")
        );
        assert_eq!(
            package_of("com.shop.CartTest#adds").as_deref(),
            Some("com.shop")
        );
        assert_eq!(package_of("adds"), None);
        let l = TmLimits::default();
        let mut s = CaseSyncSettings::default();
        assert_eq!(
            folder_names(&s, &data(), &l),
            vec!["Automated", "Cart", "Add"]
        );
        s.folder_from = FolderFrom::Package;
        assert_eq!(folder_names(&s, &data(), &l), vec!["Automated", "shop"]);
        s.folder_from = FolderFrom::None;
        assert_eq!(folder_names(&s, &data(), &l), vec!["Automated"]);
        let d = code_data(
            "a.B.c",
            "c",
            vec![],
            &serde_json::json!([{"name": "AS_ID", "value": "12"}, {"name": "tms", "value": "C42"}]),
            Some(
                &serde_json::json!({"description": {"text": "d"}, "links": [{"url": "https://example.com", "type": "tms", "name": "QA-1"}]}),
            ),
            None,
        );
        assert_eq!(d.allure_id.as_deref(), Some("12"));
        assert_eq!(d.description.as_deref(), Some("d"));
        assert_eq!(d.tms_keys(), vec!["C42", "QA-1"]);
        let d = code_data(
            "x",
            "x",
            vec![],
            &Value::Null,
            None,
            Some(("9", "allure_id")),
        );
        assert_eq!(d.allure_id.as_deref(), Some("9"));
        let d = code_data(
            "x",
            "x",
            vec![],
            &Value::Null,
            None,
            Some(("9", "ctrf_test_id")),
        );
        assert_eq!(d.allure_id, None);
    }
}
