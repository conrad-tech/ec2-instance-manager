use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::accounts;
use crate::error::Result;
use crate::models::{Mode, PortForwardPreset, ProfileConfig, RecentConnection, SavedFilter, TagMapping};
use crate::util::{home_dir, split_csv};

const APP_DIR: &str = "ec2-manager";
const FILE_NAME: &str = "config.ini";
const RECENTS_LIMIT: usize = 20;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub default_mode: Mode,
    pub default_terminal: Option<String>,
    pub default_region: Option<String>,
    pub account_regions: BTreeMap<String, String>,
    pub tag_mapping: TagMapping,
    pub favorites: BTreeMap<String, Vec<String>>,
    pub recents: Vec<RecentConnection>,
    pub saved_filters: BTreeMap<String, Vec<SavedFilter>>,
    pub port_forward_presets: Vec<PortForwardPreset>,
    pub profiles: Vec<ProfileConfig>,
    pub last_selected_profile: Option<String>,
    pub theme: Option<String>,
    pub scroll_sensitivity: Option<f32>,
    pub ui_scale: Option<f32>,
    pub account_colors_enabled: bool,
    pub account_colors: BTreeMap<String, String>,
    pub reset_filter_on_profile_switch: bool,
    /// Environment names excluded from the color legend
    pub excluded_envs: Vec<String>,
    /// Whether the one-shot "shared excluded by default" migration has run.
    pub shared_env_default_applied: bool,
    /// Saved window position/size from last close
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
    pub window_w: Option<f32>,
    pub window_h: Option<f32>,
    pub window_maximized: Option<bool>,
    /// Starting path used when opening the remote SSM file browser in a
    /// new connection tab. Falls back to "/home/ec2-user" if unset.
    pub default_remote_browser_path: Option<String>,
    /// Starting directory used by the native Upload/Download file dialogs.
    /// Falls back to the OS default (typically last-used) if unset.
    pub default_local_dialog_path: Option<String>,
    /// Global library of known SSH private-key (pem) paths the user has
    /// added or that were discovered while scanning ~/.ssh/config.
    pub ssh_pem_library: Vec<String>,
    /// Per-profile default pem path (profile_id -> pem path).
    pub ssh_pem_default: BTreeMap<String, String>,
    /// Per-profile SSH login user (profile_id -> user, e.g. "ec2-user").
    pub ssh_user_default: BTreeMap<String, String>,
    /// Per-instance pem override (instance_id -> pem path). Takes
    /// precedence over the profile default for that one instance.
    pub ssh_pem_instance: BTreeMap<String, String>,
    /// Per-profile "don't ask for pem again" flag (profile_id -> true).
    pub vscode_pem_suppressed: BTreeMap<String, bool>,
    /// Default left-pane action for the selected instance:
    /// "connect" (embedded SSM terminal) or "vscode" (open in VS Code).
    pub default_connect_action: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_mode: Mode::Live,
            default_terminal: None,
            default_region: Some("us-east-1".to_string()),
            account_regions: BTreeMap::new(),
            tag_mapping: TagMapping::default(),
            favorites: BTreeMap::new(),
            recents: Vec::new(),
            saved_filters: BTreeMap::new(),
            port_forward_presets: vec![
                PortForwardPreset {
                    name: "ssh-local".to_string(),
                    local_port: 2222,
                    remote_port: 22,
                },
                PortForwardPreset {
                    name: "postgres".to_string(),
                    local_port: 5432,
                    remote_port: 5432,
                },
            ],
            profiles: Vec::new(),
            last_selected_profile: None,
            theme: None,
            scroll_sensitivity: None,
            ui_scale: None,
            account_colors_enabled: true,
            account_colors: BTreeMap::new(),
            reset_filter_on_profile_switch: true,
            excluded_envs: Vec::new(),
            shared_env_default_applied: false,
            window_x: None,
            window_y: None,
            window_w: None,
            window_h: None,
            window_maximized: None,
            default_remote_browser_path: None,
            default_local_dialog_path: None,
            ssh_pem_library: Vec::new(),
            ssh_pem_default: BTreeMap::new(),
            ssh_user_default: BTreeMap::new(),
            ssh_pem_instance: BTreeMap::new(),
            vscode_pem_suppressed: BTreeMap::new(),
            default_connect_action: "connect".to_string(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Option<PathBuf> {
        if cfg!(windows) {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|p| p.join(APP_DIR).join(FILE_NAME))
                .or_else(|| {
                    home_dir().map(|p| {
                        p.join("AppData")
                            .join("Roaming")
                            .join(APP_DIR)
                            .join(FILE_NAME)
                    })
                })
        } else {
            home_dir().map(|p| p.join(".config").join(APP_DIR).join(FILE_NAME))
        }
    }

    pub fn load() -> Result<Self> {
        let Some(path) = Self::config_path() else {
            let mut cfg = Self::default();
            let json_accounts = accounts::load_accounts();
            if !json_accounts.is_empty() {
                cfg.profiles = json_accounts;
            }
            return Ok(cfg);
        };

        if !path.exists() {
            let mut cfg = Self::default();
            let json_accounts = accounts::load_accounts();
            if !json_accounts.is_empty() {
                cfg.profiles = json_accounts;
            }
            return Ok(cfg);
        }

        let raw = fs::read_to_string(path)?;
        let mut cfg = Self::parse(&raw);

        // accounts.json takes precedence over profile_name.* entries in config.ini
        let json_accounts = accounts::load_accounts();
        if !json_accounts.is_empty() {
            cfg.profiles = json_accounts;
        }

        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::config_path() else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, self.to_text())?;
        Ok(())
    }

    pub fn upsert_account_region(&mut self, account_id: &str, region: &str) {
        if !account_id.is_empty() && !region.is_empty() {
            self.account_regions
                .insert(account_id.to_string(), region.to_string());
        }
    }

    /// Add a pem path to the global library if not already present.
    pub fn add_pem_to_library(&mut self, pem: &str) {
        let pem = pem.trim();
        if !pem.is_empty() && !self.ssh_pem_library.iter().any(|p| p == pem) {
            self.ssh_pem_library.push(pem.to_string());
        }
    }

    /// Resolve the pem to use for a given instance: per-instance override
    /// first, then the profile default. Returns None if neither is set.
    pub fn resolve_pem(&self, profile_id: &str, instance_id: &str) -> Option<String> {
        self.ssh_pem_instance
            .get(instance_id)
            .or_else(|| self.ssh_pem_default.get(profile_id))
            .cloned()
    }

    /// Resolve the SSH login user for a profile, defaulting to "ec2-user".
    pub fn resolve_ssh_user(&self, profile_id: &str) -> String {
        self.ssh_user_default
            .get(profile_id)
            .cloned()
            .unwrap_or_else(|| "ec2-user".to_string())
    }

    pub fn scope_key(account_id: &str, region: &str) -> String {
        format!("{}:{}", account_id.trim(), region.trim())
    }

    pub fn favorites_for_scope(&self, account_id: &str, region: &str) -> Vec<String> {
        let key = Self::scope_key(account_id, region);
        self.favorites.get(&key).cloned().unwrap_or_default()
    }

    pub fn is_favorite(&self, account_id: &str, region: &str, instance_id: &str) -> bool {
        let key = Self::scope_key(account_id, region);
        self.favorites
            .get(&key)
            .map(|v| v.iter().any(|id| id.eq_ignore_ascii_case(instance_id)))
            .unwrap_or(false)
    }


    pub fn toggle_favorite(&mut self, account_id: &str, region: &str, instance_id: &str) -> bool {
        let key = Self::scope_key(account_id, region);
        let entry = self.favorites.entry(key).or_default();
        if let Some(idx) = entry
            .iter()
            .position(|id| id.eq_ignore_ascii_case(instance_id))
        {
            entry.remove(idx);
            false
        } else {
            entry.push(instance_id.to_string());
            entry.sort();
            true
        }
    }

    pub fn add_recent_connection(&mut self, recent: RecentConnection) {
        self.recents.retain(|item| {
            !(item.account_id == recent.account_id
                && item.region == recent.region
                && item.instance_id.eq_ignore_ascii_case(&recent.instance_id))
        });
        self.recents.insert(0, recent);
        if self.recents.len() > RECENTS_LIMIT {
            self.recents.truncate(RECENTS_LIMIT);
        }
    }

    pub fn recents_for_scope(&self, account_id: &str, region: &str) -> Vec<RecentConnection> {
        self.recents
            .iter()
            .filter(|item| item.account_id == account_id && item.region == region)
            .cloned()
            .collect()
    }

    pub fn saved_filters_for_scope(&self, account_id: &str, region: &str) -> Vec<SavedFilter> {
        let key = Self::scope_key(account_id, region);
        self.saved_filters.get(&key).cloned().unwrap_or_default()
    }

    pub fn upsert_saved_filter(&mut self, account_id: &str, region: &str, saved: SavedFilter) {
        let key = Self::scope_key(account_id, region);
        let list = self.saved_filters.entry(key).or_default();

        if let Some(existing) = list
            .iter_mut()
            .find(|item| item.name.eq_ignore_ascii_case(&saved.name))
        {
            *existing = saved;
            return;
        }

        list.push(saved);
        list.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
    }

    pub fn delete_saved_filter(&mut self, account_id: &str, region: &str, name: &str) -> bool {
        let key = Self::scope_key(account_id, region);
        let Some(list) = self.saved_filters.get_mut(&key) else {
            return false;
        };

        let before = list.len();
        list.retain(|item| !item.name.eq_ignore_ascii_case(name));
        before != list.len()
    }

    pub fn upsert_port_forward_preset(&mut self, preset: PortForwardPreset) {
        if let Some(existing) = self
            .port_forward_presets
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&preset.name))
        {
            *existing = preset;
            return;
        }

        self.port_forward_presets.push(preset);
        self.port_forward_presets.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
    }

    pub fn delete_port_forward_preset(&mut self, name: &str) -> bool {
        let before = self.port_forward_presets.len();
        self.port_forward_presets
            .retain(|preset| !preset.name.eq_ignore_ascii_case(name));
        before != self.port_forward_presets.len()
    }

    pub fn has_configured_profiles(&self) -> bool {
        !self.profiles.is_empty()
    }

    fn parse(raw: &str) -> Self {
        let mut cfg = Self::default();
        cfg.recents.clear();

        let mut profile_names: BTreeMap<String, String> = BTreeMap::new();
        let mut profile_regions: BTreeMap<String, String> = BTreeMap::new();
        let mut profile_account_ids: BTreeMap<String, String> = BTreeMap::new();

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((k, v)) = line.split_once('=') else {
                continue;
            };

            let key = k.trim();
            let value = v.trim();

            if let Some(rest) = key.strip_prefix("profile_name.") {
                if !rest.is_empty() && !value.is_empty() {
                    profile_names.insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("profile_region.") {
                if !rest.is_empty() && !value.is_empty() {
                    profile_regions.insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("profile_account_id.") {
                if !rest.is_empty() && !value.is_empty() {
                    profile_account_ids.insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("account_color.") {
                if !rest.is_empty() && !value.is_empty() {
                    cfg.account_colors
                        .insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("account_region.") {
                if !rest.is_empty() && !value.is_empty() {
                    cfg.account_regions
                        .insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("ssh_pem.") {
                if !rest.is_empty() && !value.is_empty() {
                    cfg.ssh_pem_default
                        .insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("ssh_user.") {
                if !rest.is_empty() && !value.is_empty() {
                    cfg.ssh_user_default
                        .insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("ssh_pem_instance.") {
                if !rest.is_empty() && !value.is_empty() {
                    cfg.ssh_pem_instance
                        .insert(rest.to_string(), value.to_string());
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("vscode_pem_suppress.") {
                if !rest.is_empty() {
                    cfg.vscode_pem_suppressed.insert(
                        rest.to_string(),
                        matches!(value, "1" | "true" | "TRUE"),
                    );
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("favorite.") {
                if !rest.is_empty() {
                    cfg.favorites.insert(rest.to_string(), split_csv(value));
                }
                continue;
            }

            if let Some(rest) = key.strip_prefix("saved_filter.") {
                if rest.is_empty() || value.is_empty() {
                    continue;
                }
                if let Some(saved) = parse_saved_filter(value) {
                    cfg.saved_filters
                        .entry(rest.to_string())
                        .or_default()
                        .push(saved);
                }
                continue;
            }

            match key {
                "default_mode" => {
                    if let Some(parsed) = Mode::parse(value) {
                        cfg.default_mode = parsed;
                    }
                }
                "default_terminal" => {
                    cfg.default_terminal = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                "default_region" => {
                    cfg.default_region = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                "env_keys" => {
                    let parsed = split_csv(value);
                    if !parsed.is_empty() {
                        cfg.tag_mapping.env_keys = parsed;
                    }
                }
                "app_keys" => {
                    let parsed = split_csv(value);
                    if !parsed.is_empty() {
                        cfg.tag_mapping.app_keys = parsed;
                    }
                }
                "role_keys" => {
                    let parsed = split_csv(value);
                    if !parsed.is_empty() {
                        cfg.tag_mapping.role_keys = parsed;
                    }
                }
                "team_keys" => {
                    let parsed = split_csv(value);
                    if !parsed.is_empty() {
                        cfg.tag_mapping.team_keys = parsed;
                    }
                }
                "recent" => {
                    if let Some(recent) = parse_recent(value) {
                        cfg.recents.push(recent);
                    }
                }
                "port_forward_preset" => {
                    if let Some(preset) = parse_port_forward_preset(value) {
                        cfg.upsert_port_forward_preset(preset);
                    }
                }
                "theme" => {
                    cfg.theme = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                "scroll_sensitivity" => {
                    if let Ok(val) = value.parse::<f32>() {
                        cfg.scroll_sensitivity = Some(val);
                    }
                }
                "ui_scale" => {
                    if let Ok(val) = value.parse::<f32>() {
                        cfg.ui_scale = Some(val);
                    }
                }
                "account_colors_enabled" => {
                    cfg.account_colors_enabled = !matches!(value, "0" | "false" | "FALSE");
                }
                "reset_filter_on_profile_switch" => {
                    cfg.reset_filter_on_profile_switch = !matches!(value, "0" | "false" | "FALSE");
                }
                "excluded_envs" => {
                    cfg.excluded_envs = split_csv(value);
                }
                "shared_env_default_applied" => {
                    cfg.shared_env_default_applied = matches!(value, "1" | "true" | "TRUE");
                }
                "last_selected_profile" => {
                    cfg.last_selected_profile = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                "window_x" => { if let Ok(v) = value.parse::<f32>() { cfg.window_x = Some(v); } }
                "window_y" => { if let Ok(v) = value.parse::<f32>() { cfg.window_y = Some(v); } }
                "window_w" => { if let Ok(v) = value.parse::<f32>() { cfg.window_w = Some(v); } }
                "window_h" => { if let Ok(v) = value.parse::<f32>() { cfg.window_h = Some(v); } }
                "window_maximized" => { cfg.window_maximized = Some(matches!(value, "1" | "true" | "TRUE")); }
                "ssh_pem_known" => {
                    if !value.is_empty() && !cfg.ssh_pem_library.iter().any(|p| p == value) {
                        cfg.ssh_pem_library.push(value.to_string());
                    }
                }
                "default_connect_action" => {
                    cfg.default_connect_action = if value == "vscode" {
                        "vscode".to_string()
                    } else {
                        "connect".to_string()
                    };
                }
                "default_remote_browser_path" => {
                    cfg.default_remote_browser_path = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                "default_local_dialog_path" => {
                    cfg.default_local_dialog_path = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                }
                _ => {}
            }
        }

        // Merge profile_name and profile_region maps into Vec<ProfileConfig>.
        for (id, display_name) in &profile_names {
            cfg.profiles.push(ProfileConfig {
                profile_id: id.clone(),
                display_name: display_name.clone(),
                account_id: profile_account_ids.get(id).cloned().unwrap_or_default(),
                region: profile_regions.get(id).cloned(),
                sort_order: None,
                color: None,
            });
        }

        cfg
    }

    fn to_text(&self) -> String {
        let mut lines = vec![
            format!("default_mode={}", self.default_mode.as_str()),
            format!(
                "default_terminal={}",
                self.default_terminal.clone().unwrap_or_default()
            ),
            format!(
                "default_region={}",
                self.default_region.clone().unwrap_or_default()
            ),
            format!("env_keys={}", self.tag_mapping.env_keys.join(",")),
            format!("app_keys={}", self.tag_mapping.app_keys.join(",")),
            format!("role_keys={}", self.tag_mapping.role_keys.join(",")),
            format!("team_keys={}", self.tag_mapping.team_keys.join(",")),
        ];

        if let Some(theme) = &self.theme {
            lines.push(format!("theme={theme}"));
        }

        if let Some(val) = self.scroll_sensitivity {
            lines.push(format!("scroll_sensitivity={val}"));
        }

        if let Some(val) = self.ui_scale {
            lines.push(format!("ui_scale={val}"));
        }

        if let Some(v) = self.window_x { lines.push(format!("window_x={v}")); }
        if let Some(v) = self.window_y { lines.push(format!("window_y={v}")); }
        if let Some(v) = self.window_w { lines.push(format!("window_w={v}")); }
        if let Some(v) = self.window_h { lines.push(format!("window_h={v}")); }
        if let Some(v) = self.window_maximized { lines.push(format!("window_maximized={}", if v { "true" } else { "false" })); }

        if let Some(ref p) = self.default_remote_browser_path {
            lines.push(format!("default_remote_browser_path={p}"));
        }
        if let Some(ref p) = self.default_local_dialog_path {
            lines.push(format!("default_local_dialog_path={p}"));
        }

        if self.default_connect_action == "vscode" {
            lines.push("default_connect_action=vscode".to_string());
        }
        for pem in &self.ssh_pem_library {
            lines.push(format!("ssh_pem_known={pem}"));
        }
        for (profile, pem) in &self.ssh_pem_default {
            lines.push(format!("ssh_pem.{profile}={pem}"));
        }
        for (profile, user) in &self.ssh_user_default {
            lines.push(format!("ssh_user.{profile}={user}"));
        }
        for (instance, pem) in &self.ssh_pem_instance {
            lines.push(format!("ssh_pem_instance.{instance}={pem}"));
        }
        for (profile, suppressed) in &self.vscode_pem_suppressed {
            if *suppressed {
                lines.push(format!("vscode_pem_suppress.{profile}=1"));
            }
        }

        for profile in &self.profiles {
            lines.push(format!(
                "profile_name.{}={}",
                profile.profile_id, profile.display_name
            ));
            if !profile.account_id.is_empty() {
                lines.push(format!(
                    "profile_account_id.{}={}",
                    profile.profile_id, profile.account_id
                ));
            }
            if let Some(region) = &profile.region {
                lines.push(format!(
                    "profile_region.{}={}",
                    profile.profile_id, region
                ));
            }
        }

        if let Some(last) = &self.last_selected_profile {
            lines.push(format!("last_selected_profile={last}"));
        }

        if !self.account_colors_enabled {
            lines.push("account_colors_enabled=0".to_string());
        }

        if !self.reset_filter_on_profile_switch {
            lines.push("reset_filter_on_profile_switch=0".to_string());
        }

        if !self.excluded_envs.is_empty() {
            lines.push(format!("excluded_envs={}", self.excluded_envs.join(",")));
        }

        if self.shared_env_default_applied {
            lines.push("shared_env_default_applied=true".to_string());
        }

        for (profile, color) in &self.account_colors {
            lines.push(format!("account_color.{profile}={color}"));
        }

        for (account, region) in &self.account_regions {
            lines.push(format!("account_region.{account}={region}"));
        }

        for (scope, ids) in &self.favorites {
            lines.push(format!("favorite.{scope}={}", ids.join(",")));
        }

        for recent in &self.recents {
            lines.push(format!(
                "recent={}|{}|{}|{}|{}",
                recent.account_id,
                recent.region,
                recent.instance_id,
                recent.name.clone().unwrap_or_default(),
                recent.timestamp_unix
            ));
        }

        for (scope, filters) in &self.saved_filters {
            for saved in filters {
                lines.push(format!(
                    "saved_filter.{scope}={}|{}|{}|{}|{}|{}",
                    saved.name,
                    saved.include_terms.join(","),
                    saved.exclude_terms.join(","),
                    saved.states.join(","),
                    if saved.only_ssm_managed { "1" } else { "0" },
                    saved.pinned_ids.join(",")
                ));
            }
        }

        for preset in &self.port_forward_presets {
            lines.push(format!(
                "port_forward_preset={}|{}|{}",
                preset.name, preset.local_port, preset.remote_port
            ));
        }

        lines.push(String::new());
        lines.join("\n")
    }
}

fn parse_recent(raw: &str) -> Option<RecentConnection> {
    let fields: Vec<&str> = raw.split('|').collect();
    if fields.len() < 5 {
        return None;
    }

    Some(RecentConnection {
        account_id: fields[0].trim().to_string(),
        region: fields[1].trim().to_string(),
        instance_id: fields[2].trim().to_string(),
        name: {
            let value = fields[3].trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        },
        timestamp_unix: fields[4].trim().parse::<u64>().ok()?,
    })
}

fn parse_saved_filter(raw: &str) -> Option<SavedFilter> {
    let fields: Vec<&str> = raw.split('|').collect();
    if fields.len() < 4 {
        return None;
    }

    let name = fields[0].trim();
    if name.is_empty() {
        return None;
    }

    if fields.len() >= 6 {
        return Some(SavedFilter {
            name: name.to_string(),
            include_terms: split_csv(fields[1]),
            exclude_terms: split_csv(fields[2]),
            states: split_csv(fields[3]),
            only_ssm_managed: matches!(fields[4].trim(), "1" | "true" | "TRUE"),
            pinned_ids: split_csv(fields[5]),
        });
    }
    if fields.len() >= 5 {
        return Some(SavedFilter {
            name: name.to_string(),
            include_terms: split_csv(fields[1]),
            exclude_terms: split_csv(fields[2]),
            states: split_csv(fields[3]),
            only_ssm_managed: matches!(fields[4].trim(), "1" | "true" | "TRUE"),
            pinned_ids: Vec::new(),
        });
    }

    let include_terms = {
        let value = fields[1].trim();
        if value.is_empty() {
            Vec::new()
        } else {
            vec![value.to_string()]
        }
    };

    Some(SavedFilter {
        name: name.to_string(),
        include_terms,
        exclude_terms: Vec::new(),
        states: split_csv(fields[2]),
        only_ssm_managed: matches!(fields[3].trim(), "1" | "true" | "TRUE"),
        pinned_ids: Vec::new(),
    })
}

fn parse_port_forward_preset(raw: &str) -> Option<PortForwardPreset> {
    let fields: Vec<&str> = raw.split('|').collect();
    if fields.len() < 3 {
        return None;
    }

    let name = fields[0].trim();
    if name.is_empty() {
        return None;
    }

    Some(PortForwardPreset {
        name: name.to_string(),
        local_port: fields[1].trim().parse::<u16>().ok()?,
        remote_port: fields[2].trim().parse::<u16>().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config() {
        let raw = "\
            default_mode=live\n\
            default_terminal=kitty\n\
            default_region=us-west-2\n\
            env_keys=Env,Environment\n\
            account_region.123=us-east-1\n\
            favorite.123:us-east-1=i-1,i-2\n\
            recent=123|us-east-1|i-1|api|1730000000\n\
            saved_filter.123:us-east-1=prod-only|prod|running|1\n\
            port_forward_preset=ssh|2222|22\n\
        ";

        let cfg = AppConfig::parse(raw);
        assert_eq!(cfg.default_mode, Mode::Live);
        assert_eq!(cfg.default_terminal.as_deref(), Some("kitty"));
        assert_eq!(cfg.default_region.as_deref(), Some("us-west-2"));
        assert_eq!(
            cfg.account_regions.get("123").map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(cfg.favorites_for_scope("123", "us-east-1").len(), 2);
        assert_eq!(cfg.recents.len(), 1);
        assert_eq!(cfg.saved_filters_for_scope("123", "us-east-1").len(), 1);
        assert!(cfg.port_forward_presets.iter().any(|p| p.name == "ssh"));
    }

    #[test]
    fn toggle_favorite_roundtrip() {
        let mut cfg = AppConfig::default();
        assert!(cfg.toggle_favorite("123", "us-east-1", "i-1"));
        assert!(cfg.is_favorite("123", "us-east-1", "i-1"));
        assert!(!cfg.toggle_favorite("123", "us-east-1", "i-1"));
        assert!(!cfg.is_favorite("123", "us-east-1", "i-1"));
    }

    #[test]
    fn recents_are_deduped_and_limited() {
        let mut cfg = AppConfig::default();
        cfg.recents.clear();

        for idx in 0..30u64 {
            cfg.add_recent_connection(RecentConnection {
                account_id: "123".to_string(),
                region: "us-east-1".to_string(),
                instance_id: format!("i-{idx}"),
                name: None,
                timestamp_unix: idx,
            });
        }

        assert_eq!(cfg.recents.len(), 20);
        assert_eq!(cfg.recents[0].instance_id, "i-29");

        cfg.add_recent_connection(RecentConnection {
            account_id: "123".to_string(),
            region: "us-east-1".to_string(),
            instance_id: "i-20".to_string(),
            name: Some("x".to_string()),
            timestamp_unix: 100,
        });

        assert_eq!(cfg.recents[0].instance_id, "i-20");
        assert_eq!(cfg.recents.len(), 20);
    }

    #[test]
    fn save_delete_filters() {
        let mut cfg = AppConfig::default();
        cfg.upsert_saved_filter(
            "123",
            "us-east-1",
            SavedFilter {
                name: "prod".to_string(),
                include_terms: vec!["prod".to_string()],
                exclude_terms: Vec::new(),
                states: vec!["running".to_string()],
                only_ssm_managed: true,
                pinned_ids: Vec::new(),
            },
        );

        assert_eq!(cfg.saved_filters_for_scope("123", "us-east-1").len(), 1);
        assert!(cfg.delete_saved_filter("123", "us-east-1", "prod"));
        assert_eq!(cfg.saved_filters_for_scope("123", "us-east-1").len(), 0);
    }

    #[test]
    fn config_roundtrip_keeps_new_fields() {
        let mut cfg = AppConfig::default();
        cfg.toggle_favorite("123", "us-east-1", "i-abc");
        cfg.add_recent_connection(RecentConnection {
            account_id: "123".to_string(),
            region: "us-east-1".to_string(),
            instance_id: "i-abc".to_string(),
            name: Some("api".to_string()),
            timestamp_unix: 11,
        });
        cfg.upsert_saved_filter(
            "123",
            "us-east-1",
            SavedFilter {
                name: "all".to_string(),
                include_terms: vec!["api".to_string(), "prod".to_string()],
                exclude_terms: vec!["legacy".to_string()],
                states: vec![],
                only_ssm_managed: false,
                pinned_ids: Vec::new(),
            },
        );
        cfg.upsert_port_forward_preset(PortForwardPreset {
            name: "mysql".to_string(),
            local_port: 3306,
            remote_port: 3306,
        });
        cfg.scroll_sensitivity = Some(5.0);

        let raw = cfg.to_text();
        let parsed = AppConfig::parse(&raw);

        assert!(parsed.is_favorite("123", "us-east-1", "i-abc"));
        assert_eq!(parsed.recents_for_scope("123", "us-east-1").len(), 1);
        assert_eq!(parsed.saved_filters_for_scope("123", "us-east-1").len(), 1);
        let saved = parsed.saved_filters_for_scope("123", "us-east-1");
        assert_eq!(saved[0].include_terms, vec!["api", "prod"]);
        assert_eq!(saved[0].exclude_terms, vec!["legacy"]);
        assert!(parsed
            .port_forward_presets
            .iter()
            .any(|p| p.name == "mysql"));
        assert_eq!(parsed.scroll_sensitivity, Some(5.0));
    }

    #[test]
    fn parse_saved_filter_backward_compatible_with_old_format() {
        let raw = "saved_filter.123:us-east-1=prod-only|prod|running|1\n";
        let cfg = AppConfig::parse(raw);
        let saved = cfg.saved_filters_for_scope("123", "us-east-1");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].include_terms, vec!["prod"]);
        assert!(saved[0].exclude_terms.is_empty());
        assert_eq!(saved[0].states, vec!["running"]);
        assert!(saved[0].only_ssm_managed);
    }
}
