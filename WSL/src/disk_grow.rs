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
}
