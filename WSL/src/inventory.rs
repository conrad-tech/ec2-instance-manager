use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::aws_cli::run_aws_cli;
use crate::config::AppConfig;
use crate::error::Result;
use crate::models::{AuthStatus, AwsContext, Instance, Inventory, Mode, TagMapping};
use crate::sim;
use crate::util::normalize_field;

static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();

#[derive(Clone)]
struct CacheEntry {
    inventory: Inventory,
    cached_at: Instant,
}

pub fn load_inventory(
    context: &AwsContext,
    mapping: &TagMapping,
    env_keys: &[String],
    force_refresh: bool,
) -> Result<Inventory> {
    if context.mode == Mode::Live && context.auth_status != AuthStatus::Ok {
        return Ok(Inventory {
            instances: Vec::new(),
            fetched_at: SystemTime::now(),
        });
    }

    let account = context
        .account_id
        .clone()
        .unwrap_or_else(|| "unknown-account".to_string());
    let cache_key = format!("{}:{}:{}", context.mode.as_str(), account, context.region);

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if !force_refresh {
        if let Ok(guard) = cache.lock() {
            if let Some(entry) = guard.get(&cache_key) {
                if entry.cached_at.elapsed() <= Duration::from_secs(45) {
                    return Ok(entry.inventory.clone());
                }
            }
        }
    }

    let inventory = if context.mode == Mode::Sim {
        sim::generate_inventory(&context.region, mapping, env_keys)
    } else {
        load_live_inventory(context, mapping, env_keys)?
    };

    if let Ok(mut guard) = cache.lock() {
        guard.insert(
            cache_key,
            CacheEntry {
                inventory: inventory.clone(),
                cached_at: Instant::now(),
            },
        );
    }

    Ok(inventory)
}

/// Returns the path to the disk cache file for a given profile/region.
fn disk_cache_path(profile: &str, region: &str) -> Option<PathBuf> {
    AppConfig::config_path().map(|p| {
        let safe_profile = profile.replace(['/', '\\', ':'], "_");
        let safe_region = region.replace(['/', '\\', ':'], "_");
        p.with_file_name(format!("inventory_cache_{safe_profile}_{safe_region}.json"))
    })
}

/// Load inventory from disk cache. Returns None if no cache exists or it can't be read.
///
/// The derived fields are recomputed on the way out, because a cache written
/// under one environment tag key would otherwise keep serving that key's answer
/// until the next refetch -- so changing an account's key would appear to do
/// nothing. `tags` is cached alongside and `derive_fields` is a walk over a map
/// already in memory, which is cheaper and always correct next to trying to
/// invalidate the file whenever a key changes.
pub fn load_disk_cache(
    profile: &str,
    region: &str,
    mapping: &TagMapping,
    env_keys: &[String],
) -> Option<Inventory> {
    let path = disk_cache_path(profile, region)?;
    let data = std::fs::read_to_string(&path).ok()?;
    let mut inventory: Inventory = serde_json::from_str(&data).ok()?;
    for instance in &mut inventory.instances {
        derive_instance_fields(instance, mapping, env_keys);
    }
    Some(inventory)
}

/// Save inventory to disk cache.
pub fn save_disk_cache(profile: &str, region: &str, inventory: &Inventory) {
    if let Some(path) = disk_cache_path(profile, region) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string(inventory) {
            let _ = std::fs::write(&path, data);
        }
    }
}

fn load_live_inventory(
    context: &AwsContext,
    mapping: &TagMapping,
    env_keys: &[String],
) -> Result<Inventory> {
    let profile = context.profile.to_string();
    let region = context.region.to_string();

    let p1 = profile.clone();
    let r1 = region.clone();
    let basics_handle = std::thread::spawn(move || {
        run_aws_cli(
            Some(&p1),
            Some(&r1),
            &[
                "ec2",
                "describe-instances",
                "--output",
                "text",
                "--query",
                "Reservations[].Instances[].[InstanceId,State.Name,PrivateIpAddress,Placement.AvailabilityZone,InstanceType,ImageId,LaunchTime,PrivateDnsName]",
            ],
        )
    });

    let p2 = profile.clone();
    let r2 = region.clone();
    let tags_handle = std::thread::spawn(move || {
        run_aws_cli(
            Some(&p2),
            Some(&r2),
            &[
                "ec2",
                "describe-tags",
                "--output",
                "text",
                "--query",
                "Tags[].[ResourceId,Key,Value]",
            ],
        )
    });

    let p3 = profile.clone();
    let r3 = region.clone();
    let ssm_handle = std::thread::spawn(move || {
        run_aws_cli(
            Some(&p3),
            Some(&r3),
            &[
                "ssm",
                "describe-instance-information",
                "--output",
                "text",
                "--query",
                "InstanceInformationList[].[InstanceId,PingStatus,LastPingDateTime]",
            ],
        )
    });

    let basics_output = basics_handle.join().expect("basics thread panicked")?;
    let tags_output = tags_handle.join().expect("tags thread panicked")?;
    let ssm_output = ssm_handle.join().expect("ssm thread panicked")?;

    let mut instances = parse_instance_basics(&basics_output)?;
    apply_tags(&mut instances, &tags_output);
    apply_ssm_status(&mut instances, &ssm_output);
    derive_fields(&mut instances, mapping, env_keys);

    let mut out: Vec<Instance> = instances.into_values().collect();
    out.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));

    Ok(Inventory {
        instances: out,
        fetched_at: SystemTime::now(),
    })
}

fn parse_instance_basics(raw: &str) -> Result<BTreeMap<String, Instance>> {
    let mut map = BTreeMap::new();
    let raw_trimmed = raw.trim();

    if raw_trimmed.is_empty()
        || raw_trimmed.eq_ignore_ascii_case("none")
        || raw_trimmed.eq_ignore_ascii_case("null")
    {
        return Ok(map);
    }

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }

        let instance_id = fields[0].trim();
        let state = fields[1].trim();

        if instance_id.is_empty() {
            continue;
        }

        let mut instance = Instance::new(instance_id.to_string(), state.to_string());
        instance.private_ip = fields.get(2).and_then(|v| normalize_field(v));
        instance.az = fields.get(3).and_then(|v| normalize_field(v));
        instance.instance_type = fields.get(4).and_then(|v| normalize_field(v));
        instance.image_id = fields.get(5).and_then(|v| normalize_field(v));
        instance.launch_time = fields.get(6).and_then(|v| normalize_field(v));
        instance.private_dns = fields.get(7).and_then(|v| normalize_field(v));

        map.insert(instance_id.to_string(), instance);
    }

    Ok(map)
}

fn apply_tags(instances: &mut BTreeMap<String, Instance>, raw: &str) {
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 3 {
            continue;
        }

        let offset = if fields[0] == "TAGS" { 1 } else { 0 };
        if fields.len() < offset + 3 {
            continue;
        }

        let resource_id = fields[offset].trim();
        let key = fields[offset + 1].trim();
        let value = fields[offset + 2..].join("\t").trim().to_string();

        if let Some(instance) = instances.get_mut(resource_id) {
            instance.tags.insert(key.to_string(), value);
        }
    }
}

fn apply_ssm_status(instances: &mut BTreeMap<String, Instance>, raw: &str) {
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }

        let instance_id = fields[0].trim();
        if let Some(instance) = instances.get_mut(instance_id) {
            instance.ssm_managed = true;
            instance.ssm_ping = fields.get(1).and_then(|v| normalize_field(v));
            instance.ssm_last_ping = fields.get(2).and_then(|v| normalize_field(v));
        }
    }
}

fn derive_fields(
    instances: &mut BTreeMap<String, Instance>,
    mapping: &TagMapping,
    env_keys: &[String],
) {
    for instance in instances.values_mut() {
        derive_instance_fields(instance, mapping, env_keys);
    }
}

/// The per-instance half of `derive_fields`, so the disk cache can re-derive
/// an `Inventory` (a `Vec`) without first rebuilding the map.
fn derive_instance_fields(instance: &mut Instance, mapping: &TagMapping, env_keys: &[String]) {
    instance.name = instance.tags.get("Name").cloned();
    // Resolved here, at parse time, because this is the one place the
    // account is unambiguous -- an inventory belongs to exactly one. That
    // is what lets `instance_env` in the GUI collapse to a field read
    // instead of every one of its call sites carrying the key.
    instance.env = first_tag(&instance.tags, env_keys);
    instance.app_service = first_tag(&instance.tags, &mapping.app_keys);
    instance.role = first_tag(&instance.tags, &mapping.role_keys);
    instance.team_owner = first_tag(&instance.tags, &mapping.team_keys);

    instance.asg = instance
        .tags
        .get("aws:autoscaling:groupName")
        .cloned()
        .or_else(|| instance.tags.get("AutoScalingGroupName").cloned());
}

fn first_tag(tags: &BTreeMap<String, String>, keys: &[String]) -> Option<String> {
    for key in keys {
        if let Some(value) = tags.get(key) {
            if !value.trim().is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // `Instance` does NOT derive Default -- use the `Instance::new(id, state)`
    // constructor the existing tests in this module already use.
    fn tagged(key: &str, value: &str) -> BTreeMap<String, Instance> {
        let mut inst = Instance::new("i-1".to_string(), "running".to_string());
        inst.tags.insert(key.to_string(), value.to_string());
        let mut map = BTreeMap::new();
        map.insert("i-1".to_string(), inst);
        map
    }

    /// The account's own key wins, which is the whole point: an account
    /// tagging under `Stage` gets its environments seen.
    #[test]
    fn derive_fields_reads_the_account_s_own_env_tag_key() {
        let mut insts = tagged("Stage", "sbx");
        let keys = vec!["Stage".to_string(), "MMODAL_ENV".to_string()];
        derive_fields(&mut insts, &TagMapping::default(), &keys);
        assert_eq!(insts["i-1"].env.as_deref(), Some("sbx"));
    }

    /// And MMODAL_ENV still works, so nothing that works today stops.
    #[test]
    fn derive_fields_still_reads_mmodal_env() {
        let mut insts = tagged("MMODAL_ENV", "DEV1");
        let keys = vec!["MMODAL_ENV".to_string()];
        derive_fields(&mut insts, &TagMapping::default(), &keys);
        assert_eq!(insts["i-1"].env.as_deref(), Some("DEV1"));
    }

    /// The regression that would otherwise have shipped: a real instance
    /// routinely carries a generic `Env` tag ALONGSIDE `MMODAL_ENV`, and the
    /// first version of the key chain put the global `env_keys` list first --
    /// so every such instance silently re-resolved to the wrong value.
    ///
    /// Pinned here, against the default chain an unconfigured account actually
    /// gets, rather than only over the key list: this is the level where the
    /// resolution happens.
    #[test]
    fn mmodal_env_beats_a_generic_env_tag_on_the_same_instance() {
        let mut insts = tagged("MMODAL_ENV", "DEV1");
        insts
            .get_mut("i-1")
            .expect("the instance exists")
            .tags
            .insert("Env".to_string(), "prod".to_string());
        let keys = AppConfig::default().env_tag_keys_for("111");
        derive_fields(&mut insts, &TagMapping::default(), &keys);
        assert_eq!(insts["i-1"].env.as_deref(), Some("DEV1"));
    }

    /// The lower-case spelling still resolves -- `sim.rs` writes exactly this,
    /// and the GUI's five hardcoded lookups each carried an `or_else` for it,
    /// so dropping it would have broken sim on its own.
    #[test]
    fn the_lower_case_mmodal_env_tag_still_resolves() {
        let mut insts = tagged("mmodal_env", "staging");
        let keys = AppConfig::default().env_tag_keys_for("111");
        derive_fields(&mut insts, &TagMapping::default(), &keys);
        assert_eq!(insts["i-1"].env.as_deref(), Some("staging"));
    }

    /// And an account tagging ONLY `Env` now resolves, which it never did
    /// before this existed -- that is what `env_keys` still earns its place in
    /// the chain for, sitting last.
    #[test]
    fn an_instance_tagged_only_env_resolves_through_the_global_list() {
        let mut insts = tagged("Env", "sbx");
        let keys = AppConfig::default().env_tag_keys_for("111");
        derive_fields(&mut insts, &TagMapping::default(), &keys);
        assert_eq!(insts["i-1"].env.as_deref(), Some("sbx"));
    }

    /// An instance carrying none of the keys has no environment -- not a blank
    /// string, which would read as an environment named "".
    #[test]
    fn an_instance_with_no_matching_tag_has_no_environment() {
        let mut insts = tagged("Unrelated", "x");
        let keys = vec!["MMODAL_ENV".to_string()];
        derive_fields(&mut insts, &TagMapping::default(), &keys);
        assert_eq!(insts["i-1"].env, None);
    }

    #[test]
    fn parse_basics_lines() {
        let raw = "i-1\trunning\t10.0.0.1\tus-east-1a\tt3.micro\t2026-01-01T00:00:00Z\n";
        let got = parse_instance_basics(raw).expect("expected one instance row");
        assert_eq!(got.len(), 1);
        let one = got.get("i-1").expect("instance must exist");
        assert_eq!(one.private_ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(one.state, "running");
    }

    #[test]
    fn parse_basics_empty_output_is_empty_inventory() {
        let got = parse_instance_basics("").expect("empty output should parse");
        assert!(got.is_empty());
    }

    #[test]
    fn parse_basics_none_output_is_empty_inventory() {
        let got = parse_instance_basics("None").expect("None output should parse");
        assert!(got.is_empty());
    }

    #[test]
    fn tag_apply_with_prefix() {
        let mut map = BTreeMap::new();
        map.insert(
            "i-1".to_string(),
            Instance::new("i-1".to_string(), "running".to_string()),
        );
        apply_tags(&mut map, "TAGS\ti-1\tName\tapi\n");
        let one = map.get("i-1").expect("instance must exist");
        assert_eq!(one.tags.get("Name").map(String::as_str), Some("api"));
    }
}
