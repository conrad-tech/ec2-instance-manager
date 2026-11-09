//! Deciding what to do about an EC2 `DiskUtilization` alert: which alerts
//! this watcher claims, which volume is the data volume, and how far it may
//! grow. Pure; the GUI owns every network hop.

use crate::alerts::Alert;
use crate::features::{GrafanaFeature, ReaperFeature, UnhealthyHostFeature};
use crate::{grafana, reaper, unhealthy_host};

/// Both must appear in the alert title.
pub const TITLE_MARKERS: [&str; 2] = ["[EC2]", "DiskUtilization"];

/// `true` when `hay` contains `needle`, case-insensitively. A blank needle
/// matches nothing (copied locally, as every watcher does).
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// Never claimed, whatever else the title says: Kafka disk alerts are a
/// different family with a different fix. A hard AND in code, not a config
/// value, like reaper's title word, so no rule can widen it.
pub const EXCLUDED_TITLE_WORD: &str = "kafka";

/// Does the title carry both markers, and not the Kafka word? The two
/// markers alone do NOT keep Kafka out: `[EC2]: dev1-Kafka-100-
/// DiskUtilization-Warning` carries both.
pub fn identifies(alert: &Alert) -> bool {
    TITLE_MARKERS.iter().all(|m| contains_ci(&alert.message, m))
        && !contains_ci(&alert.message, EXCLUDED_TITLE_WORD)
}

/// Ours to act on: identified, and not claimed by an older watcher.
pub fn claims(
    alert: &Alert,
    reaper_cfg: &ReaperFeature,
    unhealthy_cfg: &UnhealthyHostFeature,
    grafana_cfg: &GrafanaFeature,
) -> bool {
    identifies(alert)
        && !reaper::identifies(alert, reaper_cfg)
        && !unhealthy_host::identifies(alert, unhealthy_cfg)
        && !grafana::identifies(alert, grafana_cfg)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    pub id: String,
    pub size_gib: u64,
    pub volume_type: String,
    pub device: String,
}

/// The data volume: the larger of the instance's volumes. Refuses when that
/// cannot be told with confidence.
pub fn choose_volume(volumes: &[Volume], root_device: &str) -> Result<Volume, String> {
    if volumes.is_empty() {
        return Err("no volumes found on the instance".into());
    }
    if volumes.len() == 1 {
        return Err("only one volume is attached; refusing to guess it is the data volume".into());
    }
    let mut sorted: Vec<&Volume> = volumes.iter().collect();
    sorted.sort_by(|a, b| b.size_gib.cmp(&a.size_gib));
    if sorted[0].size_gib == sorted[1].size_gib {
        return Err("the two largest volumes are of equal size; cannot tell which is data".into());
    }
    if sorted[0].device == root_device {
        return Err("the largest volume is the root device; refusing to grow it".into());
    }
    Ok(sorted[0].clone())
}

/// Mounts treated as data filesystems when an instance has more than one
/// data volume. Overridable by `disk_grow.data_mounts`.
pub const DEFAULT_DATA_MOUNTS: [&str; 2] = ["/data", "/data2"];

/// A data filesystem is grown only when it is ABOVE this use. Overridable by
/// `disk_grow.grow_above_pct`.
pub const DEFAULT_GROW_ABOVE_PCT: u32 = 79;

/// The `Use%` of `mount`'s row in `df -h` output.
pub fn use_pct_for_mount(df_h: &str, mount: &str) -> Option<u32> {
    let row = df_h
        .lines()
        .skip(1)
        .find(|l| l.split_whitespace().last() == Some(mount))?;
    row.split_whitespace()
        .find_map(|w| w.strip_suffix('%'))
        .and_then(|n| n.parse().ok())
}

/// The alert's facts, with the instance id taken from the `InstanceId`
/// property (and the account and environment from their tags) when the
/// description yielded none. The description wins whenever
/// it has a readable id; the property is only a fallback, and goes through
/// the same whitelist (`reaper::find_instance_id`) before it reaches argv.
/// A tag value, trimmed, or `None` when blank or an unrendered template.
fn usable(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty() && !v.contains(['{', '}', '%', '<', '>'])).then(|| v.to_string())
}

pub fn facts_with_instance_and_account(
    mut facts: crate::instance_age::AlertFacts,
    alert: &Alert,
) -> crate::instance_age::AlertFacts {
    if facts.instance_id.is_empty() {
        if let Some((_, v)) = alert
            .extra
            .iter()
            .find(|(k, _)| !k.contains("{{") && k.trim().eq_ignore_ascii_case("InstanceId"))
        {
            if let Some(id) = reaper::find_instance_id(v) {
                facts.instance_id = id;
            }
        }
    }
    // The account, the same way: only when the description gave none. It is a tag on the alert (`Account: <id>`),
    // already split out as `alert.account`. Unrendered templates are refused.
    if facts.account.is_empty() && facts.account_id.is_empty() {
        if let Some(v) = usable(&alert.account) {
            let (name, id) = crate::instance_age::split_account(&v);
            facts.account = name;
            facts.account_id = id;
        }
    }
    facts
}

/// `facts_with_instance_and_account`, plus the environment from its tag.
/// Only the disk-utilization watcher uses the environment fallback: it is
/// what an exclusion list is matched against, so the other watchers (whose
/// behaviour must not change for alerts they decline today) do not get it.
pub fn facts_with_property(
    facts: crate::instance_age::AlertFacts,
    alert: &Alert,
) -> crate::instance_age::AlertFacts {
    let mut facts = facts_with_instance_and_account(facts, alert);
    if facts.environment.trim().is_empty() {
        if let Some(v) = usable(&alert.environment) {
            facts.environment = v;
        }
    }
    facts
}

/// The mount the alert itself names (its `mountpoint` property, e.g.
/// `/data`). `None` when absent, unrendered or not a plain absolute path.
pub fn alert_mount(alert: &Alert) -> Option<String> {
    let (_, v) = alert
        .extra
        .iter()
        .find(|(k, _)| !k.contains("{{") && k.trim().eq_ignore_ascii_case("mountpoint"))?;
    let m = v.trim();
    (safe_mount(m)).then(|| m.to_string())
}

/// An alert whose volume is never grown: the title carries a
/// `no_grow_title_words` entry (reaper), or the alert names the root
/// filesystem (`/`). Both get the same handling: compress the logs, wait
/// `no_grow_wait` for the alert to close, escalate if it has not.
pub fn never_grown(cfg: &crate::features::DiskGrowFeature, alert: &Alert) -> bool {
    cfg.no_grow(&alert.message) || alert_mount(alert).as_deref() == Some("/")
}

/// The volumes to grow.
///
/// * When the alert names its mount (`alert_mount`): exactly the non-root
///   volume mounted there, whatever its use now (the alert is the evidence,
///   and the log compression that runs first may have freed space). The
///   root mount `/` is refused outright.
/// * One data volume (the instance has two volumes): the larger one, as
///   `choose_volume` picks it, and only when its filesystem is above
///   `above_pct`. A data volume the box cannot resolve or read a use% for is
///   still returned: the grow chain reports that failure itself.
/// * More than one (three or more volumes, e.g. `/data` and `/data2`): every
///   non-root volume mounted at one of `data_mounts` whose filesystem is
///   above `above_pct`. Either one, or both when both are. None above the
///   threshold is an error, not a quiet success: the alert says a disk is
///   full and this cannot find it.
pub fn choose_targets(
    volumes: &[Volume],
    root_device: &str,
    probe: &Probe,
    data_mounts: &[String],
    above_pct: u32,
    alert_mount: Option<&str>,
) -> Result<Vec<Volume>, String> {
    if let Some(mount) = alert_mount {
        if mount == "/" {
            return Err("the alert names the root filesystem (/); it is never grown".into());
        }
        for v in volumes.iter().filter(|v| v.device != root_device) {
            if let Ok(dev) = resolve_data_device(probe, &v.id) {
                if dev.mount == mount {
                    return Ok(vec![v.clone()]);
                }
            }
        }
        return Err(format!("the alert names {mount}, but no non-root xfs volume is mounted there"));
    }
    if volumes.len() <= 2 {
        let v = choose_volume(volumes, root_device)?;
        if let Ok(dev) = resolve_data_device(probe, &v.id) {
            if let Some(pct) = use_pct_for_mount(&probe.df_h, &dev.mount) {
                if pct <= above_pct {
                    return Err(format!(
                        "the data filesystem {} is at {pct}%, not above {above_pct}%",
                        dev.mount
                    ));
                }
            }
        }
        return Ok(vec![v]);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut over: Vec<Volume> = Vec::new();
    for v in volumes.iter().filter(|v| v.device != root_device) {
        let Ok(dev) = resolve_data_device(probe, &v.id) else { continue };
        if !data_mounts.iter().any(|m| m == &dev.mount) {
            continue;
        }
        let Some(pct) = use_pct_for_mount(&probe.df_h, &dev.mount) else {
            return Err(format!("no df -h use% for {}", dev.mount));
        };
        seen.push(format!("{} {pct}%", dev.mount));
        if pct > above_pct {
            over.push(v.clone());
        }
    }
    if seen.is_empty() {
        return Err(format!(
            "no non-root volume is an xfs mount at one of {}",
            data_mounts.join(", ")
        ));
    }
    if over.is_empty() {
        return Err(format!("no data filesystem is above {above_pct}% ({})", seen.join(", ")));
    }
    over.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(over)
}

/// The Scripts-menu check offers a grow for a filesystem at this use% or
/// more (the watcher's own threshold is "above 79", the same line).
pub const SCRIPT_AT_LEAST_PCT: u32 = 80;

/// What the Scripts-menu check offers by default: +1 TiB.
pub const SCRIPT_DEFAULT_ADD_GIB: u64 = 1024;

/// One non-root xfs volume as the Scripts-menu check found it.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptRow {
    pub volume: Volume,
    pub mount: String,
    pub use_pct: u32,
    /// The `df -h` header and this mount's row, as the report shows it.
    pub df: String,
    pub fs_bytes: u64,
}

impl ScriptRow {
    /// Whether this volume is offered a grow.
    pub fn needs_grow(&self) -> bool {
        self.use_pct >= SCRIPT_AT_LEAST_PCT
    }
}

/// The check's answer: every non-root volume it could read, plus a line for
/// each one it could not (never silently dropped).
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptCheck {
    pub rows: Vec<ScriptRow>,
    pub skipped: Vec<String>,
}

/// Every non-root volume mounted as xfs with its use%. A volume holding `/`
/// is never a row (`resolve_data_device` refuses any disk or partition that
/// does), whatever device name the instance reports it under.
pub fn script_check(volumes: &[Volume], root_device: &str, probe: &Probe) -> ScriptCheck {
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for v in volumes.iter().filter(|v| v.device != root_device) {
        let dev = match resolve_data_device(probe, &v.id) {
            Ok(d) => d,
            Err(e) => {
                skipped.push(format!("{} ({}): {e}", v.id, v.device));
                continue;
            }
        };
        let (Some(use_pct), Some(df), Some(fs_bytes)) = (
            use_pct_for_mount(&probe.df_h, &dev.mount),
            df_h_for_mount(&probe.df_h, &dev.mount),
            fs_bytes_for_mount(probe, &dev.mount),
        ) else {
            skipped.push(format!("{} at {}: the box gave no usable df -h / size row", v.id, dev.mount));
            continue;
        };
        rows.push(ScriptRow { volume: v.clone(), mount: dev.mount, use_pct, df, fs_bytes });
    }
    rows.sort_by(|a, b| a.mount.cmp(&b.mount));
    ScriptCheck { rows, skipped }
}

/// The size a typed "add this many GiB" gives `volume`, or why it is refused.
/// Blank, non-numeric, negative and zero are refused with a sentence naming
/// the box; a result over the volume type's maximum is refused too.
pub fn script_new_size(volume: &Volume, typed: &str) -> Result<u64, String> {
    let t = typed.trim();
    if t.is_empty() {
        return Err("enter how many GiB to add".into());
    }
    if t.starts_with('-') {
        return Err("the amount to add cannot be negative".into());
    }
    let add: u64 = t.parse().map_err(|_| "the amount to add must be a whole number of GiB".to_string())?;
    if add == 0 {
        return Err("the amount to add must be at least 1 GiB".into());
    }
    let max = max_gib(&volume.volume_type);
    match volume.size_gib.checked_add(add) {
        Some(n) if n <= max => Ok(n),
        _ => Err(format!(
            "{} GiB + {add} GiB is over the {} maximum of {max} GiB",
            volume.size_gib, volume.volume_type
        )),
    }
}

/// Largest size, in GiB, the volume type allows.
pub fn max_gib(volume_type: &str) -> u64 {
    match volume_type {
        "gp2" | "gp3" | "io1" | "io2" => 65536,
        "st1" | "sc1" => 16384,
        "standard" => 1024,
        _ => 16384,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrowthKind {
    OneTb,
    TenPercent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Growth {
    pub new_gib: u64,
    pub kind: GrowthKind,
}

/// The sizes offered: +1 TiB, then +10% (rounded up), each only if it fits.
pub fn growth_options(volume: &Volume) -> Vec<Growth> {
    let max = max_gib(&volume.volume_type);
    let size = volume.size_gib;
    let mut out = Vec::new();
    let tb = size + 1024;
    if tb <= max {
        out.push(Growth { new_gib: tb, kind: GrowthKind::OneTb });
    }
    let pct = size + size.div_ceil(10);
    if pct <= max {
        out.push(Growth { new_gib: pct, kind: GrowthKind::TenPercent });
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifyError {
    Refused,
    RateLimited,
    Ambiguous,
}

/// Reads `modify-volume` stderr. Rate limit is checked first; anything
/// unrecognised (including empty) is ambiguous.
pub fn classify_modify_error(stderr: &str) -> ModifyError {
    let s = stderr.to_ascii_lowercase();
    if s.contains("volumemodificationrateexceeded")
        || s.contains("within the last 6 hours")
        || s.contains("in the last 6 hours")
        || s.contains("6 hours")
    {
        return ModifyError::RateLimited;
    }
    if s.contains("invalidparametervalue")
        || s.contains("volumelimitexceeded")
        || s.contains("exceeds")
        || s.contains("maximum")
        || s.contains("greater than")
    {
        return ModifyError::Refused;
    }
    ModifyError::Ambiguous
}

pub const PROBE_SCRIPT: &str = include_str!("../assets/scripts/disk_grow_probe.sh");
pub const FS_SCRIPT: &str = include_str!("../assets/scripts/disk_grow_fs.sh");
pub const GZIP_SCRIPT: &str = include_str!("../assets/scripts/disk_grow_gzip.sh");

/// The gzip script with its mode set. `dry` only counts what it would
/// compress.
pub fn gzip_command(dry: bool) -> String {
    format!("RE_DG_DRY='{}'\n{}", u8::from(dry), GZIP_SCRIPT)
}

/// What the gzip script reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GzipResult {
    pub files: u64,
    pub bytes: u64,
    pub failed: u64,
    pub dry: bool,
    /// Files the clean-up steps emptied or removed (docker logs, /tmp, old
    /// `*.log`), and the bytes they held. Dry: what would be.
    pub cleaned: u64,
    pub cleaned_bytes: u64,
    /// The journal vacuum: `ok`, `failed`, `none` (no journalctl) or `skipped`.
    pub journal: String,
}

impl GzipResult {
    /// One sentence for a log line or report.
    pub fn summary(&self) -> String {
        let verb = if self.dry { "would be" } else { "was" };
        format!(
            "{} old log file(s), {} bytes, {verb} gzipped ({} failed); {} file(s), {} bytes, {verb} \
             emptied or removed (docker logs, /tmp, old *.log); journal vacuum: {}",
            self.files, self.bytes, self.failed, self.cleaned, self.cleaned_bytes, self.journal
        )
    }
}

/// Reads the script's marker line. No marker is an error.
pub fn parse_gzip(output: &str) -> Result<GzipResult, String> {
    let line = output
        .lines()
        .find(|l| l.starts_with("__DG_GZ__"))
        .ok_or_else(|| "the log compression reported no result".to_string())?;
    let field = |name: &str| -> Result<u64, String> {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("the log compression result has no {name}"))
    };
    Ok(GzipResult {
        files: field("files")?,
        bytes: field("bytes")?,
        failed: field("failed")?,
        dry: field("dry")? == 1,
        // Absent from a reply by an older script: nothing was cleaned.
        cleaned: field("cleaned").unwrap_or(0),
        cleaned_bytes: field("cleaned_bytes").unwrap_or(0),
        journal: line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("journal="))
            .unwrap_or("none")
            .to_string(),
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockDev {
    pub name: String,
    pub dev_type: String,
    pub serial: String,
    pub fstype: String,
    pub mountpoint: String,
    pub pkname: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub df_h: String,
    pub fs_bytes: Vec<(String, u64)>,
    pub blocks: Vec<BlockDev>,
}

/// Parses one `lsblk -P` line: `KEY="value" KEY="value"`.
fn parse_lsblk_line(line: &str) -> Option<BlockDev> {
    let mut dev = BlockDev::default();
    let mut rest = line.trim();
    let mut any = false;
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim();
        let after = &rest[eq + 2..];
        let end = after.find('"')?;
        let val = &after[..end];
        match key {
            "NAME" => dev.name = val.to_string(),
            "TYPE" => dev.dev_type = val.to_string(),
            "SERIAL" => dev.serial = val.to_string(),
            "FSTYPE" => dev.fstype = val.to_string(),
            "MOUNTPOINT" => dev.mountpoint = val.to_string(),
            "PKNAME" => dev.pkname = val.to_string(),
            "SIZE" => dev.size_bytes = val.trim().parse().unwrap_or(0),
            _ => {}
        }
        any = true;
        rest = after[end + 1..].trim_start();
    }
    if any && !dev.name.is_empty() { Some(dev) } else { None }
}

/// Reads the probe script's output. Any missing marker or section is an
/// error, never an empty success.
pub fn parse_probe(output: &str) -> Result<Probe, String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Sec {
        None,
        Df,
        Bytes,
        Lsblk,
    }
    let (mut begun, mut ended) = (false, false);
    let (mut saw_df, mut saw_bytes, mut saw_lsblk) = (false, false, false);
    let mut sec = Sec::None;
    let mut probe = Probe::default();
    let mut df_lines: Vec<&str> = Vec::new();
    let mut bytes_header_skipped = false;
    for line in output.lines() {
        let t = line.trim_end();
        match t {
            "__DG_BEGIN__" => {
                begun = true;
                continue;
            }
            "__DG_END__" => {
                ended = true;
                sec = Sec::None;
                continue;
            }
            "__DG_SEC__ DFH" => {
                sec = Sec::Df;
                saw_df = true;
                continue;
            }
            "__DG_SEC__ BYTES" => {
                sec = Sec::Bytes;
                saw_bytes = true;
                continue;
            }
            "__DG_SEC__ LSBLK" => {
                sec = Sec::Lsblk;
                saw_lsblk = true;
                continue;
            }
            _ => {}
        }
        if !begun || t.trim().is_empty() {
            continue;
        }
        match sec {
            Sec::Df => df_lines.push(t),
            Sec::Bytes => {
                if !bytes_header_skipped {
                    bytes_header_skipped = true;
                    continue;
                }
                let mut it = t.rsplitn(2, char::is_whitespace);
                let size = it.next().and_then(|n| n.parse::<u64>().ok());
                let target = it.next().map(str::trim);
                if let (Some(size), Some(target)) = (size, target) {
                    probe.fs_bytes.push((target.to_string(), size));
                }
            }
            Sec::Lsblk => {
                if let Some(d) = parse_lsblk_line(t) {
                    probe.blocks.push(d);
                }
            }
            Sec::None => {}
        }
    }
    if !begun || !ended || !saw_df || !saw_bytes || !saw_lsblk {
        return Err("probe output is incomplete (missing marker or section)".into());
    }
    if probe.blocks.is_empty() {
        return Err("probe listed no block devices".into());
    }
    probe.df_h = df_lines.join("\n");
    Ok(probe)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DataDevice {
    pub disk: String,
    pub partition: Option<String>,
    pub mount: String,
    pub device_bytes: u64,
}

/// Finds the XFS mount on the volume (matched by its serial, which is the
/// volume id without the dash). Refuses anything it cannot grow safely.
pub fn resolve_data_device(probe: &Probe, volume_id: &str) -> Result<DataDevice, String> {
    let serial = volume_id.replace('-', "");
    let disk = probe
        .blocks
        .iter()
        .find(|b| b.dev_type == "disk" && !b.serial.is_empty() && b.serial == serial)
        .ok_or_else(|| format!("no disk on the box has serial {serial}"))?;
    let children: Vec<&BlockDev> = probe.blocks.iter().filter(|b| b.pkname == disk.name).collect();
    let nested = probe
        .blocks
        .iter()
        .filter(|b| children.iter().any(|c| c.name == b.pkname))
        .chain(children.iter().copied());
    for b in nested {
        if b.dev_type == "lvm" || b.dev_type.starts_with("raid") {
            let kind = if b.dev_type == "lvm" { "lvm" } else { "raid" };
            return Err(format!("{} sits on {kind} ({}); refusing to grow it", disk.name, b.name));
        }
    }
    // The root filesystem is never grown, whatever the EBS device names say
    // about which volume is root: a disk (or any partition of it) holding `/`
    // is refused outright.
    if disk.mountpoint == "/" || children.iter().any(|c| c.mountpoint == "/") {
        return Err(format!("{} holds the root filesystem (/); refusing to grow it", disk.name));
    }
    let has_mount = |b: &BlockDev| b.mountpoint.starts_with('/');
    let (holder, partition) = if has_mount(disk) {
        (disk, None)
    } else if let Some(c) = children.iter().find(|c| c.dev_type == "part" && has_mount(c)) {
        (*c, Some(c.name.clone()))
    } else {
        return Err(format!("{} is not mounted", disk.name));
    };
    if !holder.fstype.eq_ignore_ascii_case("xfs") {
        return Err(format!(
            "{} is mounted at {} but is not xfs (found '{}')",
            holder.name, holder.mountpoint, holder.fstype
        ));
    }
    Ok(DataDevice {
        disk: disk.name.clone(),
        partition,
        mount: holder.mountpoint.clone(),
        device_bytes: disk.size_bytes,
    })
}

/// The `df -h` header and the row for `mount`.
pub fn df_h_for_mount(df_h: &str, mount: &str) -> Option<String> {
    let mut lines = df_h.lines();
    let header = lines.next()?;
    let row = lines.find(|l| l.split_whitespace().last() == Some(mount))?;
    Some(format!("{header}\n{row}"))
}

/// An alert environment nothing can be decided from: blank, or carrying a
/// template character (`{ } % < >`). This feed has been seen serving
/// unrendered `{{…}}` and `&{%…%}%` where values should be, and either could
/// be an excluded environment.
pub fn environment_unreadable(env: &str) -> bool {
    let t = env.trim();
    t.is_empty() || t.contains(['{', '}', '%', '<', '>'])
}

/// What a filesystem grow attempt should do, from a fresh probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsPlan {
    /// The device is ahead of its filesystem: grow it.
    Grow,
    /// Not ahead, but the filesystem is larger than it was before the
    /// incident: a grow already landed and its answer was lost.
    AlreadyGrown,
    /// Not ahead and not larger: the new size has not reached the box yet.
    NotYet,
}

pub fn fs_plan(device_bytes: u64, fs_now: u64, fs_recorded: u64) -> FsPlan {
    if device_ahead_of_fs(device_bytes, fs_now) {
        FsPlan::Grow
    } else if fs_now > fs_recorded {
        FsPlan::AlreadyGrown
    } else {
        FsPlan::NotYet
    }
}

/// An already-acknowledged alert whose filesystem fills its device: is a
/// modification under way that this app (before a restart) started?
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Modifying, optimizing or an unrecognised state: track it as an
    /// incident (never resizing) so the filesystem is grown when it is
    /// usable, or it escalates when it is not.
    Track,
    /// Nothing of ours to finish.
    Nothing(String),
    /// Escalate.
    Failed(String),
}

/// Whether the poll's Full path may resize, read from the volume's latest
/// modification. Only a finished one (completed or failed), or none at all,
/// allows it; a modification in flight (or a state this cannot name) means
/// a grow is already under way -- resizing again would race it and be
/// refused by AWS, which pages falsely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResizeGate {
    Resize,
    InFlight,
    Unreadable(String),
}

pub fn resize_gate(state: &Result<ModState, String>) -> ResizeGate {
    match state {
        Ok(ModState::Completed | ModState::Failed) => ResizeGate::Resize,
        Err(e) if e == NO_MODIFICATION => ResizeGate::Resize,
        Ok(ModState::Modifying | ModState::Optimizing | ModState::Unknown(_)) => ResizeGate::InFlight,
        Err(e) => ResizeGate::Unreadable(e.clone()),
    }
}

/// Acknowledged by anyone: the flag, or JSM's third status `acked` (some
/// replies carry only one of the two).
pub fn alert_acknowledged(alert: &Alert) -> bool {
    alert.acknowledged || alert.status.trim().eq_ignore_ascii_case("acked")
}

/// What recovery on an already-acknowledged alert (or a Full path that
/// found a modification in flight) should do.
///
/// Called only when the device is NOT ahead of its filesystem, so the
/// filesystem already fills whatever size the box shows. Optimizing or
/// Completed is therefore ours to finish only while the box still shows
/// less than the volume's size (`device_bytes < volume_gib` GiB): once it
/// shows the full size, the filesystem was already grown (before a restart,
/// or by hand) and tracking it would wait for a grow that cannot come and
/// page falsely. A Modifying volume cannot yet have a grown filesystem.
pub fn recovery(state: &Result<ModState, String>, device_bytes: u64, volume_gib: u64) -> Recovery {
    let box_lags = device_bytes < volume_gib.saturating_mul(1024 * 1024 * 1024);
    match state {
        Ok(ModState::Modifying | ModState::Unknown(_)) => Recovery::Track,
        Ok(ModState::Optimizing | ModState::Completed) if box_lags => Recovery::Track,
        Ok(ModState::Optimizing | ModState::Completed) => Recovery::Nothing(
            "the box already shows the volume's full size and the filesystem fills it".into(),
        ),
        // The latest-ever modification, possibly months old, and not one
        // this run started: nothing to finish. Never a page or a 6h hold;
        // the caller logs one Warn.
        Ok(ModState::Failed) => Recovery::Nothing(
            "the volume's last modification failed (not one this run started)".into(),
        ),
        Err(e) if e == NO_MODIFICATION => {
            Recovery::Nothing("the volume has never been modified".into())
        }
        Err(e) => Recovery::Failed(format!("could not read the modification state: {e}")),
    }
}

pub fn fs_bytes_for_mount(probe: &Probe, mount: &str) -> Option<u64> {
    probe.fs_bytes.iter().find(|(m, _)| m == mount).map(|(_, b)| *b)
}

/// True when the device is meaningfully larger than its filesystem, i.e. a
/// resize landed but the filesystem was never grown.
///
/// A fully grown XFS filesystem is still smaller than its device: the log and
/// metadata take a little, about 2 GiB on a 16 TB volume (a real box showed
/// 17177732186112 bytes of filesystem on a 17179869184000 byte device). So
/// the margin is the larger of 512 MiB and 0.5% of the device. The smallest
/// grow this app asks for is +10%, far past that.
pub fn device_ahead_of_fs(device_bytes: u64, fs_bytes: u64) -> bool {
    let margin = (512 * 1024 * 1024u64).max(device_bytes / 200);
    device_bytes.saturating_sub(fs_bytes) >= margin
}

fn safe_device_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn safe_mount(s: &str) -> bool {
    s.starts_with('/')
        && !s.contains("..")
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

/// The three assignments followed by the filesystem script, for sending to
/// the box. Inputs are whitelisted, not escaped.
pub fn fs_grow_command(dev: &DataDevice) -> Result<String, String> {
    if !safe_device_name(&dev.disk) {
        return Err(format!("refusing unsafe disk name '{}'", dev.disk));
    }
    let part = dev.partition.as_deref().unwrap_or("");
    if !part.is_empty() && !safe_device_name(part) {
        return Err(format!("refusing unsafe partition name '{part}'"));
    }
    if !safe_mount(&dev.mount) {
        return Err(format!("refusing unsafe mount point '{}'", dev.mount));
    }
    Ok(format!(
        "RE_DG_DISK='{}'\nRE_DG_PART='{}'\nRE_DG_MOUNT='{}'\n{}",
        dev.disk, part, dev.mount, FS_SCRIPT
    ))
}

#[derive(Clone, Debug, PartialEq)]
pub enum FsGrow {
    Grown { df_after: String },
    Failed(String),
}

/// Reads the filesystem script's verdict. No marker is a failure.
pub fn parse_fs_grow(output: &str) -> FsGrow {
    let lines: Vec<&str> = output.lines().collect();
    if let Some(i) = lines.iter().position(|l| l.starts_with("__DG_FAIL__")) {
        let first = lines[i].trim_start_matches("__DG_FAIL__").trim();
        let mut msg = first.to_string();
        for l in &lines[i + 1..] {
            msg.push('\n');
            msg.push_str(l);
        }
        return FsGrow::Failed(msg.trim().to_string());
    }
    if let Some(i) = lines.iter().position(|l| l.trim_end() == "__DG_OK__") {
        return FsGrow::Grown { df_after: lines[i + 1..].join("\n").trim().to_string() };
    }
    FsGrow::Failed("no verdict marker in the output; the grow did not report back".into())
}

pub fn alert_comment(before: &str, after: &str) -> String {
    format!("Before:\n{before}\n\nAfter:\n{after}")
}

// ---- Incident state machine ------------------------------------------------

/// AWS's modification state for a volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModState {
    Modifying,
    Optimizing,
    Completed,
    Failed,
    Unknown(String),
}

/// The `Err` text when a volume has no modification on record.
pub const NO_MODIFICATION: &str = "no volume modification was reported";

/// Reads `VolumesModifications[0].ModificationState`. No entry is an `Err`.
pub fn parse_mod_state(json: &str) -> Result<ModState, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("unreadable modification reply: {e}"))?;
    let state = v
        .get("VolumesModifications")
        .and_then(|m| m.get(0))
        .and_then(|m| m.get("ModificationState"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| NO_MODIFICATION.to_string())?;
    Ok(match state {
        "modifying" => ModState::Modifying,
        "optimizing" => ModState::Optimizing,
        "completed" => ModState::Completed,
        "failed" => ModState::Failed,
        other => ModState::Unknown(other.to_string()),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Modifying,
    Optimizing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Wait,
    GrowFilesystem,
    Complete,
    Escalate(String),
    StopFlashing(String),
}

/// What to do next for one incident. Reasons carry no ids.
pub fn transition(
    phase: Phase,
    mod_state: &ModState,
    fs_grown: bool,
    elapsed_ms: u64,
    usable_wait_ms: u64,
    optimize_ceiling_ms: u64,
) -> Transition {
    match mod_state {
        ModState::Failed => Transition::Escalate("the volume modification failed".into()),
        ModState::Optimizing | ModState::Completed if !fs_grown => Transition::GrowFilesystem,
        ModState::Completed => Transition::Complete,
        ModState::Optimizing => {
            if elapsed_ms >= optimize_ceiling_ms {
                Transition::StopFlashing(
                    "the volume is still optimizing; the disk is already grown".into(),
                )
            } else {
                Transition::Wait
            }
        }
        ModState::Modifying | ModState::Unknown(_) => {
            if phase == Phase::Modifying && elapsed_ms >= usable_wait_ms {
                Transition::Escalate(
                    "the volume did not become usable within the expected time".into(),
                )
            } else {
                Transition::Wait
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incident {
    pub instance: String,
    pub owner_alert: String,
    pub created_at: String,
    pub volume_id: String,
    pub df_before: String,
    pub mount: String,
    pub started_ms: u64,
    pub phase: Phase,
    pub fs_grown: bool,
    /// The filesystem's size in bytes before any grow, so a retry can tell
    /// a grow whose answer was lost from one that never happened.
    pub fs_bytes_before: u64,
    /// What the alert said about the box (account, region, instance), kept
    /// so every later poll resolves the same AWS context the owner did.
    pub facts: crate::instance_age::AlertFacts,
}

impl Incident {
    /// One incident per volume: an instance with two data volumes runs two.
    pub fn key(&self) -> String {
        format!("{}|{}", self.instance, self.volume_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Begin {
    Owner,
    Duplicate { owner: String },
    Held,
    /// A fix for this instance started within the cooldown window.
    Cooling,
}

/// A failed instance keeps its slot for AWS's 6-hour modification window.
pub const FAILED_HOLD_MS: u64 = 6 * 60 * 60 * 1000;

/// Owned by one poll thread; no locks.
#[derive(Default)]
pub struct DiskGrowState {
    seen: std::collections::HashSet<String>,
    running: std::collections::HashMap<String, Incident>,
    failed: std::collections::HashMap<String, u64>,
    /// When the last fix for an instance started (`monotonic_ms`).
    cooled: std::collections::HashMap<String, u64>,
}

impl DiskGrowState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_seen(&self, alert_id: &str) -> bool {
        self.seen.contains(alert_id)
    }

    pub fn mark_seen(&mut self, alert_id: &str) {
        self.seen.insert(alert_id.to_string());
    }

    pub fn begin(&mut self, instance: &str, _alert_id: &str, now_ms: u64) -> Begin {
        if let Some(inc) = self.running.values().find(|i| i.instance == instance) {
            return Begin::Duplicate { owner: inc.owner_alert.clone() };
        }
        if let Some(&at) = self.failed.get(instance) {
            if now_ms.saturating_sub(at) < FAILED_HOLD_MS {
                return Begin::Held;
            }
            self.failed.remove(instance);
        }
        Begin::Owner
    }

    /// Records that a fix for `instance` started now. Whatever its outcome
    /// (grown, escalated, closed, timed out), a new alert for the instance
    /// inside the cooldown is not acted on.
    pub fn cool(&mut self, instance: &str, now_ms: u64) {
        self.cooled.insert(instance.to_string(), now_ms);
    }

    /// True when a fix for `instance` started less than `window_ms` ago.
    pub fn cooling(&self, instance: &str, now_ms: u64, window_ms: u64) -> bool {
        self.cooled
            .get(instance)
            .is_some_and(|&at| now_ms.saturating_sub(at) < window_ms)
    }

    pub fn start(&mut self, incident: Incident) {
        self.running.insert(incident.key(), incident);
    }

    pub fn incidents(&self) -> Vec<Incident> {
        let mut v: Vec<Incident> = self.running.values().cloned().collect();
        v.sort_by(|a, b| a.key().cmp(&b.key()));
        v
    }

    /// Marks the incident `key` (`Incident::key`) filesystem-grown.
    pub fn fs_grown(&mut self, key: &str) {
        if let Some(i) = self.running.get_mut(key) {
            i.fs_grown = true;
        }
    }

    pub fn set_phase(&mut self, key: &str, phase: Phase) {
        if let Some(i) = self.running.get_mut(key) {
            i.phase = phase;
        }
    }

    /// Ends the one incident `key`.
    pub fn finish(&mut self, key: &str) {
        self.running.remove(key);
    }

    /// True while any incident for `instance` is still running.
    pub fn instance_busy(&self, instance: &str) -> bool {
        self.running.values().any(|i| i.instance == instance)
    }

    /// True while the incident `key` is running.
    pub fn is_running(&self, key: &str) -> bool {
        self.running.contains_key(key)
    }

    /// Ends the incident `key` and holds the instance's slot from `now_ms`.
    /// A sibling incident on another volume of the same instance carries on:
    /// dropping it would leave its volume grown under a small filesystem.
    pub fn failed(&mut self, key: &str, instance: &str, now_ms: u64) {
        self.running.remove(key);
        self.failed.insert(instance.to_string(), now_ms);
    }

    pub fn expire_failed(&mut self, now_ms: u64) {
        self.failed.retain(|_, at| now_ms.saturating_sub(*at) < FAILED_HOLD_MS);
    }

    pub fn owner_of(&self, instance: &str) -> Option<&str> {
        self.running.values().find(|i| i.instance == instance).map(|i| i.owner_alert.as_str())
    }

    pub fn growing_keys(&self) -> Vec<String> {
        let mut v: Vec<String> = self.running.values().map(|i| i.instance.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(title: &str) -> Alert {
        Alert { message: title.to_string(), ..Default::default() }
    }

    fn vol(id: &str, size: u64, ty: &str, dev: &str) -> Volume {
        Volume { id: id.into(), size_gib: size, volume_type: ty.into(), device: dev.into() }
    }

    #[test]
    fn identifies_needs_both_markers() {
        assert!(identifies(&alert("[EC2]: dev1-Cassandra-100-DiskUtilization-Warning")));
        assert!(identifies(&alert("[EC2]: dev1-Cassandra-100-DiskUtilization-Critical")));
        assert!(!identifies(&alert("[Kafka]: x-DiskUtilization-Warning")));
        assert!(!identifies(&alert("[EC2]: x-CPUUtilization-Warning")));
        assert!(identifies(&alert("[ec2]: x-diskutilization-warning")));
    }

    #[test]
    fn an_ec2_kafka_disk_alert_is_never_identified() {
        // Both markers present: only the hard-coded Kafka word keeps it out.
        assert!(!identifies(&alert("[EC2]: dev1-Kafka-100-DiskUtilization-Warning")));
        assert!(!identifies(&alert("[EC2]: prod-KAFKA-broker-DiskUtilization-Critical")));
    }

    #[test]
    fn a_resize_waits_on_any_modification_in_flight() {
        assert_eq!(resize_gate(&Ok(ModState::Completed)), ResizeGate::Resize);
        assert_eq!(resize_gate(&Ok(ModState::Failed)), ResizeGate::Resize);
        assert_eq!(resize_gate(&Err(NO_MODIFICATION.to_string())), ResizeGate::Resize);
        assert_eq!(resize_gate(&Ok(ModState::Modifying)), ResizeGate::InFlight);
        assert_eq!(resize_gate(&Ok(ModState::Optimizing)), ResizeGate::InFlight);
        assert_eq!(resize_gate(&Ok(ModState::Unknown("x".into()))), ResizeGate::InFlight);
        assert!(matches!(resize_gate(&Err("AccessDenied".into())), ResizeGate::Unreadable(_)));
    }

    #[test]
    fn acked_status_counts_as_acknowledged() {
        let with = |status: &str, acknowledged: bool| Alert {
            status: status.into(),
            acknowledged,
            ..Default::default()
        };
        assert!(alert_acknowledged(&with("open", true)));
        assert!(alert_acknowledged(&with("acked", false)));
        assert!(alert_acknowledged(&with(" ACKED ", false)));
        assert!(!alert_acknowledged(&with("open", false)));
        assert!(!alert_acknowledged(&with("closed", false)));
    }

    #[test]
    fn claims_yields_to_the_older_watchers() {
        let reaper_cfg = ReaperFeature {
            message_contains: "DiskUtilization".into(),
            ..Default::default()
        };
        let u = UnhealthyHostFeature::default();
        let g = GrafanaFeature::default();
        let plain = alert("[EC2]: dev1-Cassandra-100-DiskUtilization-Warning");
        assert!(claims(&plain, &reaper_cfg, &u, &g));
        // A reaper-titled disk alert is ours too (it gets the log gzip but
        // never a grow, see `DiskGrowFeature::no_grow`); reaper no longer
        // claims any disk-utilization alert.
        let reapery = alert("[EC2]: reaper-DiskUtilization-Warning");
        assert!(claims(&reapery, &reaper_cfg, &u, &g));
        assert!(!reaper::identifies(&reapery, &reaper_cfg));
        assert!(!claims(&alert("[EC2]: x-CPUUtilization"), &reaper_cfg, &u, &g));
    }

    #[test]
    fn choose_volume_takes_the_larger_one() {
        let v = [vol("v-root", 300, "gp3", "/dev/xvda"), vol("v-data", 17408, "gp3", "/dev/sdf")];
        assert_eq!(choose_volume(&v, "/dev/xvda").unwrap().id, "v-data");
    }

    #[test]
    fn choose_volume_refuses_when_the_larger_is_the_root() {
        let v = [vol("a", 300, "gp3", "/dev/sdf"), vol("b", 900, "gp3", "/dev/xvda")];
        assert!(choose_volume(&v, "/dev/xvda").unwrap_err().contains("root"));
    }

    #[test]
    fn choose_volume_refuses_on_equal_sizes() {
        let v = [vol("a", 500, "gp3", "/dev/sdf"), vol("b", 500, "gp3", "/dev/xvda")];
        assert!(choose_volume(&v, "/dev/xvda").unwrap_err().contains("equal"));
    }

    #[test]
    fn choose_volume_refuses_with_one_volume() {
        let v = [vol("a", 500, "gp3", "/dev/xvda")];
        assert!(choose_volume(&v, "/dev/xvda").unwrap_err().contains("only one"));
    }

    #[test]
    fn choose_volume_refuses_with_none() {
        assert!(choose_volume(&[], "/dev/xvda").unwrap_err().contains("no volumes"));
    }

    #[test]
    fn growth_is_one_tb_then_ten_percent() {
        let v = vol("v", 17408, "gp3", "/dev/sdf");
        assert_eq!(
            growth_options(&v),
            vec![
                Growth { new_gib: 18432, kind: GrowthKind::OneTb },
                Growth { new_gib: 19149, kind: GrowthKind::TenPercent },
            ]
        );
    }

    #[test]
    fn growth_skips_options_over_the_type_maximum() {
        assert!(growth_options(&vol("v", 65000, "gp3", "d")).is_empty());
        assert_eq!(
            growth_options(&vol("v", 64512, "gp3", "d")),
            vec![Growth { new_gib: 65536, kind: GrowthKind::OneTb }]
        );
        assert_eq!(
            growth_options(&vol("v", 900, "standard", "d")),
            vec![Growth { new_gib: 990, kind: GrowthKind::TenPercent }]
        );
        assert!(growth_options(&vol("v", 16000, "st1", "d")).is_empty());
    }

    #[test]
    fn ten_percent_rounds_up() {
        let g = growth_options(&vol("v", 11, "gp3", "d"));
        assert_eq!(g.last().unwrap(), &Growth { new_gib: 13, kind: GrowthKind::TenPercent });
    }

    #[test]
    fn max_gib_by_type() {
        assert_eq!(max_gib("io2"), 65536);
        assert_eq!(max_gib("sc1"), 16384);
        assert_eq!(max_gib("standard"), 1024);
        assert_eq!(max_gib("weird"), 16384);
    }

    #[test]
    fn modify_errors_are_classified() {
        assert_eq!(classify_modify_error("VolumeModificationRateExceeded"), ModifyError::RateLimited);
        assert_eq!(
            classify_modify_error("You can only modify once in the last 6 hours"),
            ModifyError::RateLimited
        );
        assert_eq!(
            classify_modify_error("InvalidParameterValue: size exceeds"),
            ModifyError::Refused
        );
        // AWS's own wording, and checked ahead of the Refused keywords.
        assert_eq!(
            classify_modify_error("Wait at least 6 hours between modifications"),
            ModifyError::RateLimited
        );
        assert_eq!(
            classify_modify_error("you have reached the maximum; wait at least 6 hours"),
            ModifyError::RateLimited
        );
        assert_eq!(
            classify_modify_error("An error occurred (VolumeLimitExceeded) when calling ModifyVolume"),
            ModifyError::Refused
        );
        assert_eq!(classify_modify_error(""), ModifyError::Ambiguous);
        assert_eq!(classify_modify_error("Connection timed out"), ModifyError::Ambiguous);
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    fn lsblk_line(name: &str, ty: &str, serial: &str, fs: &str, mnt: &str, pk: &str, size: u64) -> String {
        format!(
            "NAME=\"{name}\" TYPE=\"{ty}\" SERIAL=\"{serial}\" FSTYPE=\"{fs}\" MOUNTPOINT=\"{mnt}\" PKNAME=\"{pk}\" SIZE=\"{size}\""
        )
    }

    fn probe_text(extra_lsblk: &[String]) -> String {
        let mut l = vec![
            lsblk_line("nvme0n1", "disk", "vol0aaaaaaaaaaaaaaaa", "", "", "", 300 * GIB),
            lsblk_line("nvme0n1p1", "part", "", "xfs", "/", "nvme0n1", 300 * GIB - 1),
        ];
        l.extend(extra_lsblk.iter().cloned());
        format!(
            "__DG_BEGIN__\n__DG_SEC__ DFH\nFilesystem Size Used Avail Use% Mounted on\n/dev/nvme0n1p1 300G 10G 290G 4% /\n/dev/nvme1n1 18T 17T 1.0T 95% /data\n__DG_SEC__ BYTES\nMounted on 1B-blocks\n/ 322122547200\n/data 19791209299968\n__DG_SEC__ LSBLK\n{}\n__DG_END__\n",
            l.join("\n")
        )
    }

    fn data_disk() -> String {
        lsblk_line("nvme1n1", "disk", "vol0123456789abcdef", "xfs", "/data", "", 18432 * GIB)
    }

    fn probe_with(extra: &[String]) -> Probe {
        parse_probe(&probe_text(extra)).unwrap()
    }

    #[test]
    fn the_probe_is_parsed_into_df_sizes_and_block_devices() {
        let p = probe_with(&[data_disk()]);
        assert!(p.df_h.starts_with("Filesystem"));
        assert!(p.df_h.contains("/data"));
        assert_eq!(p.fs_bytes, vec![("/".to_string(), 322122547200), ("/data".to_string(), 19791209299968)]);
        assert_eq!(p.blocks.len(), 3);
        let d = &p.blocks[2];
        assert_eq!(d.name, "nvme1n1");
        assert_eq!(d.dev_type, "disk");
        assert_eq!(d.serial, "vol0123456789abcdef");
        assert_eq!(d.fstype, "xfs");
        assert_eq!(d.mountpoint, "/data");
        assert_eq!(d.pkname, "");
        assert_eq!(d.size_bytes, 18432 * GIB);
    }

    #[test]
    fn a_probe_without_its_markers_is_an_error() {
        assert!(parse_probe("").is_err());
        assert!(parse_probe("__DG_BEGIN__\n__DG_SEC__ DFH\nx\n").is_err());
    }

    #[test]
    fn the_data_device_is_found_by_volume_id_without_the_dash() {
        let p = probe_with(&[data_disk()]);
        let d = resolve_data_device(&p, "vol-0123456789abcdef").unwrap();
        assert_eq!(d.disk, "nvme1n1");
        assert_eq!(d.partition, None);
        assert_eq!(d.mount, "/data");
        assert_eq!(d.device_bytes, 18432 * GIB);
    }

    #[test]
    fn a_partitioned_data_disk_reports_its_partition_and_mount() {
        let p = probe_with(&[
            lsblk_line("nvme1n1", "disk", "vol0123456789abcdef", "", "", "", 18432 * GIB),
            lsblk_line("nvme1n1p1", "part", "", "xfs", "/data", "nvme1n1", 18432 * GIB - 1),
        ]);
        let d = resolve_data_device(&p, "vol-0123456789abcdef").unwrap();
        assert_eq!(d.disk, "nvme1n1");
        assert_eq!(d.partition.as_deref(), Some("nvme1n1p1"));
        assert_eq!(d.mount, "/data");
    }

    #[test]
    fn a_mount_other_than_data_still_resolves() {
        let p = probe_with(&[lsblk_line(
            "nvme1n1", "disk", "vol0123456789abcdef", "xfs", "/var/lib/cassandra", "", 100 * GIB,
        )]);
        assert_eq!(resolve_data_device(&p, "vol-0123456789abcdef").unwrap().mount, "/var/lib/cassandra");
    }

    #[test]
    fn resolution_refuses_unmounted_non_xfs_lvm_and_raid() {
        let id = "vol-0123456789abcdef";
        let unmounted = probe_with(&[lsblk_line("nvme1n1", "disk", "vol0123456789abcdef", "xfs", "", "", GIB)]);
        assert!(resolve_data_device(&unmounted, id).unwrap_err().contains("not mounted"));
        let ext4 = probe_with(&[lsblk_line("nvme1n1", "disk", "vol0123456789abcdef", "ext4", "/data", "", GIB)]);
        assert!(resolve_data_device(&ext4, id).unwrap_err().contains("not xfs"));
        let lvm = probe_with(&[
            lsblk_line("nvme1n1", "disk", "vol0123456789abcdef", "LVM2_member", "", "", GIB),
            lsblk_line("vg-data", "lvm", "", "xfs", "/data", "nvme1n1", GIB),
        ]);
        assert!(resolve_data_device(&lvm, id).unwrap_err().contains("lvm"));
        let raid = probe_with(&[
            lsblk_line("nvme1n1", "disk", "vol0123456789abcdef", "linux_raid_member", "", "", GIB),
            lsblk_line("md0", "raid1", "", "xfs", "/data", "nvme1n1", GIB),
        ]);
        assert!(resolve_data_device(&raid, id).unwrap_err().contains("raid"));
    }

    #[test]
    fn an_unknown_volume_id_is_an_error() {
        let p = probe_with(&[data_disk()]);
        assert!(resolve_data_device(&p, "vol-0000000000000000").is_err());
    }

    #[test]
    fn df_h_for_mount_returns_header_and_the_one_row() {
        let p = probe_with(&[data_disk()]);
        let s = df_h_for_mount(&p.df_h, "/data").unwrap();
        assert_eq!(s.lines().count(), 2);
        assert!(s.lines().next().unwrap().starts_with("Filesystem"));
        assert!(s.lines().nth(1).unwrap().ends_with("/data"));
        assert!(df_h_for_mount(&p.df_h, "/nope").is_none());
    }

    #[test]
    fn fs_bytes_are_looked_up_by_mount() {
        let p = probe_with(&[data_disk()]);
        assert_eq!(fs_bytes_for_mount(&p, "/data"), Some(19791209299968));
        assert_eq!(fs_bytes_for_mount(&p, "/x"), None);
    }

    #[test]
    fn device_ahead_of_fs_detects_a_half_finished_grow() {
        let fs = 17408 * GIB;
        assert!(!device_ahead_of_fs(fs, fs));
        assert!(device_ahead_of_fs(fs + 1024 * GIB, fs));
        assert!(!device_ahead_of_fs(fs + 100 * 1024 * 1024, fs));
        assert!(!device_ahead_of_fs(fs - 1, fs));
        // A fully grown 16 TB XFS filesystem sits about 2 GiB under its device.
        assert!(!device_ahead_of_fs(17_179_869_184_000, 17_177_732_186_112));
        // A real +1 TB on that volume is still caught.
        assert!(device_ahead_of_fs(17_179_869_184_000 + 1024 * GIB, 17_177_732_186_112));
    }

    fn dd(disk: &str, part: Option<&str>, mount: &str) -> DataDevice {
        DataDevice { disk: disk.into(), partition: part.map(String::from), mount: mount.into(), device_bytes: 0 }
    }

    #[test]
    fn the_filesystem_command_whitelists_its_inputs() {
        assert!(fs_grow_command(&dd("nvme1n1", None, "/data")).is_ok());
        assert!(fs_grow_command(&dd("nvme1n1", Some("nvme1n1p1"), "/data")).is_ok());
        assert!(fs_grow_command(&dd("nvme1n1;rm", None, "/data")).is_err());
        assert!(fs_grow_command(&dd("nvme1n1", Some("p 1"), "/data")).is_err());
        for bad in ["data", "/data/../etc", "/da ta", "/da'ta", "/da$ta", "/da`ta", "/da\"ta", ""] {
            assert!(fs_grow_command(&dd("nvme1n1", None, bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_filesystem_command_assigns_the_three_variables_first() {
        let c = fs_grow_command(&dd("nvme1n1", None, "/data")).unwrap();
        let mut l = c.lines();
        assert_eq!(l.next(), Some("RE_DG_DISK='nvme1n1'"));
        assert_eq!(l.next(), Some("RE_DG_PART=''"));
        assert_eq!(l.next(), Some("RE_DG_MOUNT='/data'"));
        assert!(c.ends_with(FS_SCRIPT));
        let c = fs_grow_command(&dd("nvme1n1", Some("nvme1n1p1"), "/data")).unwrap();
        assert!(c.lines().nth(1) == Some("RE_DG_PART='nvme1n1p1'"));
    }

    #[test]
    fn a_missing_verdict_marker_is_a_failure_never_success() {
        assert!(matches!(parse_fs_grow(""), FsGrow::Failed(_)));
        assert!(matches!(parse_fs_grow("xfs_growfs: ok\n"), FsGrow::Failed(_)));
        assert_eq!(
            parse_fs_grow("__DG_OK__\nFilesystem Size\n/dev/x 19T /data\n"),
            FsGrow::Grown { df_after: "Filesystem Size\n/dev/x 19T /data".into() }
        );
        match parse_fs_grow("__DG_FAIL__ growpart\ncannot open\n") {
            FsGrow::Failed(m) => assert!(m.contains("growpart") && m.contains("cannot open")),
            other => panic!("{other:?}"),
        }
        // FAIL outranks OK if both somehow appear.
        assert!(matches!(parse_fs_grow("__DG_OK__\n__DG_FAIL__ x\n"), FsGrow::Failed(_)));
    }

    #[test]
    fn the_alert_comment_is_only_the_two_df_blocks() {
        assert_eq!(alert_comment("B1\nB2", "A1\nA2"), "Before:\nB1\nB2\n\nAfter:\nA1\nA2");
    }

    fn exec_lines(script: &str) -> Vec<&str> {
        script.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
    }

    #[test]
    fn a_started_fix_cools_the_instance_for_the_window_only() {
        let mut s = DiskGrowState::new();
        let day = 24 * 60 * 60 * 1000;
        assert!(!s.cooling("i-1", 5, day));
        s.cool("i-1", 1_000);
        assert!(s.cooling("i-1", 1_000 + day - 1, day));
        assert!(!s.cooling("i-1", 1_000 + day, day));
        assert!(!s.cooling("i-2", 1_001, day), "per instance");
    }

    #[test]
    fn the_gzip_result_is_parsed_and_a_missing_marker_is_an_error() {
        let r = parse_gzip("noise\n__DG_GZ__ files=3 bytes=1024 failed=1 dry=0\n").unwrap();
        assert_eq!(
            r,
            GzipResult {
                files: 3,
                bytes: 1024,
                failed: 1,
                dry: false,
                cleaned: 0,
                cleaned_bytes: 0,
                journal: "none".into()
            }
        );
        let r = parse_gzip(
            "__DG_GZ__ files=1 bytes=2 failed=0 dry=0 cleaned=4 cleaned_bytes=900 journal=ok",
        )
        .unwrap();
        assert_eq!((r.cleaned, r.cleaned_bytes, r.journal.as_str()), (4, 900, "ok"));
        assert!(r.summary().contains("4 file(s), 900 bytes, was emptied"), "{}", r.summary());
        assert!(parse_gzip("__DG_GZ__ files=0 bytes=0 failed=0 dry=1").unwrap().dry);
        assert!(parse_gzip("done").is_err());
        assert!(parse_gzip("__DG_GZ__ files=x bytes=0 failed=0 dry=0").is_err());
    }

    #[test]
    fn the_cleanup_script_runs_only_its_listed_steps_and_never_when_dry() {
        let all = exec_lines(GZIP_SCRIPT);
        let text = all.join("\n");
        // The only mutating verbs: the four listed clean-up steps and gzip.
        assert_eq!(text.matches("truncate -s 0 --").count(), 1, "one truncate, in sweep()");
        assert_eq!(text.matches("rm -f --").count(), 1, "one removal, in sweep()");
        assert_eq!(text.matches("journalctl --vacuum-time=2d").count(), 1);
        assert_eq!(text.matches("gzip -n --").count(), 1);
        for banned in ["mv ", "unlink", "shred", "dd ", "> /", "-delete", "-exec", "rm -rf", "rm -r "] {
            assert!(!text.contains(banned), "cleanup script contains {banned:?}");
        }
        // The four steps, each aimed at exactly the path it was asked for.
        assert!(text.contains("find /var/lib/docker/containers -type f -name '*-json.log'"));
        assert!(text.contains("find /tmp -type f -atime +10 -print0"));
        assert!(text.contains("find /var/log -type f -name '*.log' -size +0 -mtime +7"));
        // Today's logs are never gzipped; already-compressed files are skipped.
        assert!(text.contains("find /var/log -xdev -type f"));
        assert!(text.contains("! -newermt \"$today\""), "today's logs must be skipped");
        assert!(text.contains("! -name '*.gz'"));
        // Dry is checked before either mutation in sweep() and before the vacuum.
        let sweep = text.find("sweep()").expect("sweep");
        let dry_check = text[sweep..].find("[ \"$dry\" = \"1\" ]").expect("dry check") + sweep;
        assert!(dry_check < text[sweep..].find("truncate -s 0 --").unwrap() + sweep);
        assert!(text.contains("if [ \"$dry\" = \"1\" ]; then\njournal=skipped"));
        // The clean-up runs before the gzip, or step 4 would find nothing.
        assert!(text.find("sweep truncate").unwrap() < text.find("gzip -n --").unwrap());
        assert!(!all.iter().any(|l| l.contains("__DG_GZ__")), "literal marker");
        let dry = gzip_command(true);
        assert!(dry.starts_with("RE_DG_DRY='1'\n") && gzip_command(false).starts_with("RE_DG_DRY='0'\n"));
    }

    #[test]
    fn the_probe_script_changes_nothing_on_the_box() {
        let bad = probe_violations(PROBE_SCRIPT);
        assert!(bad.is_empty(), "probe lines outside the allow-list: {bad:?}");
        // Markers are assembled at runtime, never literal.
        let all = exec_lines(PROBE_SCRIPT);
        for m in ["__DG_BEGIN__", "__DG_SEC__", "__DG_END__"] {
            assert!(!all.iter().any(|l| l.contains(m)), "literal marker {m}");
        }
    }

    /// Executable lines that are not on the allow-list: each must start with
    /// `set`, `echo`, `df`, `lsblk` or be a plain `NAME=value` assignment,
    /// and none may redirect, chain, pipe, background or substitute.
    fn probe_violations(script: &str) -> Vec<String> {
        let assign = |l: &str| {
            l.split_once('=').is_some_and(|(k, v)| {
                !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !k.starts_with(|c: char| c.is_ascii_digit())
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        };
        exec_lines(script)
            .into_iter()
            .filter(|l| {
                let first = l.split_whitespace().next().unwrap_or("");
                let allowed = matches!(first, "set" | "echo" | "df" | "lsblk") || assign(l);
                let chained = ['>', '<', ';', '&', '|', '`'].iter().any(|c| l.contains(*c)) || l.contains("$(");
                !allowed || chained
            })
            .map(String::from)
            .collect()
    }

    #[test]
    fn the_probe_allow_list_rejects_mutating_lines() {
        let bad = probe_violations("set -u\necho ok\npartprobe /dev/nvme1n1\necho x > /tmp/f\ndf -h\nx=1; reboot\n");
        assert_eq!(bad, vec!["partprobe /dev/nvme1n1", "echo x > /tmp/f", "x=1; reboot"]);
        assert!(probe_violations("set -u\nb=BEGIN\necho \"__x_${b}__\"\ndf -h\nlsblk -b -P\n").is_empty());
    }

    #[test]
    fn the_filesystem_script_only_grows_and_reads() {
        let body = exec_lines(FS_SCRIPT).join("\n");
        assert!(body.contains("xfs_growfs") && body.contains("growpart"));
        for bad in ["resize2fs", "mkfs", "rm ", "umount", "reboot", "systemctl"] {
            assert!(!body.contains(bad), "{bad}");
        }
        assert!(!body.contains("__DG_OK__") && !body.contains("__DG_FAIL__"));
    }

    fn inc(instance: &str, owner: &str) -> Incident {
        Incident {
            instance: instance.into(),
            owner_alert: owner.into(),
            created_at: "t".into(),
            volume_id: "vol-1".into(),
            df_before: "df".into(),
            mount: "/data".into(),
            started_ms: 0,
            phase: Phase::Modifying,
            fs_grown: false,
            fs_bytes_before: 0,
            facts: Default::default(),
        }
    }

    #[test]
    fn begin_makes_the_first_alert_the_owner_and_later_ones_duplicates() {
        let mut s = DiskGrowState::new();
        assert_eq!(s.begin("i-1", "a1", 0), Begin::Owner);
        s.start(inc("i-1", "a1"));
        assert_eq!(s.begin("i-1", "a2", 10), Begin::Duplicate { owner: "a1".into() });
        assert_eq!(s.begin("i-2", "a3", 10), Begin::Owner);
        assert!(!s.is_seen("a1"));
        s.mark_seen("a1");
        assert!(s.is_seen("a1"));
    }

    #[test]
    fn a_duplicate_never_replaces_the_owner() {
        let mut s = DiskGrowState::new();
        s.begin("i-1", "a1", 0);
        s.start(inc("i-1", "a1"));
        s.begin("i-1", "a2", 5);
        assert_eq!(s.owner_of("i-1"), Some("a1"));
        assert_eq!(s.incidents().len(), 1);
    }

    #[test]
    fn a_failed_instance_holds_its_slot_for_six_hours_then_frees_it() {
        let mut s = DiskGrowState::new();
        s.start(inc("i-1", "a1"));
        s.failed("i-1|vol-1", "i-1", 1_000);
        let h = 60 * 60 * 1000;
        assert_eq!(s.begin("i-1", "a2", 1_000 + 6 * h - 60_000), Begin::Held);
        assert_eq!(s.begin("i-1", "a2", 1_000 + 6 * h), Begin::Owner);
        let mut s = DiskGrowState::new();
        s.start(inc("i-1", "a1"));
        s.failed("i-1|vol-1", "i-1", 0);
        s.expire_failed(6 * h - 1);
        assert_eq!(s.begin("i-1", "a2", 6 * h - 1), Begin::Held);
        s.expire_failed(6 * h);
        assert_eq!(s.begin("i-1", "a2", 6 * h), Begin::Owner);
    }

    #[test]
    fn a_finished_incident_frees_the_instance() {
        let mut s = DiskGrowState::new();
        s.start(inc("i-1", "a1"));
        s.finish("i-1|vol-1");
        assert_eq!(s.begin("i-1", "a2", 0), Begin::Owner);
        assert!(s.owner_of("i-1").is_none());
    }

    #[test]
    fn transition_waits_while_modifying_then_escalates_after_the_usable_window() {
        let t = |e| transition(Phase::Modifying, &ModState::Modifying, false, e, 100, 500);
        assert_eq!(t(0), Transition::Wait);
        assert_eq!(t(99), Transition::Wait);
        assert!(matches!(t(100), Transition::Escalate(_)));
    }

    #[test]
    fn transition_grows_the_filesystem_once_usable() {
        for m in [ModState::Optimizing, ModState::Completed] {
            for p in [Phase::Modifying, Phase::Optimizing] {
                assert_eq!(transition(p, &m, false, 0, 100, 500), Transition::GrowFilesystem);
            }
        }
        assert_ne!(
            transition(Phase::Optimizing, &ModState::Optimizing, true, 0, 100, 500),
            Transition::GrowFilesystem
        );
        assert_ne!(
            transition(Phase::Optimizing, &ModState::Completed, true, 0, 100, 500),
            Transition::GrowFilesystem
        );
    }

    #[test]
    fn a_failed_modification_escalates() {
        for p in [Phase::Modifying, Phase::Optimizing] {
            for g in [false, true] {
                assert!(matches!(
                    transition(p, &ModState::Failed, g, 0, 100, 500),
                    Transition::Escalate(_)
                ));
            }
        }
    }

    #[test]
    fn completed_after_the_filesystem_grew_is_complete() {
        assert_eq!(
            transition(Phase::Optimizing, &ModState::Completed, true, 0, 100, 500),
            Transition::Complete
        );
    }

    #[test]
    fn optimizing_past_the_ceiling_stops_flashing_without_escalating() {
        let t = |e| transition(Phase::Optimizing, &ModState::Optimizing, true, e, 100, 500);
        assert_eq!(t(499), Transition::Wait);
        assert!(matches!(t(500), Transition::StopFlashing(_)));
        assert!(matches!(t(10_000), Transition::StopFlashing(_)));
    }

    #[test]
    fn an_unknown_modification_state_is_never_success() {
        let u = ModState::Unknown("weird".into());
        assert_eq!(transition(Phase::Optimizing, &u, true, 0, 100, 500), Transition::Wait);
        assert_eq!(transition(Phase::Modifying, &u, false, 99, 100, 500), Transition::Wait);
        assert!(matches!(
            transition(Phase::Modifying, &u, false, 100, 100, 500),
            Transition::Escalate(_)
        ));
        for e in [0, 100, 10_000] {
            assert_ne!(transition(Phase::Modifying, &u, false, e, 100, 500), Transition::Complete);
        }
    }

    #[test]
    fn parse_mod_state_reads_the_state_and_rejects_an_empty_list() {
        let j = |s: &str| format!(r#"{{"VolumesModifications":[{{"ModificationState":"{s}"}}]}}"#);
        assert_eq!(parse_mod_state(&j("modifying")), Ok(ModState::Modifying));
        assert_eq!(parse_mod_state(&j("optimizing")), Ok(ModState::Optimizing));
        assert_eq!(parse_mod_state(&j("completed")), Ok(ModState::Completed));
        assert_eq!(parse_mod_state(&j("failed")), Ok(ModState::Failed));
        assert_eq!(parse_mod_state(&j("odd")), Ok(ModState::Unknown("odd".into())));
        assert!(parse_mod_state(r#"{"VolumesModifications":[]}"#).is_err());
        assert!(parse_mod_state("not json").is_err());
    }

    #[test]
    fn growing_keys_lists_every_running_incident() {
        let mut s = DiskGrowState::new();
        s.start(inc("i-b", "a2"));
        s.start(inc("i-a", "a1"));
        s.set_phase("i-a|vol-1", Phase::Optimizing);
        assert_eq!(s.growing_keys(), vec!["i-a".to_string(), "i-b".to_string()]);
        s.fs_grown("i-a|vol-1");
        assert!(s.incidents().iter().any(|i| i.instance == "i-a" && i.fs_grown));
        s.finish("i-a|vol-1");
        assert_eq!(s.growing_keys(), vec!["i-b".to_string()]);
    }


    fn probe_two_data(pct_a: u32, pct_b: u32) -> Probe {
        let blk = |name: &str, ty: &str, serial: &str, mount: &str, pk: &str| BlockDev {
            name: name.into(),
            dev_type: ty.into(),
            serial: serial.into(),
            fstype: if mount.is_empty() { "".into() } else { "xfs".into() },
            mountpoint: mount.into(),
            pkname: pk.into(),
            size_bytes: 1 << 40,
        };
        Probe {
            df_h: format!(
                "Filesystem Size Used Avail Use% Mounted on\n/dev/nvme1n1 1T 1T 1T {pct_a}% /data\n/dev/nvme2n1 1T 1T 1T {pct_b}% /data2"
            ),
            fs_bytes: vec![],
            blocks: vec![
                blk("nvme0n1", "disk", "volroot", "", ""),
                blk("nvme1n1", "disk", "vola", "/data", ""),
                blk("nvme2n1", "disk", "volb", "/data2", ""),
            ],
        }
    }

    fn three_volumes() -> Vec<Volume> {
        vec![
            vol("vol-root", 100, "gp3", "/dev/xvda"),
            vol("vol-a", 1000, "gp3", "/dev/sdf"),
            vol("vol-b", 1000, "gp3", "/dev/sdg"),
        ]
    }

    #[test]
    fn the_root_filesystem_is_never_resolved_as_a_data_device() {
        // Plain disk mounted at /.
        let mut p = probe_two_data(95, 95);
        p.blocks[0].mountpoint = "/".into();
        p.blocks[0].fstype = "xfs".into();
        assert!(resolve_data_device(&p, "vol-root").unwrap_err().contains("root filesystem"));
        // Partitioned root disk: / is on the partition, /boot/efi on another.
        let part = |name: &str, mount: &str| BlockDev {
            name: name.into(),
            dev_type: "part".into(),
            fstype: "xfs".into(),
            mountpoint: mount.into(),
            pkname: "nvme0n1".into(),
            ..Default::default()
        };
        let mut p = probe_two_data(95, 95);
        p.blocks.push(part("nvme0n1p128", "/boot/efi"));
        p.blocks.push(part("nvme0n1p1", "/"));
        assert!(resolve_data_device(&p, "vol-root").unwrap_err().contains("root filesystem"));
        // And a volume that is on the box's / is never a target, even if the
        // EBS device name did not say root.
        let vols = vec![
            vol("vol-root", 5000, "gp3", "/dev/sdz"),
            vol("vol-a", 1000, "gp3", "/dev/sdf"),
            vol("vol-b", 1000, "gp3", "/dev/sdg"),
        ];
        let mounts: Vec<String> = vec!["/".into(), "/data".into(), "/data2".into()];
        let r = choose_targets(&vols, "/dev/xvda", &p, &mounts, 79, None).unwrap();
        assert!(r.iter().all(|v| v.id != "vol-root"), "{r:?}");
    }

    #[test]
    fn the_instance_id_property_is_only_a_fallback() {
        let mut a = alert("[EC2]: x-Cassandra-DiskUtilization-Warning");
        a.extra.insert("InstanceId".into(), "i-022a4cc66a1fcb45a".into());
        let none = crate::instance_age::AlertFacts::default();
        assert_eq!(facts_with_property(none, &a).instance_id, "i-022a4cc66a1fcb45a");
        // The description's own id wins.
        let own = crate::instance_age::AlertFacts { instance_id: "i-0123456789abcdef0".into(), ..Default::default() };
        assert_eq!(facts_with_property(own, &a).instance_id, "i-0123456789abcdef0");
        // Not an instance id: stays empty.
        a.extra.insert("InstanceId".into(), "{{ $labels.instance }}".into());
        assert_eq!(facts_with_property(Default::default(), &a).instance_id, "");
    }

    #[test]
    fn the_account_and_environment_fall_back_to_their_tags() {
        let mut a = alert("[EC2]: x-Cassandra-DiskUtilization-Warning");
        a.account = "123456789012".into();
        a.environment = "DEV1".into();
        let f = facts_with_property(Default::default(), &a);
        assert_eq!(f.account_id, "123456789012");
        assert_eq!(f.environment, "DEV1");
        // The description's own values win.
        let own = crate::instance_age::AlertFacts {
            account_id: "111111111111".into(),
            environment: "PROD".into(),
            ..Default::default()
        };
        let f = facts_with_property(own, &a);
        assert_eq!((f.account_id.as_str(), f.environment.as_str()), ("111111111111", "PROD"));
        // A template is never an account or an environment.
        a.account = "{{ $labels.account }}".into();
        a.environment = "&{%env%}%".into();
        let f = facts_with_property(Default::default(), &a);
        assert!(f.account.is_empty() && f.account_id.is_empty() && f.environment.is_empty());
    }

    #[test]
    fn the_other_watchers_get_instance_and_account_but_never_the_environment() {
        let mut a = alert("x");
        a.account = "123456789012".into();
        a.environment = "DEV1".into();
        a.extra.insert("InstanceId".into(), "i-022a4cc66a1fcb45a".into());
        let f = facts_with_instance_and_account(Default::default(), &a);
        assert_eq!(f.instance_id, "i-022a4cc66a1fcb45a");
        assert_eq!(f.account_id, "123456789012");
        assert!(f.environment.is_empty(), "the environment fallback is the disk watcher's only");
        assert_eq!(facts_with_property(Default::default(), &a).environment, "DEV1");
    }

    #[test]
    fn an_alert_for_the_root_filesystem_is_never_grown() {
        let cfg = crate::features::DiskGrowFeature::default();
        let mut a = alert("[EC2]: x-Cassandra-DiskUtilization-Warning");
        assert!(!never_grown(&cfg, &a));
        a.extra.insert("mountpoint".into(), "/data".into());
        assert!(!never_grown(&cfg, &a));
        a.extra.insert("mountpoint".into(), "/".into());
        assert!(never_grown(&cfg, &a));
    }

    #[test]
    fn the_alert_names_the_mount_to_grow_and_the_root_is_refused() {
        let mut a = alert("[EC2]: x-Cassandra-DiskUtilization-Warning");
        assert_eq!(alert_mount(&a), None);
        a.extra.insert("mountpoint".into(), "/data2".into());
        assert_eq!(alert_mount(&a).as_deref(), Some("/data2"));
        a.extra.insert("mountpoint".into(), "{{ $labels.mountpoint }}".into());
        assert_eq!(alert_mount(&a), None, "an unrendered template is not a mount");
        let p = probe_two_data(10, 10); // both well under the threshold
        let vols = three_volumes();
        let mounts: Vec<String> = vec!["/data".into(), "/data2".into()];
        // Named: grown whatever its use, and only that one.
        let r = choose_targets(&vols, "/dev/xvda", &p, &mounts, 79, Some("/data2")).unwrap();
        assert_eq!(r.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), vec!["vol-b"]);
        assert!(choose_targets(&vols, "/dev/xvda", &p, &mounts, 79, Some("/")).is_err());
        assert!(choose_targets(&vols, "/dev/xvda", &p, &mounts, 79, Some("/nope")).is_err());
    }

    #[test]
    fn use_percent_is_read_from_the_mounts_df_row() {
        let p = probe_two_data(91, 40);
        assert_eq!(use_pct_for_mount(&p.df_h, "/data"), Some(91));
        assert_eq!(use_pct_for_mount(&p.df_h, "/data2"), Some(40));
        assert_eq!(use_pct_for_mount(&p.df_h, "/nope"), None);
    }

    #[test]
    fn two_data_volumes_grow_whichever_is_above_the_threshold() {
        let mounts: Vec<String> = DEFAULT_DATA_MOUNTS.iter().map(|s| s.to_string()).collect();
        let ids = |r: Result<Vec<Volume>, String>| -> Vec<String> {
            r.unwrap().into_iter().map(|v| v.id).collect()
        };
        let go = |a, b| choose_targets(&three_volumes(), "/dev/xvda", &probe_two_data(a, b), &mounts, 80, None);
        assert_eq!(ids(go(85, 50)), vec!["vol-a"]);
        assert_eq!(ids(go(50, 85)), vec!["vol-b"]);
        assert_eq!(ids(go(90, 95)), vec!["vol-a", "vol-b"]);
        // "above 80", so exactly 80 is not.
        assert!(go(80, 80).is_err());
        assert!(go(10, 20).unwrap_err().contains("above 80%"));
    }

    #[test]
    fn two_volumes_grow_the_larger_one_only_when_it_is_above_the_threshold() {
        let vols = vec![vol("vol-root", 100, "gp3", "/dev/xvda"), vol("vol-a", 1000, "gp3", "/dev/sdf")];
        let r = choose_targets(&vols, "/dev/xvda", &probe_two_data(80, 10), &[], 79, None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "vol-a");
        // 79 is not above 79.
        assert!(choose_targets(&vols, "/dev/xvda", &probe_two_data(79, 10), &[], 79, None).is_err());
        // A box that cannot be read for use% leaves it to the grow chain.
        let mut unreadable = probe_two_data(10, 10);
        unreadable.df_h = "Filesystem Size Used Avail Use% Mounted on".into();
        assert_eq!(choose_targets(&vols, "/dev/xvda", &unreadable, &[], 79, None).unwrap().len(), 1);
    }

    #[test]
    fn a_non_data_mount_or_the_root_is_never_a_target() {
        let mut p = probe_two_data(95, 95);
        p.blocks[2].mountpoint = "/var/lib/other".into();
        p.df_h = p.df_h.replace("/data2", "/var/lib/other");
        let mounts: Vec<String> = DEFAULT_DATA_MOUNTS.iter().map(|s| s.to_string()).collect();
        let r = choose_targets(&three_volumes(), "/dev/xvda", &p, &mounts, 80, None).unwrap();
        assert_eq!(r.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), vec!["vol-a"]);
    }

    #[test]
    fn two_incidents_on_one_instance_finish_and_fail_independently() {
        let mut s = DiskGrowState::new();
        let mut a = inc("i-1", "a1");
        a.volume_id = "vol-a".into();
        let mut b = inc("i-1", "a1");
        b.volume_id = "vol-b".into();
        s.start(a.clone());
        s.start(b.clone());
        assert_eq!(s.incidents().len(), 2);
        assert_eq!(s.growing_keys(), vec!["i-1".to_string()]);
        s.finish(&a.key());
        assert!(s.instance_busy("i-1"));
        assert!(s.is_running(&b.key()));
        s.failed(&b.key(), "i-1", 0);
        assert!(!s.instance_busy("i-1"));
        assert_eq!(s.begin("i-1", "a2", 1), Begin::Held);
    }

    #[test]
    fn an_unreadable_environment_is_blank_or_a_template() {
        for env in ["", "  ", "{{env}}", "&{%x%}%", "<env>"] {
            assert!(environment_unreadable(env), "{env:?}");
        }
        for env in ["prod", "PROD ONE", "dev1"] {
            assert!(!environment_unreadable(env), "{env:?}");
        }
    }

    #[test]
    fn the_filesystem_plan_tells_a_lost_answer_from_a_device_not_yet_grown() {
        let gib = 1024 * 1024 * 1024u64;
        // The device is ahead: grow.
        assert_eq!(fs_plan(2000 * gib, 1000 * gib, 1000 * gib), FsPlan::Grow);
        // Not ahead, but the filesystem is larger than recorded: a grow that
        // landed and whose answer was lost.
        assert_eq!(fs_plan(2000 * gib, 2000 * gib, 1000 * gib), FsPlan::AlreadyGrown);
        // Not ahead and not larger: the new size has not reached the box.
        assert_eq!(fs_plan(1000 * gib, 1000 * gib, 1000 * gib), FsPlan::NotYet);
        // XFS overhead (under the 512 MiB threshold) is not a pending grow.
        let mib = 1024 * 1024u64;
        assert_eq!(fs_plan(1000 * gib, 1000 * gib - 100 * mib, 1000 * gib), FsPlan::NotYet);
    }

    #[test]
    fn recovery_tracks_a_live_modification_and_never_guesses_otherwise() {
        let gib = 1024 * 1024 * 1024u64;
        // A 2000 GiB volume; the box shows 1000 GiB (lagging) or 2000 GiB.
        let (lag, caught_up, vol) = (1000 * gib, 2000 * gib, 2000u64);
        // (e) Modifying and Unknown track whatever the sizes.
        for dev in [lag, caught_up] {
            assert_eq!(recovery(&Ok(ModState::Modifying), dev, vol), Recovery::Track);
            assert_eq!(recovery(&Ok(ModState::Unknown("x".into())), dev, vol), Recovery::Track);
        }
        // (a) Optimizing and the box already shows the full size (grown and
        // commented before a restart, or by hand): nothing. This was a false
        // page.
        assert!(matches!(recovery(&Ok(ModState::Optimizing), caught_up, vol), Recovery::Nothing(_)));
        assert!(matches!(
            recovery(&Ok(ModState::Optimizing), caught_up + gib, vol),
            Recovery::Nothing(_)
        ));
        // (b) Optimizing and the box lags the volume: track.
        assert_eq!(recovery(&Ok(ModState::Optimizing), lag, vol), Recovery::Track);
        assert_eq!(recovery(&Ok(ModState::Optimizing), caught_up - 1, vol), Recovery::Track);
        // (c) Completed and caught up: nothing.
        assert!(matches!(recovery(&Ok(ModState::Completed), caught_up, vol), Recovery::Nothing(_)));
        // (d) Completed but the box lags: track.
        assert_eq!(recovery(&Ok(ModState::Completed), lag, vol), Recovery::Track);
        // A failed modification may be months old and was not started by
        // this run: nothing to do (the caller logs a Warn), never a page or
        // a 6h hold.
        assert!(matches!(recovery(&Ok(ModState::Failed), lag, vol), Recovery::Nothing(_)));
        assert!(matches!(recovery(&Ok(ModState::Failed), caught_up, vol), Recovery::Nothing(_)));
        // Never modified at all: a person took it; nothing of ours to finish.
        let none = parse_mod_state(r#"{"VolumesModifications":[]}"#);
        assert!(matches!(recovery(&none, lag, vol), Recovery::Nothing(_)), "{none:?}");
        // Any other unreadable answer is a failure, never "nothing to do".
        assert!(matches!(recovery(&Err("AccessDenied".into()), lag, vol), Recovery::Failed(_)));
    }

    #[test]
    fn the_script_check_lists_every_non_root_volume_and_flags_those_at_80_or_more() {
        let mut p = probe_two_data(80, 79);
        p.fs_bytes = vec![("/data".into(), 1 << 40), ("/data2".into(), 1 << 40)];
        let c = script_check(&three_volumes(), "/dev/xvda", &p);
        assert!(c.skipped.is_empty(), "{:?}", c.skipped);
        let seen: Vec<(String, u32, bool)> =
            c.rows.iter().map(|r| (r.mount.clone(), r.use_pct, r.needs_grow())).collect();
        assert_eq!(
            seen,
            vec![("/data".to_string(), 80, true), ("/data2".to_string(), 79, false)]
        );
        assert!(c.rows[0].df.contains("/data"));
    }

    #[test]
    fn the_script_check_never_offers_root_and_reports_what_it_could_not_read() {
        let mut p = probe_two_data(95, 95);
        p.fs_bytes = vec![("/data".into(), 1 << 40)];
        // The root volume holds / and is never a row; /data2 has no size row.
        let c = script_check(&three_volumes(), "/dev/xvda", &p);
        assert_eq!(c.rows.len(), 1);
        assert_eq!(c.skipped.len(), 1, "{:?}", c.skipped);
        assert!(c.rows.iter().all(|r| r.mount != "/"));
        // A volume on the box's / is skipped even when named as a data device.
        let mut p = probe_two_data(95, 95);
        p.blocks[0].mountpoint = "/".into();
        p.blocks[0].fstype = "xfs".into();
        let vols = vec![vol("vol-root", 100, "gp3", "/dev/sdz")];
        let c = script_check(&vols, "/dev/xvda", &p);
        assert!(c.rows.is_empty());
        assert!(c.skipped[0].contains("root filesystem"), "{:?}", c.skipped);
    }

    #[test]
    fn a_typed_growth_is_a_positive_whole_number_that_fits_the_volume_type() {
        let v = vol("vol-a", 1000, "gp3", "/dev/sdf");
        assert_eq!(script_new_size(&v, "1024"), Ok(2024));
        assert_eq!(script_new_size(&v, " 1 "), Ok(1001));
        for bad in ["", "  ", "-1", "-1024", "0", "abc", "1.5", "1e3"] {
            assert!(script_new_size(&v, bad).is_err(), "{bad:?}");
        }
        assert!(script_new_size(&v, "-5").unwrap_err().contains("negative"));
        // Over the type's maximum, and an overflowing number, are refused.
        assert!(script_new_size(&v, "65000").is_err());
        assert!(script_new_size(&v, "18446744073709551615").is_err());
        assert_eq!(script_new_size(&v, "64536"), Ok(65536));
        assert_eq!(SCRIPT_DEFAULT_ADD_GIB, 1024);
    }
}
