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

/// Does the title carry both markers?
pub fn identifies(alert: &Alert) -> bool {
    TITLE_MARKERS.iter().all(|m| contains_ci(&alert.message, m))
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

pub fn fs_bytes_for_mount(probe: &Probe, mount: &str) -> Option<u64> {
    probe.fs_bytes.iter().find(|(m, _)| m == mount).map(|(_, b)| *b)
}

/// True when the device is at least 512 MiB larger than its filesystem, i.e.
/// a resize landed but the filesystem was never grown. XFS metadata overhead
/// never approaches that.
pub fn device_ahead_of_fs(device_bytes: u64, fs_bytes: u64) -> bool {
    device_bytes.saturating_sub(fs_bytes) >= 512 * 1024 * 1024
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
    fn claims_yields_to_the_older_watchers() {
        let reaper_cfg = ReaperFeature {
            message_contains: "DiskUtilization".into(),
            ..Default::default()
        };
        let u = UnhealthyHostFeature::default();
        let g = GrafanaFeature::default();
        let plain = alert("[EC2]: dev1-Cassandra-100-DiskUtilization-Warning");
        assert!(claims(&plain, &reaper_cfg, &u, &g));
        let reapery = alert("[EC2]: reaper-DiskUtilization-Warning");
        assert!(!claims(&reapery, &reaper_cfg, &u, &g));
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
}
