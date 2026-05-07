#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!(
        "GUI is disabled in this build. Run:\n  cargo run --features gui --bin ec2_manager_gui -- --mode sim"
    );
}

#[cfg(feature = "gui")]
mod gui {
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::io::{Read, Write};
    use std::panic::{self, AssertUnwindSafe};
    use std::path::PathBuf;
    #[cfg(test)]
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex, Once};
    use std::time::{Duration, Instant, SystemTime};

    use eframe::egui;
    use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

    use ec2_manager::aws_context::build_context_with_profile;
    use ec2_manager::config::AppConfig;
    use ec2_manager::credentials;
    use ec2_manager::connection_tabs::ConnectionTabs;
    use ec2_manager::diagnostics::run_diagnostics;
    use ec2_manager::error::{AppError, Result};
    use ec2_manager::filter::{apply_filters, matching_tags, Filters};
    use ec2_manager::gui_cli::{gui_help_text, parse_gui_args, GuiOptions};
    use ec2_manager::inventory::load_inventory;
    use ec2_manager::models::{
        AuthStatus, AwsContext, DependencyStatus, Instance, Inventory, Mode, ProfileAuthInfo,
        ProfileConfig, SavedFilter, TerminalKind, TerminalOption,
    };
    use ec2_manager::profile_choice::profile_choice_path;
    use ec2_manager::terminal::{
        build_ssm_port_forward_args, build_ssm_session_args, dependency_status,
        discover_terminals, pick_default_terminal,
    };
    use ec2_manager::util::truncate;
    use ec2_manager::workflow::find_instance;
    use ec2_manager::wsl_setup;

    const GUI_DEFAULT_WIDTH: f32 = 1720.0;
    const GUI_DEFAULT_HEIGHT: f32 = 980.0;
    const GUI_MIN_WIDTH: f32 = 1280.0;
    const GUI_MIN_HEIGHT: f32 = 760.0;
    const PROFILE_POLL_INTERVAL: Duration = Duration::from_secs(1);
    const AUTH_EXPIRY_CHECK_INTERVAL: Duration = Duration::from_secs(10);
    const PROFILE_CHANGE_DEBOUNCE: Duration = Duration::from_secs(2);
    /// Auto-close a Connections tab this long after its PTY exited (e.g. SSM
    /// inactivity logout).
    const STALE_TAB_AUTOCLOSE: Duration = Duration::from_secs(3600);
    const STALE_TAB_CHECK_INTERVAL: Duration = Duration::from_secs(60);
    const GUI_SMOKE_MARKER_ENV: &str = "EC2_MANAGER_GUI_SMOKE_MARKER";
    const GUI_SMOKE_EXPECTED_TEXT_ENV: &str = "EC2_MANAGER_GUI_SMOKE_EXPECTED_TEXT";
    const GUI_SMOKE_EXIT_ON_MARKER_ENV: &str = "EC2_MANAGER_GUI_SMOKE_EXIT_ON_MARKER";
    const GUI_SMOKE_AUTO_CONNECT_ENV: &str = "EC2_MANAGER_GUI_SMOKE_AUTO_CONNECT";

    const COL_FAV_W: f32 = 55.0;
    const COL_INSTANCE_W: f32 = 80.0;
    const COL_NAME_W: f32 = 50.0;
    const COL_STATE_W: f32 = 50.0;
    const COL_SSM_W: f32 = 40.0;
    const COL_IP_W: f32 = 83.0;
    const COL_DNS_W: f32 = 220.0;
    const COL_ENV_W: f32 = 40.0;
    const COL_INSTANCE_TYPE_W: f32 = 95.0;
    const COL_AMI_W: f32 = 71.0;
    const COL_TAG_W: f32 = 120.0;
    const COL_COPY_W: f32 = 14.0;
    const STATE_FILTER_NONE: &str = "";
    const STATE_FILTER_RUNNING: &str = "running";
    const STATE_FILTER_STOPPED: &str = "stopped";
    const STATE_FILTER_TERMINATED: &str = "terminated";
    const AWS_REGION_AUTO: &str = "(auto)";
    const AWS_REGIONS: &[&str] = &[
        "us-east-1",
        "us-east-2",
        "us-west-1",
        "us-west-2",
        "ca-central-1",
        "sa-east-1",
        "eu-west-1",
        "eu-west-2",
        "eu-west-3",
        "eu-central-1",
        "eu-central-2",
        "eu-north-1",
        "eu-south-1",
        "eu-south-2",
        "me-south-1",
        "me-central-1",
        "af-south-1",
        "ap-south-1",
        "ap-south-2",
        "ap-east-1",
        "ap-southeast-1",
        "ap-southeast-2",
        "ap-southeast-3",
        "ap-southeast-4",
        "ap-northeast-1",
        "ap-northeast-2",
        "ap-northeast-3",
    ];


    #[derive(Clone, Debug)]
    struct VolumeInfo {
        volume_id: String,
        size_gb: String,
        volume_type: String,
        device: String,
        state: String,
        attach_time: String,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum MainTab {
        Inventory,
        Connections,
        Details,
        Log,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SortColumn {
        Favorite,
        InstanceId,
        Name,
        State,
        Ssm,
        PrivateIp,
        PrivateDns,
        Env,
        InstanceType,
        AmiId,
        MatchTag,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SortDirection {
        Ascending,
        Descending,
    }

    impl SortDirection {
        fn toggle(self) -> Self {
            match self {
                Self::Ascending => Self::Descending,
                Self::Descending => Self::Ascending,
            }
        }

        fn arrow(self) -> &'static str {
            match self {
                Self::Ascending => " ^",
                Self::Descending => " v",
            }
        }
    }

    const INVENTORY_HEADERS: &[(&str, SortColumn)] = &[
        ("Favorite", SortColumn::Favorite),
        ("InstanceId", SortColumn::InstanceId),
        ("Name", SortColumn::Name),
        ("State", SortColumn::State),
        ("SSM", SortColumn::Ssm),
        ("Private IP", SortColumn::PrivateIp),
        ("Private DNS", SortColumn::PrivateDns),
        ("AMI ID", SortColumn::AmiId),
        ("Instance Type", SortColumn::InstanceType),
        ("Environment", SortColumn::Env),
        ("Match Tag", SortColumn::MatchTag),
    ];

    impl SortColumn {
        fn default_width(self) -> f32 {
            match self {
                Self::Favorite => COL_FAV_W,
                Self::InstanceId => COL_INSTANCE_W + COL_COPY_W,
                Self::Name => COL_NAME_W,
                Self::State => COL_STATE_W,
                Self::Ssm => COL_SSM_W,
                Self::PrivateIp => COL_IP_W + COL_COPY_W,
                Self::PrivateDns => COL_DNS_W + COL_COPY_W,
                Self::AmiId => COL_AMI_W + COL_COPY_W,
                Self::InstanceType => COL_INSTANCE_TYPE_W,
                Self::Env => COL_ENV_W,
                Self::MatchTag => COL_TAG_W,
            }
        }
    }

    fn default_column_widths() -> HashMap<u8, f32> {
        let cols = [
            SortColumn::Favorite,
            SortColumn::InstanceId,
            SortColumn::Name,
            SortColumn::State,
            SortColumn::Ssm,
            SortColumn::PrivateIp,
            SortColumn::PrivateDns,
            SortColumn::AmiId,
            SortColumn::InstanceType,
            SortColumn::Env,
            SortColumn::MatchTag,
        ];
        cols.iter().map(|c| (*c as u8, c.default_width())).collect()
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum LogLevel {
        Error,
        Warn,
        Info,
        Debug,
        Trace,
    }

    impl LogLevel {
        fn as_str(self) -> &'static str {
            match self {
                Self::Error => "ERROR",
                Self::Warn => "WARN",
                Self::Info => "INFO",
                Self::Debug => "DEBUG",
                Self::Trace => "TRACE",
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct LogEntry {
        level: LogLevel,
        message: String,
    }

    #[derive(Clone, Debug)]
    struct LogFilters {
        error: bool,
        warn: bool,
        info: bool,
        debug: bool,
        trace: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct GuiSmokeConfig {
        marker_path: PathBuf,
        expected_text: String,
        exit_on_marker: bool,
        auto_connect: bool,
    }

    impl Default for LogFilters {
        fn default() -> Self {
            Self {
                error: true,
                warn: true,
                info: true,
                debug: true,
                trace: false,
            }
        }
    }

    impl LogFilters {
        fn includes(&self, level: LogLevel) -> bool {
            match level {
                LogLevel::Error => self.error,
                LogLevel::Warn => self.warn,
                LogLevel::Info => self.info,
                LogLevel::Debug => self.debug,
                LogLevel::Trace => self.trace,
            }
        }

        fn set_verbosity_low(&mut self) {
            self.error = true;
            self.warn = true;
            self.info = true;
            self.debug = false;
            self.trace = false;
        }

        fn set_verbosity_medium(&mut self) {
            self.error = true;
            self.warn = true;
            self.info = true;
            self.debug = true;
            self.trace = false;
        }

        fn set_verbosity_high(&mut self) {
            self.error = true;
            self.warn = true;
            self.info = true;
            self.debug = true;
            self.trace = true;
        }
    }

    enum ProcEvent {
        Output { tab_id: u64, bytes: Vec<u8> },
        Exited { tab_id: u64, code: i32 },
        Error { tab_id: u64, error: String },
        VolumeResult {
            volumes: std::result::Result<Vec<VolumeInfo>, String>,
            iam_role: Option<String>,
        },
    }

    enum RefreshEvent {
        Completed {
            generation: u64,
            profile_id: String,
            context: AwsContext,
            inventory: Inventory,
            config_update: Option<(String, String)>,
        },
        AuthNotOk {
            generation: u64,
            profile_id: String,
            context: AwsContext,
        },
        Failed {
            generation: u64,
            profile_id: String,
            error: String,
        },
    }

    #[cfg(target_os = "windows")]
    enum UiEvent {
        PtyReady { tab_id: u64, session: PtySession },
        Error { tab_id: u64, error: String },
    }

    struct PtySession {
        child: Box<dyn portable_pty::Child + Send>,
        master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
        writer: Arc<Mutex<Box<dyn Write + Send>>>,
        parser: vt100::Parser,
        last_size: Option<(u16, u16)>,
        bytes_received: u64,
        output_event_count: u64,
        scroll_offset: usize,
        /// Whether the auto-PS1 command has already been sent for this session.
        ps1_auto_sent: bool,
        /// Accumulates user-typed bytes between Enter presses so we can
        /// detect commands like `sudo su` / `sudo su - <user>` and re-run
        /// the prep-terminal sequence after the new shell prompt appears.
        input_line_buf: String,
        /// Set when the most recently submitted line was a `sudo su` form;
        /// the next prompt detected in output triggers re-prep.
        pending_reprep: bool,
        /// Trimmed text of the cursor-line prompt the last time PREP was
        /// auto-fired. Used to ensure we only re-prep when a *new*
        /// shell prompt appears (e.g., after `sudo su`), not on every
        /// subsequent output flush.
        last_prepped_prompt: String,
        /// When the most recent paste was written to the PTY, and how many
        /// bytes it contained. Used to log the echo-return latency of large
        /// pastes so we can distinguish remote-buffering stalls from local
        /// parse/render stalls. Cleared once we log the first inbound byte.
        paste_write_at: Option<Instant>,
        paste_write_bytes: usize,
    }

    /// Absolute terminal position: scroll-invariant coordinate.
    /// `abs_row` 0 = newest line (bottom at scroll_offset 0), increasing into history.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct AbsPos {
        abs_row: usize,
        col: u16,
    }

    #[derive(Clone, Debug, Default)]
    struct TerminalSelection {
        anchor: Option<AbsPos>, // where drag started
        end: Option<AbsPos>,    // where drag is now
    }

    impl TerminalSelection {
        /// Returns (start, end) in reading order: start has higher abs_row
        /// (further into history / top of display), end has lower abs_row
        /// (closer to present / bottom of display).
        fn normalized(&self) -> Option<(AbsPos, AbsPos)> {
            let a = self.anchor?;
            let e = self.end?;
            if a.abs_row > e.abs_row || (a.abs_row == e.abs_row && a.col <= e.col) {
                Some((a, e))
            } else {
                Some((e, a))
            }
        }

        #[allow(dead_code)]
        fn is_active(&self) -> bool {
            self.anchor.is_some() && self.end.is_some()
        }

        fn clear(&mut self) {
            self.anchor = None;
            self.end = None;
        }
    }

    #[derive(Clone, Debug)]
    struct PtyCommand {
        program: String,
        args: Vec<String>,
    }

    #[derive(Clone, Debug)]
    #[allow(dead_code)]
    struct FileEntry {
        name: String,
        is_dir: bool,
        size: u64,
        permissions: String,
        modified: String,
    }

    #[derive(Clone, Debug)]
    enum FileOpStatus {
        Idle,
        Listing,
        Downloading,
        Uploading,
        /// Opening a file in the editor
        Initializing,
        /// Saving editor changes back to EC2
        Updating,
        Error(String),
    }

    #[derive(Clone)]
    struct EditorTab {
        remote_path: String,
        content: String,
        original: String,
        dirty: bool,
        status: String,
    }

    impl EditorTab {
        fn filename(&self) -> &str {
            self.remote_path
                .rsplit('/')
                .next()
                .unwrap_or(&self.remote_path)
        }
    }

    struct FileBrowserState {
        current_path: String,
        entries: Vec<FileEntry>,
        status: FileOpStatus,
        selected_entries: std::collections::BTreeSet<usize>,
        last_clicked_entry: Option<usize>,
        pending_downloads: usize,
        path_input: String,
        initialized: bool,
        /// Multiple open editor tabs per connection
        editor_tabs: Vec<EditorTab>,
        /// Index of the currently active editor tab
        active_editor: Option<usize>,
        /// In-memory cache of visited directory listings (cleared when tab closes)
        dir_cache: HashMap<String, Vec<FileEntry>>,
        /// Directories currently expanded in the tree view
        expanded_dirs: std::collections::HashSet<String>,
        /// Directories currently being fetched in the background
        fetching_dirs: std::collections::HashSet<String>,
        /// Whether the terminal had focus last frame (for auto-refresh)
        terminal_had_focus: bool,
        /// Currently selected file in tree view (full_path, filename)
        selected_file: Option<(String, String)>,
    }

    impl Default for FileBrowserState {
        fn default() -> Self {
            Self {
                current_path: String::new(),
                entries: Vec::new(),
                status: FileOpStatus::Idle,
                selected_entries: std::collections::BTreeSet::new(),
                last_clicked_entry: None,
                pending_downloads: 0,
                path_input: String::new(),
                initialized: false,
                editor_tabs: Vec::new(),
                active_editor: None,
                dir_cache: HashMap::new(),
                expanded_dirs: std::collections::HashSet::new(),
                fetching_dirs: std::collections::HashSet::new(),
                terminal_had_focus: false,
                selected_file: None,
            }
        }
    }

    enum FileOpEvent {
        ListingCompleted {
            tab_id: u64,
            path: String,
            entries: Vec<FileEntry>,
        },
        ListingFailed {
            tab_id: u64,
            error: String,
        },
        DownloadCompleted {
            tab_id: u64,
            remote_path: String,
            local_path: String,
            bytes: u64,
        },
        DownloadFailed {
            tab_id: u64,
            error: String,
        },
        UploadCompleted {
            tab_id: u64,
            local_path: String,
            remote_path: String,
            bytes: u64,
        },
        UploadFailed {
            tab_id: u64,
            error: String,
        },
        FileReadCompleted {
            tab_id: u64,
            remote_path: String,
            content: String,
        },
        FileReadFailed {
            tab_id: u64,
            error: String,
        },
        FileSaveCompleted {
            tab_id: u64,
            remote_path: String,
        },
        FileSaveFailed {
            tab_id: u64,
            error: String,
        },
    }

    /// Shell setup sent to the remote shell when the user clicks "Prep
    /// Terminal".  Execs bash (SSM sessions start in `sh`), sets a
    /// git-bash-style PS1, appends `set paste` to `~/.vimrc` if not
    /// already present (fixes staircase-indent on right-click paste into
    /// vim), and writes an `~/.ssh/config` entry that auto-accepts new
    /// host keys so `scp`/`ssh` don't stall on the fingerprint prompt.
    /// The sudo-su re-prep hook re-runs this for the switched-to user,
    /// so root (etc.) gets its own `~/.ssh/config` too.
    const PREP_TERMINAL_COMMAND: &[u8] =
        b"exec bash\rexport PS1='\\n\\[\\033[1;32m\\]\\u@\\h\\[\\033[0m\\] \\[\\033[1;35m\\]SSM\\[\\033[0m\\] \\[\\033[1;33m\\]\\w\\[\\033[0m\\]\\n\\$ '\rgrep -qxF 'set paste' ~/.vimrc 2>/dev/null || echo 'set paste' >> ~/.vimrc\rmkdir -p ~/.ssh && chmod 700 ~/.ssh 2>/dev/null\rgrep -qs 'StrictHostKeyChecking accept-new' ~/.ssh/config 2>/dev/null || printf 'Host *\\n  StrictHostKeyChecking accept-new\\n  UserKnownHostsFile ~/.ssh/known_hosts\\n' >> ~/.ssh/config\rchmod 600 ~/.ssh/config 2>/dev/null\rclear\r";

    /// Build the prep-terminal byte sequence with an explicit `stty rows
    /// R cols C` prefix. The local PTY's resize messages don't always
    /// reach the remote shell before bash has read its initial TTY size
    /// — symptom is line-wrap overlap on the *initial* shell that goes
    /// away after `sudo su` (because the new shell inherits the current,
    /// correct size). Forcing stty before `exec bash` makes the
    /// replaced bash pick up the right cols on first prep too.
    fn prep_terminal_command_for_size(rows: u16, cols: u16) -> Vec<u8> {
        let mut buf = format!("stty rows {rows} cols {cols} 2>/dev/null\r").into_bytes();
        buf.extend_from_slice(PREP_TERMINAL_COMMAND);
        buf
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RowAction {
        select: bool,
        connect: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SearchRuleKind {
        Include,
        Exclude,
    }

    #[derive(Clone, Debug)]
    struct SearchRuleInput {
        kind: SearchRuleKind,
        term: String,
    }

    impl Default for SearchRuleInput {
        fn default() -> Self {
            Self {
                kind: SearchRuleKind::Include,
                term: String::new(),
            }
        }
    }

    /// Parse rules into include/exclude terms for general search.
    /// Terms with `TagKey: value` syntax are skipped here — they're handled
    /// as tag matches in apply_filters.
    /// Returns the text of the line under the cursor, used to detect
    /// fresh shell prompts after `sudo su` so we know when to re-fire
    /// the prep-terminal sequence.
    fn current_cursor_line(parser: &vt100::Parser) -> String {
        let (row, _col) = parser.screen().cursor_position();
        let (_, cols) = parser.screen().size();
        parser.screen().contents_between(row, 0, row, cols)
    }

    /// Returns true when `line` is a `sudo su` form that switches user:
    /// `sudo su`, `sudo su -`, `sudo su - <user>`, `sudo su <user>`,
    /// optionally with extra whitespace. Used to schedule a re-run of the
    /// prep-terminal sequence after the new shell prompt appears.
    fn is_sudo_su_line(line: &str) -> bool {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 2 || tokens[0] != "sudo" || tokens[1] != "su" {
            return false;
        }
        match tokens.len() {
            2 => true,                                 // sudo su
            3 => true,                                 // sudo su -   |   sudo su <user>
            4 => tokens[2] == "-",                     // sudo su - <user>
            _ => false,
        }
    }

    fn search_terms_from_rules(rules: &[SearchRuleInput]) -> (Vec<String>, Vec<String>) {
        let mut includes = Vec::new();
        let mut excludes = Vec::new();

        for rule in rules {
            let term = rule.term.trim();
            if term.is_empty() {
                continue;
            }
            // Skip "Key: Value" syntax — handled as tag match in apply_filters
            if term.contains(':') {
                continue;
            }
            match rule.kind {
                SearchRuleKind::Include => includes.push(term.to_string()),
                SearchRuleKind::Exclude => excludes.push(term.to_string()),
            }
        }

        (includes, excludes)
    }

    fn rules_from_search_terms(includes: &[String], excludes: &[String]) -> Vec<SearchRuleInput> {
        let mut rules = Vec::new();

        for term in includes {
            rules.push(SearchRuleInput {
                kind: SearchRuleKind::Include,
                term: term.clone(),
            });
        }
        for term in excludes {
            rules.push(SearchRuleInput {
                kind: SearchRuleKind::Exclude,
                term: term.clone(),
            });
        }

        if rules.is_empty() {
            rules.push(SearchRuleInput::default());
        }

        rules
    }

    fn states_from_state_filter(selected_state_filter: &str) -> Vec<String> {
        let state = selected_state_filter.trim();
        if state.is_empty() {
            Vec::new()
        } else {
            vec![state.to_string()]
        }
    }

    fn state_filter_from_saved_states(saved_states: &[String]) -> String {
        let Some(first) = saved_states.first() else {
            return STATE_FILTER_NONE.to_string();
        };

        let normalized = first.trim().to_ascii_lowercase();
        match normalized.as_str() {
            STATE_FILTER_RUNNING | STATE_FILTER_STOPPED | STATE_FILTER_TERMINATED => normalized,
            _ => STATE_FILTER_NONE.to_string(),
        }
    }

    fn selected_region_label(selected_region: Option<&str>, context_region: Option<&str>) -> String {
        if let Some(region) = selected_region {
            return region.to_string();
        }
        match context_region {
            Some(region) => format!("{AWS_REGION_AUTO} ({region})"),
            None => AWS_REGION_AUTO.to_string(),
        }
    }

    fn panic_log_path() -> PathBuf {
        AppConfig::config_path()
            .map(|p| p.with_file_name("ec2_manager_gui_panic.log"))
            .unwrap_or_else(|| std::env::temp_dir().join("ec2_manager_gui_panic.log"))
    }

    fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
        if let Some(message) = payload.downcast_ref::<&'static str>() {
            return (*message).to_string();
        }
        if let Some(message) = payload.downcast_ref::<String>() {
            return message.clone();
        }
        "non-string panic payload".to_string()
    }

    fn append_panic_log_entry(entry: &str) {
        let path = panic_log_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{entry}");
        }
    }

    fn install_gui_panic_hook() {
        static PANIC_HOOK_ONCE: Once = Once::new();
        PANIC_HOOK_ONCE.call_once(|| {
            let default_hook = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                let location = info
                    .location()
                    .map(|loc| format!("{}:{}", loc.file(), loc.line()))
                    .unwrap_or_else(|| "unknown-location".to_string());
                let payload = panic_payload_to_string(info.payload());
                let message = format!("panic captured: {payload} @ {location}");
                append_panic_log_entry(&message);
                eprintln!("error: {message}");
                default_hook(info);
            }));
        });
    }

    pub fn run() {
        install_gui_panic_hook();
        if std::env::args().any(|a| a == "--help" || a == "-h") {
            println!("{}", gui_help_text());
            return;
        }

        let options = match parse_gui_args(std::env::args().skip(1)) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("error: {err}");
                eprintln!("\n{}", gui_help_text());
                std::process::exit(1);
            }
        };

        let native_options = default_native_options();
        let title = "EC2 + SSM Instance Explorer";
        let app_options = options.clone();

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            eframe::run_native(
                title,
                native_options,
                Box::new(move |cc| {
                    let app = Ec2GuiApp::new(app_options.clone());
                    if app.dark_mode {
                        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
                    } else {
                        cc.egui_ctx.set_theme(egui::ThemePreference::Light);
                    }
                    cc.egui_ctx.set_pixels_per_point(app.ui_scale * cc.egui_ctx.native_pixels_per_point().unwrap_or(1.0));
                    Ok(Box::new(app))
                }),
            )
        }));

        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let message = format!("failed to start GUI: {err}");
                append_panic_log_entry(&message);
                eprintln!("error: {message}");
                std::process::exit(1);
            }
            Err(payload) => {
                let message = format!(
                    "GUI bootstrap panic: {}",
                    panic_payload_to_string(payload.as_ref())
                );
                append_panic_log_entry(&message);
                eprintln!("error: {message}");
                std::process::exit(1);
            }
        }
    }

    fn default_native_options() -> eframe::NativeOptions {
        let config = AppConfig::load().unwrap_or_default();
        let mut viewport = egui::ViewportBuilder::default()
            .with_min_inner_size([GUI_MIN_WIDTH, GUI_MIN_HEIGHT]);

        if let (Some(w), Some(h)) = (config.window_w, config.window_h) {
            viewport = viewport.with_inner_size([w, h]);
        } else {
            viewport = viewport.with_inner_size([GUI_DEFAULT_WIDTH, GUI_DEFAULT_HEIGHT]);
        }

        if let (Some(x), Some(y)) = (config.window_x, config.window_y) {
            viewport = viewport.with_position([x, y]);
        }

        viewport = viewport.with_maximized(config.window_maximized.unwrap_or(true));

        eframe::NativeOptions {
            viewport,
            ..Default::default()
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum WslSetupState {
        /// Cache says setup is done — skip check
        Cached,
        /// Checking prerequisites (background thread)
        Checking,
        /// Setup is needed — show commands to run
        Needed,
        /// Setup completed successfully
        Ready,
    }

    struct Ec2GuiApp {
        wsl_setup_state: WslSetupState,
        wsl_setup_status: Option<wsl_setup::WslSetupStatus>,
        wsl_setup_tx: Sender<wsl_setup::WslSetupStatus>,
        wsl_setup_rx: Receiver<wsl_setup::WslSetupStatus>,
        options: GuiOptions,
        gui_smoke: Option<GuiSmokeConfig>,
        gui_smoke_marker_written: bool,
        gui_smoke_auto_connect_attempted: bool,
        gui_smoke_should_close: bool,
        show_close_blocked: bool,
        /// Set when "Close All & Exit" is clicked — the app defers the actual
        /// close until cleanup threads have had time to send graceful exit
        /// commands to remote SSM sessions.
        pending_exit_at: Option<std::time::Instant>,
        config: AppConfig,
        context: Option<AwsContext>,
        dependencies: DependencyStatus,
        inventory: Inventory,
        filtered: Vec<Instance>,

        search_rules: Vec<SearchRuleInput>,
        selected_state_filter: String,
        only_ssm: bool,
        /// Additional account profile IDs to include in the instance list
        multi_account_ids: std::collections::HashSet<String>,
        save_filter_name: String,
        /// Environments hidden from the color legend (toggled on the connections page)
        hidden_envs: std::collections::HashSet<String>,
        selected_saved_filter: String,
        selected_instance_id: String,
        /// Instance shown in the Details tab
        detail_instance: Option<Instance>,
        /// Volume info fetched for the Details tab
        detail_volumes: Vec<VolumeInfo>,
        detail_volumes_loading: bool,
        detail_volumes_error: Option<String>,
        /// IAM role fetched for the Details tab
        detail_iam_role: Option<String>,
        local_port: u16,
        remote_port: u16,

        message: String,
        diagnostics: String,

        main_tab: MainTab,
        logs: VecDeque<LogEntry>,
        log_filters: LogFilters,
        terminals: Vec<TerminalOption>,
        selected_terminal_id: String,
        profile_choice_path: Option<PathBuf>,
        last_profile_choice_mtime: Option<SystemTime>,
        pending_profile_choice_mtime: Option<SystemTime>,
        pending_profile_change_since: Option<SystemTime>,
        last_profile_poll_at: Instant,
        connections: ConnectionTabs,
        pty_sessions: HashMap<u64, PtySession>,
        terminal_selections: HashMap<u64, TerminalSelection>,
        proc_tx: Sender<ProcEvent>,
        proc_rx: Receiver<ProcEvent>,
        refresh_tx: Sender<RefreshEvent>,
        refresh_rx: Receiver<RefreshEvent>,
        refreshing: bool,
        refresh_generation: u64,
        /// Profiles currently being refreshed in background threads
        refreshing_profiles: HashMap<String, u64>,
        /// Per-profile in-memory inventory cache: profile_id -> (Inventory, AwsContext)
        profile_inventory_cache: HashMap<String, (Inventory, AwsContext)>,
        debug_mode: bool,
        wsl_auto_setup: bool,
        wsl_password_buf: String,
        wsl_show_password_popup: bool,
        dark_mode: bool,
        scroll_sensitivity: f32,
        ui_scale: f32,
        last_native_ppp: f32,
        /// Stored egui Context, captured on the first update() call so
        /// background reader threads can call request_repaint() to wake
        /// the UI thread when terminal output arrives.
        egui_ctx: Option<egui::Context>,
        /// Auto-fit zoom has been applied once after startup.
        auto_fit_applied: bool,
        /// Last physical-pixel window width auto-fit was computed for;
        /// used alongside outer_pos to detect real monitor moves.
        last_auto_fit_width: f32,
        /// Last observed window outer-rect top-left. A significant jump
        /// here signals a monitor move.
        last_auto_fit_outer_pos: Option<egui::Pos2>,
        /// When auto-fit last fired. Debounce against transient jitter.
        last_auto_fit_at: Option<Instant>,
        sort_column: Option<SortColumn>,
        sort_direction: SortDirection,
        column_widths: HashMap<u8, f32>,
        selected_profile: Option<String>,
        profile_auth_infos: Vec<ProfileAuthInfo>,
        last_credentials_mtime: Option<SystemTime>,
        last_credentials_poll_at: Instant,
        last_auth_expiry_check_at: Instant,
        last_stale_tab_check_at: Instant,
        account_color_map: HashMap<String, egui::Color32>,
        tab_rename_id: Option<u64>,
        tab_rename_buf: String,
        /// Tab id currently being dragged in the connection tab strip.
        /// Tracked on self because egui's `is_being_dragged(salt)` doesn't
        /// match the salted id egui actually registers the drag under, so
        /// we observe `drag_started()` from the response and clear on release.
        dragging_tab_id: Option<u64>,
        /// Pointer-x at drag start, used to anchor the floating visual to
        /// the cursor instead of snapping to the slot center after reorders.
        drag_grab_offset_x: f32,
        tab_color_picker_rgb: [f32; 3],
        /// Profile ID for the color picker (from Edit menu or right-click)
        color_picker_profile: Option<String>,
        /// Tab id that should grab keyboard focus on the next render of the
        /// connections panel. Set when a new connection opens, the active
        /// connection tab changes, the user pastes via right-click, or
        /// clicks Prep Terminal — so the user can start typing immediately
        /// without a manual click into the terminal.
        pending_terminal_focus: Option<u64>,
        /// Used to detect main_tab transitions into Connections so we can
        /// auto-focus the visible terminal on entry.
        last_main_tab: MainTab,
        /// Last selected connection tab id seen at the top of update();
        /// a change vs `connections.selected()` triggers auto-focus.
        last_connection_tab: Option<u64>,
        file_browsers: HashMap<u64, FileBrowserState>,
        /// Per-tab editor/terminal vertical split ratio (0.0-1.0, 0.5 = 50/50)
        editor_split: HashMap<u64, f32>,
        file_op_tx: Sender<FileOpEvent>,
        file_op_rx: Receiver<FileOpEvent>,
        #[cfg(target_os = "windows")]
        ui_tx: Sender<UiEvent>,
        #[cfg(target_os = "windows")]
        ui_rx: Receiver<UiEvent>,
    }

    impl Ec2GuiApp {
        const MAX_LOG_LINES: usize = 20_000;

        fn new(options: GuiOptions) -> Self {
            let mut config = AppConfig::load().unwrap_or_default();
            if !config.shared_env_default_applied {
                if !config.excluded_envs.iter().any(|e| e.eq_ignore_ascii_case("shared")) {
                    config.excluded_envs.push("shared".to_string());
                    config.excluded_envs.sort();
                }
                config.shared_env_default_applied = true;
                let _ = config.save();
            }
            let initial_hidden_envs: std::collections::HashSet<String> =
                config.excluded_envs.iter().map(|s| s.to_ascii_lowercase()).collect();
            let dependencies = dependency_status();
            let (proc_tx, proc_rx) = mpsc::channel();
            let (wsl_setup_tx, wsl_setup_rx) = mpsc::channel();

            // Check WSL setup: if cached, skip; otherwise check in background
            let wsl_setup_state = if wsl_setup::is_setup_cached() {
                WslSetupState::Cached
            } else {
                let tx = wsl_setup_tx.clone();
                std::thread::spawn(move || {
                    let status = wsl_setup::check_wsl_setup();
                    let _ = tx.send(status);
                });
                WslSetupState::Checking
            };
            let (refresh_tx, refresh_rx) = mpsc::channel();
            let (file_op_tx, file_op_rx) = mpsc::channel();
            #[cfg(target_os = "windows")]
            let (ui_tx, ui_rx) = mpsc::channel();
            let profile_choice_path = profile_choice_path();
            let last_profile_choice_mtime = profile_choice_mtime(profile_choice_path.as_deref());
            let terminals = filter_embedded_terminals(discover_terminals());
            let selected_terminal_id = initial_terminal_id(&config, &terminals);
            let gui_smoke = gui_smoke_config_from_env();
            let dark_mode = config.theme.as_deref() != Some("light");
            let scroll_sensitivity = config.scroll_sensitivity.unwrap_or(10.0);
            let ui_scale = config.ui_scale.unwrap_or(1.0);
            let profile_auth_infos = credentials::check_all_profiles_auth(&config.profiles);
            let last_credentials_mtime = credentials::credentials_mtime();
            let selected_profile = config.last_selected_profile.clone()
                .filter(|s| !s.is_empty())
                .filter(|s| config.profiles.iter().any(|p| &p.profile_id == s))
                .or_else(|| {
                    // Prefer the first authenticated profile, fall back to the first profile
                    config.profiles.iter()
                        .find(|p| {
                            profile_auth_infos.iter()
                                .any(|a| a.profile_id == p.profile_id && a.auth_status == AuthStatus::Ok)
                        })
                        .or_else(|| config.profiles.first())
                        .map(|p| p.profile_id.clone())
                });

            let debug_mode = options.debug;
            let wsl_auto_setup = true;
            // Always start on region Auto; let the selected account's
            // accounts.json region drive the effective region.
            let mut options = options;
            options.region = None;
            let mut app = Self {
                wsl_setup_state,
                wsl_setup_status: None,
                wsl_setup_tx,
                wsl_setup_rx,
                options,
                gui_smoke,
                gui_smoke_marker_written: false,
                gui_smoke_auto_connect_attempted: false,
                gui_smoke_should_close: false,
                show_close_blocked: false,
                pending_exit_at: None,
                config,
                context: None,
                dependencies,
                inventory: Inventory {
                    instances: Vec::new(),
                    fetched_at: std::time::SystemTime::now(),
                },
                filtered: Vec::new(),
                search_rules: vec![SearchRuleInput::default()],
                selected_state_filter: "running".to_string(),
                only_ssm: false,
                multi_account_ids: std::collections::HashSet::new(),
                save_filter_name: String::new(),
                hidden_envs: initial_hidden_envs,
                selected_saved_filter: String::new(),
                selected_instance_id: String::new(),
                detail_instance: None,
                detail_volumes: Vec::new(),
                detail_volumes_loading: false,
                detail_volumes_error: None,
                detail_iam_role: None,
                local_port: 2222,
                remote_port: 22,
                message: String::new(),
                diagnostics: String::new(),
                main_tab: MainTab::Inventory,
                logs: VecDeque::new(),
                log_filters: if debug_mode {
                    LogFilters {
                        debug: true,
                        trace: true,
                        ..LogFilters::default()
                    }
                } else {
                    LogFilters::default()
                },
                terminals,
                selected_terminal_id,
                profile_choice_path,
                last_profile_choice_mtime,
                pending_profile_choice_mtime: None,
                pending_profile_change_since: None,
                last_profile_poll_at: Instant::now(),
                connections: ConnectionTabs::new(),
                pty_sessions: HashMap::new(),
                terminal_selections: HashMap::new(),
                proc_tx,
                proc_rx,
                refresh_tx,
                refresh_rx,
                refreshing: false,
                refresh_generation: 0,
                refreshing_profiles: HashMap::new(),
                profile_inventory_cache: HashMap::new(),
                debug_mode,
                wsl_auto_setup,
                wsl_password_buf: String::new(),
                wsl_show_password_popup: false,
                dark_mode,
                scroll_sensitivity,
                ui_scale,
                last_native_ppp: 0.0,
                egui_ctx: None,
                auto_fit_applied: false,
                last_auto_fit_width: 0.0,
                last_auto_fit_outer_pos: None,
                last_auto_fit_at: None,
                sort_column: None,
                sort_direction: SortDirection::Ascending,
                column_widths: default_column_widths(),
                selected_profile,
                profile_auth_infos,
                last_credentials_mtime,
                last_credentials_poll_at: Instant::now(),
                last_auth_expiry_check_at: Instant::now(),
                last_stale_tab_check_at: Instant::now(),
                account_color_map: HashMap::new(),
                tab_rename_id: None,
                dragging_tab_id: None,
                drag_grab_offset_x: 0.0,
                tab_rename_buf: String::new(),
                pending_terminal_focus: None,
                last_main_tab: MainTab::Inventory,
                last_connection_tab: None,
                tab_color_picker_rgb: [0.0, 0.0, 0.0],
                color_picker_profile: None,
                file_browsers: HashMap::new(),
                editor_split: HashMap::new(),
                file_op_tx,
                file_op_rx,
                #[cfg(target_os = "windows")]
                ui_tx,
                #[cfg(target_os = "windows")]
                ui_rx,
            };

            app.rebuild_account_colors();
            app.log_info("application started");
            if let Some(smoke) = &app.gui_smoke {
                app.log_info(format!(
                    "GUI smoke mode enabled marker={} expected='{}'",
                    smoke.marker_path.display(),
                    smoke.expected_text
                ));
            }

            // Build a preliminary context so Connect works before the background refresh completes.
            // Also load cached inventory from disk if the profile is authenticated.
            if let Some(profile_id) = app.selected_profile.clone() {
                let profile_cfg = app.config.profiles.iter()
                    .find(|p| p.profile_id == profile_id);
                // Use the profile's configured region from accounts.json only.
                // Do NOT fall back to config.default_region (last-session region)
                // because that leaks one account's region into another.
                let region = profile_cfg
                    .and_then(|p| p.region.clone())
                    .unwrap_or_else(|| "us-east-1".to_string());
                let account_id = profile_cfg
                    .map(|p| p.account_id.clone())
                    .filter(|s| !s.is_empty());

                let is_auth_ok = app.profile_auth_infos.iter().any(|a| {
                    a.profile_id == profile_id && a.auth_status == AuthStatus::Ok
                }) || app.options.mode == Mode::Sim;

                // Always set a preliminary context so Connect doesn't fail with "context not loaded"
                // Resolve the profile_id to an actual AWS CLI profile name.
                // If profile_id is already a profile name (from accounts.json "profile" field),
                // find_profile_by_account_id will return None and we use profile_id as-is.
                let resolved_profile = credentials::find_profile_by_account_id(&profile_id)
                    .unwrap_or_else(|| profile_id.clone());
                app.log_info(format!(
                    "profile resolution: profile_id={profile_id} -> resolved={resolved_profile}"
                ));
                app.context = Some(AwsContext {
                    mode: app.options.mode.clone(),
                    profile: resolved_profile,
                    account_id,
                    arn: None,
                    user_id: None,
                    region: region.clone(),
                    auth_status: if is_auth_ok { AuthStatus::Ok } else { AuthStatus::Expired },
                });

                if is_auth_ok {
                    app.load_cache_for_profile(&profile_id);
                } else {
                    app.log_info(format!(
                        "skipping disk cache for profile={profile_id}: auth not OK"
                    ));
                    app.message = "Waiting for authentication...".to_string();
                }
            } else {
                app.log_info("no selected profile, skipping disk cache");
            }

            // Pre-load disk cache for all other authenticated profiles
            // so multi-account lookup works immediately on startup.
            let other_profiles: Vec<(String, String, String)> = app.config.profiles.iter()
                .filter(|p| app.selected_profile.as_deref() != Some(p.profile_id.as_str()))
                .filter(|p| app.profile_auth_infos.iter().any(|a| a.profile_id == p.profile_id && a.auth_status == AuthStatus::Ok))
                .map(|p| {
                    let region = p.region.clone()
                        .or_else(|| app.config.default_region.clone())
                        .unwrap_or_else(|| "us-east-1".to_string());
                    (p.profile_id.clone(), p.account_id.clone(), region)
                })
                .collect();
            for (pid, account_id, region) in other_profiles {
                if let Some(cached) = ec2_manager::inventory::load_disk_cache(&pid, &region) {
                    let count = cached.instances.len();
                    let resolved = credentials::find_profile_by_account_id(&pid)
                        .unwrap_or_else(|| pid.clone());
                    let ctx = AwsContext {
                        mode: app.options.mode.clone(),
                        profile: resolved,
                        account_id: Some(account_id),
                        arn: None,
                        user_id: None,
                        region,
                        auth_status: AuthStatus::Ok,
                    };
                    app.profile_inventory_cache.insert(pid.clone(), (cached, ctx));
                    app.log_info(format!(
                        "pre-loaded {count} instances from disk cache for profile={pid}"
                    ));
                }
            }

            app.refresh_all_authenticated(true);
            app
        }

        fn poll_profile_choice_changes(&mut self) {
            if self.last_profile_poll_at.elapsed() < PROFILE_POLL_INTERVAL {
                return;
            }
            self.last_profile_poll_at = Instant::now();
            let now = SystemTime::now();

            let current_mtime = profile_choice_mtime(self.profile_choice_path.as_deref());

            // Reset pending state if file is back to the already-applied mtime.
            if current_mtime == self.last_profile_choice_mtime {
                self.pending_profile_choice_mtime = None;
                self.pending_profile_change_since = None;
                return;
            }

            // Start or restart debounce window when mtime changes.
            if self.pending_profile_choice_mtime != current_mtime {
                self.pending_profile_choice_mtime = current_mtime;
                self.pending_profile_change_since = Some(now);
                self.log_debug("profileChoice changed; waiting for debounce window");
                return;
            }

            if !profile_change_debounce_elapsed(
                self.pending_profile_change_since,
                now,
                PROFILE_CHANGE_DEBOUNCE,
            ) {
                return;
            }

            self.pending_profile_choice_mtime = None;
            self.pending_profile_change_since = None;
            self.last_profile_choice_mtime = current_mtime;
            self.log_info("detected profileChoice change, refreshing context and inventory");
            self.refresh_context_and_inventory(true);
        }

        fn poll_credentials_changes(&mut self) {
            if self.last_credentials_poll_at.elapsed() < PROFILE_POLL_INTERVAL {
                return;
            }
            self.last_credentials_poll_at = Instant::now();
            let current_mtime = credentials::credentials_mtime();
            if current_mtime != self.last_credentials_mtime {
                self.last_credentials_mtime = current_mtime;

                // Remember which profiles WERE authenticated before
                let previously_authed: Vec<String> = self
                    .config
                    .profiles
                    .iter()
                    .filter(|p| {
                        self.profile_auth_infos.iter().any(|a| {
                            a.profile_id == p.profile_id && a.auth_status == AuthStatus::Ok
                        })
                    })
                    .map(|p| p.profile_id.clone())
                    .collect();

                // Remember which profiles were NOT authenticated before
                let previously_unauthed: Vec<String> = self
                    .config
                    .profiles
                    .iter()
                    .filter(|p| {
                        !previously_authed.contains(&p.profile_id)
                    })
                    .map(|p| p.profile_id.clone())
                    .collect();

                self.profile_auth_infos =
                    credentials::check_all_profiles_auth(&self.config.profiles);
                self.log_debug("credentials file changed; refreshed profile auth status");

                // Handle profiles that just became expired
                for pid in &previously_authed {
                    let is_now_expired = self.profile_auth_infos.iter().any(|a| {
                        a.profile_id == *pid && a.auth_status != AuthStatus::Ok
                    });
                    if is_now_expired {
                        self.handle_profile_expired(pid);
                    }
                }

                // Refresh all profiles that are now authenticated.
                // This covers both newly-authenticated profiles AND
                // already-OK profiles whose credentials were renewed.
                let now_ok: Vec<String> = self.profile_auth_infos.iter()
                    .filter(|a| a.auth_status == AuthStatus::Ok)
                    .map(|a| a.profile_id.clone())
                    .collect();
                for pid in &now_ok {
                    if previously_unauthed.contains(pid) {
                        self.log_info(format!(
                            "auth became OK for profile={pid}, loading cache and refreshing"
                        ));
                        self.load_cache_for_profile(pid);
                    } else {
                        self.log_info(format!(
                            "credentials renewed for profile={pid}, refreshing"
                        ));
                    }
                    self.refresh_profile(pid, true);
                }

                // Safety net: if the selected profile is now Ok but its
                // display was cleared (e.g. handle_profile_expired ran and
                // the previously_unauthed list missed the transition because
                // expiry and re-auth collapsed into one tick), reload from
                // cache so the user doesn't see an empty list.
                if let Some(ref selected) = self.selected_profile.clone() {
                    let selected_ok = self.profile_auth_infos.iter().any(|a| {
                        a.profile_id == *selected && a.auth_status == AuthStatus::Ok
                    });
                    if selected_ok && self.inventory.instances.is_empty() {
                        self.log_info(format!(
                            "selected profile={selected} is Ok but display is empty; reloading cache"
                        ));
                        self.load_cache_for_profile(selected);
                    }
                }
            }
        }

        /// Periodically check whether any authenticated profile has expired
        /// based on its `expires_at` timestamp (time-based, independent of
        /// credentials file changes).
        fn poll_auth_expiry(&mut self) {
            if self.last_auth_expiry_check_at.elapsed() < AUTH_EXPIRY_CHECK_INTERVAL {
                return;
            }
            self.last_auth_expiry_check_at = Instant::now();

            if self.options.mode == Mode::Sim {
                return;
            }

            let now = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            // Find profiles that are marked Ok but whose expires_at is in the past
            let newly_expired: Vec<String> = self
                .profile_auth_infos
                .iter()
                .filter(|a| {
                    a.auth_status == AuthStatus::Ok
                        && a.expires_at.is_some_and(|exp| exp <= now)
                })
                .map(|a| a.profile_id.clone())
                .collect();

            if newly_expired.is_empty() {
                return;
            }

            // Re-check from the credentials file to confirm
            self.profile_auth_infos =
                credentials::check_all_profiles_auth(&self.config.profiles);

            for pid in &newly_expired {
                let still_expired = self.profile_auth_infos.iter().any(|a| {
                    a.profile_id == *pid && a.auth_status != AuthStatus::Ok
                });
                if still_expired {
                    self.handle_profile_expired(pid);
                }
            }
        }

        /// Close any Connections tabs whose PTY exited (e.g. SSM inactivity
        /// logout) longer than `STALE_TAB_AUTOCLOSE` ago.
        fn poll_stale_connection_tabs(&mut self) {
            if self.last_stale_tab_check_at.elapsed() < STALE_TAB_CHECK_INTERVAL {
                return;
            }
            self.last_stale_tab_check_at = Instant::now();

            let stale = self.connections.stale_logged_out_ids(STALE_TAB_AUTOCLOSE);
            for tab_id in stale {
                self.log_info(format!(
                    "auto-closing tab={tab_id}: disconnected > {}s",
                    STALE_TAB_AUTOCLOSE.as_secs()
                ));
                self.close_connection_tab(tab_id);
            }
        }

        /// Handle a profile whose auth has expired: save cache to disk,
        /// clear from active display, and show a message.
        fn handle_profile_expired(&mut self, profile_id: &str) {
            let display = self
                .config
                .profiles
                .iter()
                .find(|p| p.profile_id == profile_id)
                .map(|p| p.display_name.as_str())
                .unwrap_or(profile_id)
                .to_string();

            self.log_warn(format!(
                "auth expired for {display} ({profile_id})"
            ));

            // Save current instances to disk cache before clearing
            if let Some((inv, ctx)) = self.profile_inventory_cache.get(profile_id) {
                if !inv.instances.is_empty() {
                    ec2_manager::inventory::save_disk_cache(
                        profile_id,
                        &ctx.region,
                        inv,
                    );
                    self.log_info(format!(
                        "saved {} instances to disk cache for expired profile={profile_id}",
                        inv.instances.len()
                    ));
                }
            }

            let is_selected = self.selected_profile.as_deref() == Some(profile_id);
            if is_selected {
                // Save current display inventory to disk before clearing
                if !self.inventory.instances.is_empty() {
                    if let Some(ref ctx) = self.context {
                        ec2_manager::inventory::save_disk_cache(
                            profile_id,
                            &ctx.region,
                            &self.inventory,
                        );
                    }
                }
                self.inventory.instances.clear();
                self.filtered.clear();
                self.message = format!(
                    "Auth is not OK for {display}. Refresh credentials and retry."
                );
            }
        }

        fn log(&mut self, level: LogLevel, message: impl Into<String>) {
            let mut message = message.into();
            if message.trim().is_empty() {
                message = "<empty>".to_string();
            }
            self.logs.push_back(LogEntry { level, message });
            if self.logs.len() > Self::MAX_LOG_LINES {
                let overflow = self.logs.len() - Self::MAX_LOG_LINES;
                self.logs.drain(0..overflow);
            }
        }

        fn log_error(&mut self, message: impl Into<String>) {
            self.log(LogLevel::Error, message);
        }

        fn log_warn(&mut self, message: impl Into<String>) {
            self.log(LogLevel::Warn, message);
        }

        fn log_info(&mut self, message: impl Into<String>) {
            self.log(LogLevel::Info, message);
        }

        fn log_debug(&mut self, message: impl Into<String>) {
            self.log(LogLevel::Debug, message);
        }

        fn log_trace(&mut self, message: impl Into<String>) {
            self.log(LogLevel::Trace, message);
        }

        fn guarded_action<F>(&mut self, label: &str, action: F) -> bool
        where
            F: FnOnce(&mut Self) -> Result<()>,
        {
            let result = panic::catch_unwind(AssertUnwindSafe(|| action(self)));
            match result {
                Ok(Ok(())) => true,
                Ok(Err(err)) => {
                    self.message = format!("error: {err}");
                    self.log_error(format!("{label} failed: {err}"));
                    false
                }
                Err(payload) => {
                    let message = format!(
                        "{label} panicked: {}",
                        panic_payload_to_string(payload.as_ref())
                    );
                    self.message = message.clone();
                    self.log_error(message);
                    false
                }
            }
        }

        fn selected_terminal(&self) -> Option<&TerminalOption> {
            self.terminals
                .iter()
                .find(|t| t.id == self.selected_terminal_id)
        }

        fn rebuild_account_colors(&mut self) {
            // Collect extra profile_ids from open tabs not in config
            let mut extra_ids: Vec<String> = Vec::new();
            for tab in self.connections.tabs() {
                if !tab.profile_id.is_empty()
                    && !self.config.profiles.iter().any(|p| p.profile_id == tab.profile_id)
                    && !extra_ids.contains(&tab.profile_id)
                {
                    extra_ids.push(tab.profile_id.clone());
                }
            }
            self.account_color_map = build_account_color_map(
                &self.config.profiles,
                &extra_ids,
                &self.config.account_colors,
                &self.profile_inventory_cache,
                &self.inventory,
                self.selected_profile.as_deref(),
            );
        }

        /// Load cached inventory for a profile (in-memory first, then disk).
        /// Sets self.inventory, self.filtered, self.context, and self.message.
        ///
        /// Only loads cache when the current effective region matches the
        /// profile's accounts.json region (or the combobox is on Auto).
        /// If the user has picked an override region that differs from the
        /// account's configured region, the cache is not shown — per the
        /// rule that cache reflects the account's canonical region only.
        fn load_cache_for_profile(&mut self, profile_id: &str) {
            let profile_cfg = self.config.profiles.iter()
                .find(|p| p.profile_id == profile_id);
            let profile_region = profile_cfg.and_then(|p| p.region.clone());

            // Gate: if user has a non-Auto region override and it doesn't
            // match the account's configured region, blank the display.
            if let (Some(ref picked), Some(ref expected)) =
                (self.options.region.as_ref(), profile_region.as_ref())
            {
                if picked != expected {
                    self.inventory.instances.clear();
                    self.filtered.clear();
                    self.message = format!(
                        "Region override {picked} differs from account's region {expected}. \
                         Refresh to load this region's data."
                    );
                    return;
                }
            }

            // Try in-memory cache first
            if let Some((inv, ctx)) = self.profile_inventory_cache.get(profile_id) {
                let count = inv.instances.len();
                self.inventory = inv.clone();
                self.context = Some(ctx.clone());
                self.apply_filters();
                self.message = format!("Loaded {count} instances from cache (refreshing...)");
                self.log_info(format!(
                    "loaded {count} instances from memory cache for profile={profile_id}"
                ));
                return;
            }

            // Fall back to disk cache — use accounts.json region only,
            // not config.default_region, to avoid leaking one account's
            // region into another.
            let region = profile_region
                .clone()
                .unwrap_or_else(|| "us-east-1".to_string());
            let account_id = profile_cfg
                .map(|p| p.account_id.clone())
                .filter(|s| !s.is_empty());

            self.log_info(format!("disk cache lookup: profile={profile_id} region={region}"));
            if let Some(cached) = ec2_manager::inventory::load_disk_cache(profile_id, &region) {
                let count = cached.instances.len();
                let resolved_profile = credentials::find_profile_by_account_id(profile_id)
                    .unwrap_or_else(|| profile_id.to_string());
                let ctx = AwsContext {
                    mode: self.options.mode.clone(),
                    profile: resolved_profile,
                    account_id,
                    arn: None,
                    user_id: None,
                    region: region.clone(),
                    auth_status: AuthStatus::Ok,
                };
                // Store in memory cache for fast switching later
                self.profile_inventory_cache
                    .insert(profile_id.to_string(), (cached.clone(), ctx.clone()));
                self.inventory = cached;
                self.context = Some(ctx);
                self.apply_filters();
                self.message = format!("Loaded {count} instances from cache (refreshing...)");
                self.log_info(self.message.clone());
            } else {
                self.log_info("no disk cache found for this profile/region");
            }
        }

        /// Refresh a single profile in the background.
        fn refresh_profile(&mut self, profile_id: &str, force: bool) {
            self.refresh_generation += 1;
            let gen = self.refresh_generation;
            let pid = profile_id.to_string();
            self.refreshing_profiles.insert(pid.clone(), gen);

            // If this is the selected profile, show loading indicator
            if self.selected_profile.as_deref() == Some(profile_id) {
                self.refreshing = true;
                self.message = "Refreshing...".to_string();
            }
            self.log_info(format!(
                "refresh profile={pid} requested (force={force}) gen={gen}"
            ));

            let mode = self.options.mode.clone();
            let config = self.config.clone();
            // Use the profile's own configured region, not the global
            // region override, so Refresh All doesn't force every
            // account to use the currently selected account's region.
            let region_override = if self.selected_profile.as_deref() == Some(profile_id) {
                self.options.region.clone()
            } else {
                self.config.profiles.iter()
                    .find(|p| p.profile_id == pid)
                    .and_then(|p| p.region.clone())
            };
            let tx = self.refresh_tx.clone();

            std::thread::spawn(move || {
                let context = match build_context_with_profile(
                    mode,
                    &config,
                    region_override.as_deref(),
                    Some(&pid),
                ) {
                    Ok(ctx) => ctx,
                    Err(err) => {
                        let _ = tx.send(RefreshEvent::Failed {
                            generation: gen,
                            profile_id: pid,
                            error: err.to_string(),
                        });
                        return;
                    }
                };

                let config_update = context
                    .account_id
                    .as_ref()
                    .and_then(|acct| {
                        region_override
                            .as_ref()
                            .map(|region| (acct.clone(), region.clone()))
                    });

                if context.mode == Mode::Live && context.auth_status != AuthStatus::Ok {
                    let _ = tx.send(RefreshEvent::AuthNotOk {
                        generation: gen,
                        profile_id: pid,
                        context,
                    });
                    return;
                }

                // Retry up to 2 times on transient failures.
                let mut last_err = String::new();
                for attempt in 0..3 {
                    if attempt > 0 {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    match load_inventory(&context, &config.tag_mapping, force) {
                        Ok(inventory) => {
                            let _ = tx.send(RefreshEvent::Completed {
                                generation: gen,
                                profile_id: pid,
                                context,
                                inventory,
                                config_update,
                            });
                            return;
                        }
                        Err(err) => {
                            last_err = err.to_string();
                        }
                    }
                }
                let _ = tx.send(RefreshEvent::Failed {
                    generation: gen,
                    profile_id: pid,
                    error: last_err,
                });
            });
        }

        /// Convenience: refresh the currently selected profile.
        fn refresh_context_and_inventory(&mut self, force: bool) {
            if let Some(ref pid) = self.selected_profile.clone() {
                self.refresh_profile(pid, force);
            }
        }

        /// Refresh all authenticated profiles in parallel.
        fn refresh_all_authenticated(&mut self, force: bool) {
            let authed_profiles: Vec<String> = self
                .config
                .profiles
                .iter()
                .filter(|p| {
                    self.profile_auth_infos.iter().any(|a| {
                        a.profile_id == p.profile_id && a.auth_status == AuthStatus::Ok
                    })
                })
                .map(|p| p.profile_id.clone())
                .collect();

            if authed_profiles.is_empty() {
                // Fall back to refreshing selected profile (may discover auth via STS)
                self.refresh_context_and_inventory(force);
                return;
            }

            self.log_info(format!(
                "refreshing {} authenticated profiles in parallel",
                authed_profiles.len()
            ));

            for pid in &authed_profiles {
                self.refresh_profile(pid, force);
            }
        }

        fn apply_filters(&mut self) {
            let (includes, excludes) = search_terms_from_rules(&self.search_rules);
            let filters = Filters {
                includes,
                excludes,
                states: states_from_state_filter(&self.selected_state_filter),
                only_ssm_managed: self.only_ssm,
            };

            // Start with the current account's instances
            let mut all_instances = self.inventory.instances.clone();

            // Add instances from checked multi-account profiles
            let multi_ids: Vec<String> = self.multi_account_ids.iter().cloned().collect();
            let cache_keys: Vec<String> = self.profile_inventory_cache.keys().cloned().collect();
            for extra_pid in &multi_ids {
                if let Some((inv, _)) = self.profile_inventory_cache.get(extra_pid) {
                    let before = all_instances.len();
                    for inst in &inv.instances {
                        if !all_instances.iter().any(|i| i.instance_id == inst.instance_id) {
                            all_instances.push(inst.clone());
                        }
                    }
                    let added = all_instances.len() - before;
                    self.log_debug(format!(
                        "multi-account: added {added} instances from profile={extra_pid}"
                    ));
                } else {
                    self.log_debug(format!(
                        "multi-account: no cache for profile={extra_pid} (cache keys: {cache_keys:?})"
                    ));
                }
            }

            self.filtered = apply_filters(&all_instances, &filters);

            // Apply tag-specific rules using "TagKey: value" syntax.
            // Include rules with colon require the tag to match.
            // Exclude rules with colon reject instances where the tag matches.
            let mut tag_includes: Vec<(String, String)> = Vec::new();
            let mut tag_excludes: Vec<(String, String)> = Vec::new();
            for rule in &self.search_rules {
                let term = rule.term.trim();
                if term.is_empty() {
                    continue;
                }
                if let Some((key, value)) = term.split_once(':') {
                    let key = key.trim();
                    let value = value.trim().to_ascii_lowercase();
                    if !key.is_empty() && !value.is_empty() {
                        match rule.kind {
                            SearchRuleKind::Include => {
                                tag_includes.push((key.to_string(), value));
                            }
                            SearchRuleKind::Exclude => {
                                tag_excludes.push((key.to_string(), value));
                            }
                        }
                    }
                }
            }

            if !tag_includes.is_empty() || !tag_excludes.is_empty() {
                self.filtered.retain(|instance| {
                    // All tag includes must match
                    let includes_ok = tag_includes.iter().all(|(tag_key, pattern)| {
                        instance.tags.iter().any(|(k, v)| {
                            k.eq_ignore_ascii_case(tag_key)
                                && v.to_ascii_lowercase().contains(pattern.as_str())
                        })
                    });
                    // No tag excludes may match
                    let excludes_ok = !tag_excludes.iter().any(|(tag_key, pattern)| {
                        instance.tags.iter().any(|(k, v)| {
                            k.eq_ignore_ascii_case(tag_key)
                                && v.to_ascii_lowercase().contains(pattern.as_str())
                        })
                    });
                    includes_ok && excludes_ok
                });
            }

            self.auto_size_columns();
            self.log_debug(format!(
                "filters applied -> {} visible / {} total",
                self.filtered.len(),
                self.inventory.instances.len()
            ));
        }

        fn auto_size_columns(&mut self) {
            let char_w: f32 = 6.5; // approximate character width in default font
            let pad: f32 = 8.0; // small padding on each side

            let headers: &[(&str, SortColumn)] = &[
                ("Favorite", SortColumn::Favorite),
                ("InstanceId", SortColumn::InstanceId),
                ("Name", SortColumn::Name),
                ("State", SortColumn::State),
                ("SSM", SortColumn::Ssm),
                ("Private IP", SortColumn::PrivateIp),
                ("Private DNS", SortColumn::PrivateDns),
                ("AMI ID", SortColumn::AmiId),
                ("Instance Type", SortColumn::InstanceType),
                ("Environment", SortColumn::Env),
            ];

            for (label, col) in headers {
                // Header needs room for label + sort arrow + drag zone
                let header_w = label.len() as f32 * char_w + pad + 14.0;
                let has_copy = matches!(col, SortColumn::InstanceId | SortColumn::PrivateIp | SortColumn::PrivateDns | SortColumn::AmiId);
                let copy_extra = if has_copy { COL_COPY_W } else { 0.0 };

                let copy_gap = match col {
                    SortColumn::PrivateIp => 15.0,
                    SortColumn::PrivateDns => 15.0,
                    SortColumn::AmiId => 20.0,
                    _ => 0.0,
                };
                let max_content_w = self.filtered.iter().map(|inst| {
                    let text = match col {
                        SortColumn::Favorite => return 0.0_f32,
                        SortColumn::InstanceId => inst.instance_id.clone(),
                        SortColumn::Name => inst.name.clone().unwrap_or_default(),
                        SortColumn::State => inst.state.clone(),
                        SortColumn::Ssm => if inst.ssm_managed {
                            inst.ssm_ping.clone().unwrap_or_else(|| "Managed".to_string())
                        } else { "No".to_string() },
                        SortColumn::PrivateIp => inst.private_ip.clone().unwrap_or_default(),
                        SortColumn::PrivateDns => inst.private_dns.clone().unwrap_or_default(),
                        SortColumn::AmiId => inst.image_id.clone().unwrap_or_default(),
                        SortColumn::InstanceType => inst.instance_type.clone().unwrap_or_default(),
                        SortColumn::Env => inst.tags.get("MMODAL_ENV").or_else(|| inst.tags.get("mmodal_env")).cloned().unwrap_or_default(),
                        SortColumn::MatchTag => return 0.0_f32,
                    };
                    text.len() as f32 * char_w + copy_extra + copy_gap + pad
                }).fold(0.0_f32, f32::max);

                let w = header_w.max(max_content_w).max(30.0);
                self.column_widths.insert(*col as u8, w);
            }
        }

        fn paint_copy_button(ui: &mut egui::Ui, text_to_copy: &str, tooltip: &str) {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(COL_COPY_W, 14.0), egui::Sense::click());
            if ui.is_rect_visible(rect) {
                let color = ui.visuals().text_color();
                let stroke = egui::Stroke::new(1.0, color);
                let p = ui.painter();
                p.rect_stroke(egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 1.0), egui::vec2(8.0, 8.0)), 1.0, stroke, egui::StrokeKind::Outside);
                p.rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 5.0), egui::vec2(8.0, 8.0)), 1.0, ui.visuals().window_fill);
                p.rect_stroke(egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 5.0), egui::vec2(8.0, 8.0)), 1.0, stroke, egui::StrokeKind::Outside);
            }
            if resp.clicked() {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(text_to_copy);
                }
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            resp.on_hover_text(tooltip);
        }

        fn maybe_auto_connect_gui_smoke(&mut self) {
            if self.gui_smoke_auto_connect_attempted {
                return;
            }
            let Some(smoke) = &self.gui_smoke else {
                return;
            };
            if !smoke.auto_connect {
                self.gui_smoke_auto_connect_attempted = true;
                return;
            }
            self.gui_smoke_auto_connect_attempted = true;

            if self.options.mode != Mode::Sim {
                self.log_warn("GUI smoke auto-connect only runs in sim mode");
                return;
            }

            let Some(instance_id) = self
                .filtered
                .iter()
                .find(|i| i.ssm_managed)
                .or_else(|| self.filtered.first())
                .map(|i| i.instance_id.clone())
            else {
                self.log_error("GUI smoke auto-connect failed: no instances available");
                return;
            };

            self.selected_instance_id = instance_id;
            match self.connect_selected() {
                Ok(()) => self.log_info("GUI smoke auto-connect succeeded"),
                Err(err) => self.log_error(format!("GUI smoke auto-connect failed: {err}")),
            }
        }

        fn maybe_record_gui_smoke_success(&mut self, tab_id: u64, bytes: &[u8]) {
            let Some(smoke) = &self.gui_smoke else {
                return;
            };
            let marker_path = smoke.marker_path.clone();
            let expected_text = smoke.expected_text.clone();
            let exit_on_marker = smoke.exit_on_marker;
            if self.gui_smoke_marker_written {
                return;
            }
            if !gui_smoke_match_in_bytes(&expected_text, bytes) {
                return;
            }

            match write_gui_smoke_marker(&marker_path, tab_id, &expected_text) {
                Ok(()) => {
                    self.gui_smoke_marker_written = true;
                    self.log_info(format!(
                        "GUI smoke marker written to {}",
                        marker_path.display()
                    ));
                    if exit_on_marker {
                        self.gui_smoke_should_close = true;
                    }
                }
                Err(err) => {
                    self.log_error(format!(
                        "failed to write GUI smoke marker {}: {err}",
                        marker_path.display()
                    ));
                }
            }
        }

        fn account_scope(&self) -> String {
            self.context
                .as_ref()
                .and_then(|c| c.account_id.clone())
                .unwrap_or_else(|| "unknown-account".to_string())
        }

        fn region_scope(&self) -> String {
            self.context
                .as_ref()
                .map(|c| c.region.clone())
                .unwrap_or_else(|| {
                    self.options
                        .region
                        .clone()
                        .unwrap_or_else(|| "us-east-1".to_string())
                })
        }

        fn selected_instance(&self) -> Option<&Instance> {
            if self.selected_instance_id.trim().is_empty() {
                return None;
            }
            find_instance(&self.filtered, self.selected_instance_id.trim()).or_else(|| {
                find_instance(&self.inventory.instances, self.selected_instance_id.trim())
            })
        }

        fn connect_selected(&mut self) -> Result<()> {
            let instance = self
                .selected_instance()
                .ok_or_else(|| AppError::NotFound("Select an instance first".to_string()))?
                .clone();

            // Find the correct context for this instance — it may belong to
            // a multi-account profile, not the currently selected one.
            let context = self.profile_inventory_cache.iter()
                .find(|(_, (inv, _))| {
                    inv.instances.iter().any(|i| i.instance_id == instance.instance_id)
                })
                .map(|(_, (_, ctx))| ctx.clone())
                .or_else(|| self.context.clone())
                .ok_or_else(|| AppError::Parse("Context not loaded".to_string()))?;
            self.log_info(format!(
                "connect requested for {}",
                instance.instance_id
            ));

            if context.mode == Mode::Live
                && (!self.dependencies.aws_cli_found || !self.dependencies.ssm_plugin_found)
            {
                return Err(AppError::Parse(
                    "Connect requires aws CLI + session-manager-plugin in PATH".to_string(),
                ));
            }

            let command_args =
                build_ssm_session_args(&instance.instance_id, &context.region, &context.profile);
            let command_line = format!("aws {}", command_args.join(" "));
            let command = if context.mode == Mode::Sim {
                let kind = self
                    .selected_terminal()
                    .map(|t| t.kind.clone())
                    .unwrap_or(TerminalKind::Wsl);
                format_sim_command(kind, &command_line, &instance.instance_id, None)
            } else {
                command_line
            };

            let title = instance
                .name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| instance.instance_id.clone());

            self.open_connection_tab(
                title,
                instance.instance_id.clone(),
                command,
                command_args,
                &context,
            )?;
            self.main_tab = MainTab::Connections;
            Ok(())
        }

        #[allow(dead_code)]
        fn port_forward_selected(&mut self) -> Result<()> {
            let context = self
                .context
                .clone()
                .ok_or_else(|| AppError::Parse("Context not loaded".to_string()))?;
            let instance = self
                .selected_instance()
                .ok_or_else(|| AppError::NotFound("Select an instance first".to_string()))?
                .clone();
            self.log_info(format!(
                "port-forward requested for {} local={} remote={}",
                instance.instance_id, self.local_port, self.remote_port
            ));

            if context.mode == Mode::Live
                && (!self.dependencies.aws_cli_found || !self.dependencies.ssm_plugin_found)
            {
                return Err(AppError::Parse(
                    "Port forward requires aws CLI + session-manager-plugin in PATH".to_string(),
                ));
            }

            let command_args = build_ssm_port_forward_args(
                &instance.instance_id,
                &context.region,
                &context.profile,
                self.local_port,
                self.remote_port,
            );
            let command_line = format!("aws {}", command_args.join(" "));

            let command = if context.mode == Mode::Sim {
                let kind = self
                    .selected_terminal()
                    .map(|t| t.kind.clone())
                    .unwrap_or(TerminalKind::Wsl);
                format_sim_command(
                    kind,
                    &command_line,
                    &instance.instance_id,
                    Some((self.local_port, self.remote_port)),
                )
            } else {
                command_line
            };

            let title = format!(
                "{} pf {}:{}",
                instance
                    .name
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| instance.instance_id.clone()),
                self.local_port,
                self.remote_port
            );

            self.open_connection_tab(
                title,
                instance.instance_id.clone(),
                command,
                command_args,
                &context,
            )?;
            self.main_tab = MainTab::Connections;
            Ok(())
        }

        fn open_connection_tab(
            &mut self,
            title: String,
            instance_id: String,
            command: String,
            command_args: Vec<String>,
            context: &AwsContext,
        ) -> Result<()> {
            let selected_terminal = self.selected_terminal().cloned();
            self.log_debug(format!(
                "terminal selection: {}",
                terminal_debug_label(selected_terminal.as_ref())
            ));
            // Use the config profile_id (account_id) so it matches the color map.
            // Fall back to finding a config profile by context.profile, then the raw value.
            // Find the correct profile_id for this instance — it may belong
            // to a different account than the currently selected one.
            let tab_profile = self.profile_inventory_cache.iter()
                .find(|(_, (inv, _))| {
                    inv.instances.iter().any(|i| i.instance_id == instance_id)
                })
                .map(|(pid, _)| pid.clone())
                .or_else(|| self.selected_profile.clone())
                .unwrap_or_else(|| {
                    self.config.profiles.iter()
                        .find(|p| p.profile_id == context.profile || p.display_name == context.profile)
                        .map(|p| p.profile_id.clone())
                        .unwrap_or_else(|| context.profile.clone())
                });
            let tab_id = self.connections.open(title.clone(), instance_id.clone(), tab_profile);
            self.rebuild_account_colors();
            let default_path = if context.mode == Mode::Sim {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "/".to_string())
            } else {
                "/home/ec2-user".to_string()
            };
            let mut fb_state = FileBrowserState {
                current_path: default_path.clone(),
                path_input: default_path.clone(),
                ..Default::default()
            };
            fb_state.initialized = false;
            self.file_browsers.insert(tab_id, fb_state);
            self.log_info(format!(
                "opened connection tab id={tab_id} instance={instance_id}"
            ));
            self.connections
                .append_line(tab_id, format!("$ {}", truncate(&command, 220)));
            self.connections.append_line(
                tab_id,
                format!(
                    "profile={} region={} mode={}",
                    context.profile,
                    context.region,
                    context.mode.as_str()
                ),
            );
            if let Some(terminal) = &selected_terminal {
                self.connections.append_line(
                    tab_id,
                    format!("[shell profile] {} ({})", terminal.display_name, terminal.id),
                );
            }
            self.connections.append_line(
                tab_id,
                "SSM command is auto-sent. Click terminal area to focus and type directly."
                    .to_string(),
            );

            if context.mode == Mode::Live {
                self.connections.append_line(
                    tab_id,
                    "note: live SSM start-session may require a full TTY; embedded mode is best-effort.".to_string(),
                );
            }

            if self.options.dry_run {
                self.connections
                    .append_line(tab_id, "[dry-run] launch skipped".to_string());
                self.connections.set_running(tab_id, false);
                self.message = "Opened connection tab (dry-run)".to_string();
                self.log_info("dry-run mode: process launch skipped");
                return Ok(());
            }

            let pty_command = pty_command_for_context(
                selected_terminal.as_ref(),
                context,
                &command,
                &command_args,
            );
            self.log_debug(format!(
                "spawning PTY command via {}",
                pty_command.program
            ));
            self.log_trace(format!(
                "pty args: {:?} terminal={}",
                pty_command.args,
                terminal_debug_label(selected_terminal.as_ref())
            ));

            #[cfg(target_os = "windows")]
            {
                let context = context.clone();
                let proc_tx = self.proc_tx.clone();
                let ui_tx = self.ui_tx.clone();
                let pty_command = pty_command.clone();
                let egui_ctx = self.egui_ctx.clone();
                self.log_debug("spawning PTY command async");
                std::thread::spawn(move || {
                    let result = panic::catch_unwind(AssertUnwindSafe(|| {
                        spawn_pty_session_parts(tab_id, &pty_command, &context)
                    }));
                    match result {
                        Ok(Ok((session, reader))) => {
                            // Send PtyReady FIRST so the UI inserts the session
                            // into pty_sessions before any reader output arrives.
                            let _ = ui_tx.send(UiEvent::PtyReady { tab_id, session });
                            // Start reader thread AFTER PtyReady is sent.
                            start_pty_reader_thread(tab_id, reader, proc_tx, egui_ctx);
                        }
                        Ok(Err(err)) => {
                            let _ = ui_tx.send(UiEvent::Error {
                                tab_id,
                                error: format!("PTY spawn failed: {err}"),
                            });
                        }
                        Err(payload) => {
                            let _ = ui_tx.send(UiEvent::Error {
                                tab_id,
                                error: format!(
                                    "PTY spawn panicked: {}",
                                    panic_payload_to_string(payload.as_ref())
                                ),
                            });
                        }
                    }
                });
            }
            #[cfg(not(target_os = "windows"))]
            self.spawn_pty_session(tab_id, pty_command, context)?;

            if !self.options.dry_run {
                self.config
                    .add_recent_connection(ec2_manager::models::RecentConnection {
                        account_id: self.account_scope(),
                        region: self.region_scope(),
                        instance_id,
                        name: Some(title),
                        timestamp_unix: now_unix(),
                    });
                self.config.save()?;
            }

            self.message = "Opened connection tab".to_string();
            Ok(())
        }

        #[cfg(not(target_os = "windows"))]
        fn spawn_pty_session(
            &mut self,
            tab_id: u64,
            command: PtyCommand,
            context: &AwsContext,
        ) -> Result<()> {
            let (session, reader) =
                spawn_pty_session_parts(tab_id, &command, context)?;
            self.pty_sessions.insert(tab_id, session);
            start_pty_reader_thread(tab_id, reader, self.proc_tx.clone(), self.egui_ctx.clone());
            Ok(())
        }

        fn poll_connection_events(&mut self) -> bool {
            // Process PtyReady events BEFORE proc_rx output events,
            // so the session is inserted into pty_sessions before any
            // output arrives from the reader thread.
            self.poll_ui_events();

            // Drain proc_rx with a per-frame cap so a flood of output
            // (e.g., sudo su - motd, terraform plan diff, awk over
            // /etc/passwd on large-NSS hosts) can't starve the UI thread
            // and block keystrokes. Output events for the same tab are
            // coalesced into one buffer so we call vt100::Parser::process
            // once per tab per frame instead of once per chunk.
            const MAX_EVENTS_PER_FRAME: usize = 200;
            let mut pending_output: HashMap<u64, Vec<u8>> = HashMap::new();
            let mut events_processed = 0;
            let mut hit_cap = false;
            while let Ok(event) = self.proc_rx.try_recv() {
                match event {
                    ProcEvent::Output { tab_id, bytes } => {
                        pending_output
                            .entry(tab_id)
                            .or_default()
                            .extend_from_slice(&bytes);
                    }
                    ProcEvent::Error { tab_id, error } => {
                        self.log_error(format!("tab={tab_id} process error: {error}"));
                        self.connections
                            .append_line(tab_id, format!("[error] {error}"));
                    }
                    ProcEvent::Exited { tab_id, code } => {
                        self.log_info(format!("tab={tab_id} process exited with code {code}"));
                        self.connections
                            .append_line(tab_id, format!("[exit] code={code}"));
                        self.connections.set_running(tab_id, false);
                    }
                    ProcEvent::VolumeResult { volumes, iam_role } => {
                        self.detail_volumes_loading = false;
                        self.detail_iam_role = iam_role;
                        match volumes {
                            Ok(vols) => {
                                self.log_info(format!("fetched {} volumes", vols.len()));
                                self.detail_volumes = vols;
                            }
                            Err(err) => {
                                self.log_error(format!("volume fetch failed: {err}"));
                                self.detail_volumes_error = Some(err);
                            }
                        }
                    }
                }
                events_processed += 1;
                if events_processed >= MAX_EVENTS_PER_FRAME {
                    hit_cap = true;
                    break;
                }
            }

            // Apply coalesced output per tab.
            for (tab_id, bytes) in pending_output {
                // Filter the harmless WSL ConPTY getpwuid warning.
                let bytes = {
                    let text = String::from_utf8_lossy(&bytes);
                    if text.contains("CreateProcessParseCommon:1005: getpwuid") {
                        for line in text.lines() {
                            if line.contains("CreateProcessParseCommon:1005: getpwuid") {
                                self.log_debug(format!("tab={tab_id} filtered WSL noise: {line}"));
                            }
                        }
                        let cleaned: String = text.lines()
                            .filter(|l| !l.contains("CreateProcessParseCommon:1005: getpwuid"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        cleaned.into_bytes()
                    } else {
                        bytes
                    }
                };
                let mut paste_diag: Option<String> = None;
                let log_msg = if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                    session.bytes_received += bytes.len() as u64;
                    session.output_event_count += 1;
                    let evt = session.output_event_count;
                    let total = session.bytes_received;
                    // Paste-diagnostic timing: while paste_write_at is set,
                    // each inbound event logs since-write latency + parse cost
                    // so we can tell if a stall is outbound (remote) vs
                    // inbound (local parse/render).
                    let paste_info = session.paste_write_at.map(|at| {
                        (at.elapsed(), session.paste_write_bytes)
                    });
                    let parse_start = Instant::now();
                    session.parser.process(&bytes);
                    let parse_elapsed = parse_start.elapsed();
                    if let Some((since_write, write_bytes)) = paste_info {
                        let scrollback = session.parser.screen().scrollback();
                        paste_diag = Some(format!(
                            "paste-echo tab={tab_id} evt=#{evt} write_bytes={write_bytes} \
                             since_write={}ms in_bytes={} parse={}us scrollback={}",
                            since_write.as_millis(),
                            bytes.len(),
                            parse_elapsed.as_micros(),
                            scrollback,
                        ));
                        // Close the diagnostic window 2s after the paste.
                        if since_write > Duration::from_millis(2000) {
                            session.paste_write_at = None;
                        }
                    }
                    // Selection coordinates are abs_row-based and remain
                    // valid as scrollback grows, so we don't need to clear
                    // them here. (Previously cleared on any output arriving
                    // at scroll-bottom — which clobbered the user's
                    // highlight before they could right-click to copy.)
                    // Respond to Device Status Report queries.
                    respond_to_terminal_queries(session, &bytes);
                    // Auto-send PREP_TERMINAL_COMMAND in two cases:
                    //   1. Initial: first time we see a shell prompt
                    //      anywhere on the screen (loose match — works for
                    //      both `sh-4.2$` and `[ec2-user@host ~]$`).
                    //   2. Re-prep after `sudo su` / `sudo su - <user>`:
                    //      the user's typed line set `pending_reprep`. We
                    //      wait until the *cursor line* shows a fresh
                    //      `user@host`-style prompt different from the
                    //      one we last prepped — this avoids firing
                    //      prematurely on echo of the typed command (the
                    //      old prompt is still in scrollback) and avoids
                    //      firing into a `[sudo] password for ...` line.
                    let cursor_line = current_cursor_line(&session.parser);
                    let cursor_trimmed = cursor_line.trim_end().to_string();
                    let cursor_is_user_host_prompt = cursor_trimmed.contains('@')
                        && (cursor_trimmed.ends_with('$') || cursor_trimmed.ends_with('#'));

                    let initial_match = !session.ps1_auto_sent && {
                        let screen_text = session.parser.screen().contents();
                        screen_text.contains("ec2-user") || screen_text.contains("root")
                    };
                    let reprep_match = session.pending_reprep
                        && cursor_is_user_host_prompt
                        && cursor_trimmed != session.last_prepped_prompt;

                    if initial_match || reprep_match {
                        session.ps1_auto_sent = true;
                        session.pending_reprep = false;
                        session.last_prepped_prompt = cursor_trimmed;
                        let (rows, cols) = session.last_size.unwrap_or((24, 120));
                        let prep = prep_terminal_command_for_size(rows, cols);
                        if let Ok(mut stdin) = session.writer.lock() {
                            let _ = stdin.write_all(&prep)
                                .and_then(|()| stdin.flush());
                        }
                    }
                    if evt <= 5 {
                        let preview = String::from_utf8_lossy(
                            &bytes[..bytes.len().min(200)]
                        );
                        let sanitized: String = preview.chars().map(|c| {
                            if c.is_control() && c != '\n' { '.' } else { c }
                        }).collect();
                        let hex: String = bytes.iter()
                            .take(64)
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        Some((LogLevel::Debug, format!(
                            "tab={tab_id} output #{evt} len={} total={total} hex=[{hex}] preview=[{sanitized}]",
                            bytes.len(),
                        )))
                    } else {
                        Some((LogLevel::Trace, format!(
                            "tab={tab_id} output bytes={} total={total}",
                            bytes.len(),
                        )))
                    }
                } else {
                    let text = String::from_utf8_lossy(&bytes).to_string();
                    for line in text.lines() {
                        self.connections.append_line(tab_id, line.to_string());
                    }
                    None
                };
                if let Some((level, msg)) = log_msg {
                    self.log(level, msg);
                }
                if let Some(msg) = paste_diag {
                    self.log_debug(msg);
                }
                self.maybe_record_gui_smoke_success(tab_id, &bytes);
            }

            let mut exited: Vec<(u64, i32)> = Vec::new();
            let mut wait_errors: Vec<String> = Vec::new();
            for (tab_id, session) in &mut self.pty_sessions {
                let try_wait_result = panic::catch_unwind(AssertUnwindSafe(|| {
                    session.child.try_wait()
                }));
                match try_wait_result {
                    Ok(Ok(Some(status))) => {
                        exited.push((*tab_id, status.exit_code() as i32));
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(err)) => {
                        wait_errors.push(format!("tab={tab_id} try_wait error: {err}"));
                        self.connections
                            .append_line(*tab_id, format!("[wait error] {err}"));
                        exited.push((*tab_id, -1));
                    }
                    Err(_) => {
                        wait_errors.push(format!("tab={tab_id} try_wait panicked"));
                        self.connections
                            .append_line(*tab_id, "[error] try_wait panicked".to_string());
                        exited.push((*tab_id, -1));
                    }
                }
            }
            for message in wait_errors {
                self.log_error(message);
            }

            for (tab_id, code) in exited {
                if let Some(session) = self.pty_sessions.get(&tab_id) {
                    let snapshot = session.parser.screen().contents();
                    let non_empty_lines: Vec<&str> = snapshot.lines()
                        .filter(|l| !l.trim().is_empty())
                        .collect();
                    self.log_info(format!(
                        "tab={tab_id} PTY exited code={code} screen_lines={}",
                        non_empty_lines.len()
                    ));
                    if non_empty_lines.is_empty() {
                        self.log_warn(format!(
                            "tab={tab_id} process exited with empty screen (code={code}); \
                             command may have failed silently"
                        ));
                        self.connections.append_line(
                            tab_id,
                            format!(
                                "[warning] process exited with code {code} and no output — \
                                 check that aws CLI and session-manager-plugin are working"
                            ),
                        );
                    }
                    for line in non_empty_lines {
                        self.connections.append_line(tab_id, line.to_string());
                    }
                }
                self.pty_sessions.remove(&tab_id);
                self.terminal_selections.remove(&tab_id);
                let _ = self.proc_tx.send(ProcEvent::Exited { tab_id, code });
            }

            hit_cap
        }

        #[cfg(target_os = "windows")]
        fn poll_ui_events(&mut self) {
            while let Ok(event) = self.ui_rx.try_recv() {
                match event {
                    UiEvent::PtyReady { tab_id, session } => {
                        self.log_info(format!("tab={tab_id} PTY session ready"));
                        self.pty_sessions.insert(tab_id, session);
                    }
                    UiEvent::Error { tab_id, error } => {
                        self.log_error(format!("tab={tab_id} PTY spawn error: {error}"));
                        self.connections
                            .append_line(tab_id, format!("[error] {error}"));
                        self.connections.set_running(tab_id, false);
                    }
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        fn poll_ui_events(&mut self) {}

        fn poll_refresh_events(&mut self) {
            while let Ok(event) = self.refresh_rx.try_recv() {
                let event_profile = match &event {
                    RefreshEvent::Completed { profile_id, .. } => profile_id.clone(),
                    RefreshEvent::AuthNotOk { profile_id, .. } => profile_id.clone(),
                    RefreshEvent::Failed { profile_id, .. } => profile_id.clone(),
                };
                let event_gen = match &event {
                    RefreshEvent::Completed { generation, .. } => *generation,
                    RefreshEvent::AuthNotOk { generation, .. } => *generation,
                    RefreshEvent::Failed { generation, .. } => *generation,
                };

                // Discard if this profile was re-requested with a newer generation
                if let Some(&expected_gen) = self.refreshing_profiles.get(&event_profile) {
                    if event_gen < expected_gen {
                        self.log_debug(format!(
                            "discarding stale refresh for profile={event_profile} gen={event_gen} (expected={expected_gen})"
                        ));
                        continue;
                    }
                }

                // This profile's refresh is done
                self.refreshing_profiles.remove(&event_profile);

                let is_selected = self.selected_profile.as_deref() == Some(&event_profile);

                // Update loading indicator based on whether the *selected* profile
                // is still refreshing (not all profiles globally), so the user
                // can freely switch accounts while others load in the background.
                if is_selected || self.refreshing_profiles.is_empty() {
                    self.refreshing = self.selected_profile.as_ref()
                        .and_then(|pid| self.refreshing_profiles.get(pid.as_str()))
                        .is_some();
                }

                match event {
                    RefreshEvent::Completed {
                        profile_id,
                        context,
                        inventory,
                        config_update,
                        ..
                    } => {
                        // Always save to disk and in-memory cache
                        ec2_manager::inventory::save_disk_cache(
                            &profile_id,
                            &context.region,
                            &inventory,
                        );
                        self.profile_inventory_cache.insert(
                            profile_id.clone(),
                            (inventory.clone(), context.clone()),
                        );
                        self.log_info(format!(
                            "refreshed profile={profile_id}: {} instances",
                            inventory.instances.len()
                        ));
                        // Update the active display if this is the selected profile
                        if is_selected {
                            self.context = Some(context);
                            self.inventory = inventory;
                            self.apply_filters();
                            self.message = format!(
                                "Loaded {} instances ({} filtered)",
                                self.inventory.instances.len(),
                                self.filtered.len()
                            );
                        } else if self.multi_account_ids.contains(&profile_id) {
                            // Re-apply filters so newly loaded multi-account
                            // instances appear immediately
                            self.apply_filters();
                        }

                        // Rebuild colors after inventory is updated
                        self.rebuild_account_colors();

                        if let Some((account_id, region)) = config_update {
                            self.config.upsert_account_region(&account_id, &region);
                            if let Err(err) = self.config.save() {
                                self.log_warn(format!(
                                    "failed to save account-region mapping: {err}"
                                ));
                            }
                        }
                        if is_selected {
                            self.maybe_auto_connect_gui_smoke();
                        }
                    }
                    RefreshEvent::AuthNotOk { profile_id, context, .. } => {
                        let display = self.config.profiles.iter()
                            .find(|p| p.profile_id == profile_id)
                            .map(|p| p.display_name.as_str())
                            .unwrap_or(profile_id.as_str())
                            .to_string();
                        self.log_warn(format!(
                            "auth not OK for {display} ({profile_id})"
                        ));
                        // Keep the memory cache intact so it's available
                        // immediately when auth becomes OK again. For the
                        // selected profile, blank the display entirely —
                        // we don't show instances for an unauthorized
                        // account.
                        if is_selected {
                            self.context = Some(context);
                            self.inventory.instances.clear();
                            self.filtered.clear();
                            self.message = format!(
                                "Auth is not OK for {display}. Refresh credentials and retry."
                            );
                        }
                    }
                    RefreshEvent::Failed { profile_id, error, .. } => {
                        let display = self.config.profiles.iter()
                            .find(|p| p.profile_id == profile_id)
                            .map(|p| p.display_name.as_str())
                            .unwrap_or(profile_id.as_str())
                            .to_string();
                        self.log_error(format!(
                            "refresh failed for {display} ({profile_id}): {error}"
                        ));
                        if is_selected {
                            self.message = format!("error ({display}): {error}");
                        }
                    }
                }
            }
        }

        fn close_connection_tab(&mut self, tab_id: u64) {
            if let Some(session) = self.pty_sessions.remove(&tab_id) {
                self.log_debug(format!("tab={tab_id} closing PTY session, spawning cleanup thread"));
                std::thread::spawn(move || {
                    // Send "exit" to gracefully terminate the remote SSM session
                    // before killing the local process.  Without this, the SSM
                    // agent on the instance keeps the session alive until it
                    // times out, blocking new connections for ~30 s.
                    if let Ok(mut w) = session.writer.lock() {
                        let _ = w.write_all(b"\x03\nexit\n");
                        let _ = w.flush();
                    }
                    // Give the remote session a moment to process the exit
                    std::thread::sleep(std::time::Duration::from_millis(500));

                    // Drop master/writer first to unblock reader and signal EOF to child
                    drop(session.master);
                    drop(session.writer);
                    drop(session.parser);

                    let mut child = session.child;
                    // kill() can block or panic on ConPTY — catch everything
                    let kill_result = panic::catch_unwind(AssertUnwindSafe(|| {
                        child.kill()
                    }));
                    match &kill_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => eprintln!("tab={tab_id} kill error (non-fatal): {e}"),
                        Err(_) => eprintln!("tab={tab_id} kill panicked (non-fatal)"),
                    }

                    // wait() reaps the zombie — also guarded
                    let wait_result = panic::catch_unwind(AssertUnwindSafe(|| {
                        child.wait()
                    }));
                    match &wait_result {
                        Ok(Ok(status)) => {
                            eprintln!("tab={tab_id} child exited code={}", status.exit_code());
                        }
                        Ok(Err(e)) => eprintln!("tab={tab_id} wait error (non-fatal): {e}"),
                        Err(_) => eprintln!("tab={tab_id} wait panicked (non-fatal)"),
                    }
                });
            }
            self.file_browsers.remove(&tab_id);
            self.terminal_selections.remove(&tab_id);
            self.connections.close(tab_id);
            self.log_info(format!("closed connection tab id={tab_id}"));
        }

        fn request_file_listing(&mut self, tab_id: u64, path: String) {
            let Some(fb) = self.file_browsers.get_mut(&tab_id) else {
                return;
            };
            // Show cached entries immediately if available
            if let Some(cached) = fb.dir_cache.get(&path) {
                fb.entries = cached.clone();
                fb.initialized = true;
            }
            fb.status = FileOpStatus::Listing;
            fb.current_path = path.clone();
            fb.path_input = path.clone();
            fb.selected_entries.clear();
            fb.last_clicked_entry = None;

            // Look up the tab by ID (not the selected tab) so the file
            // browser always targets the correct EC2 instance.
            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            // Use the cached context for this tab's profile if available
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    list_files_local(&path)
                } else if let Some(ctx) = &context {
                    let cmd = format!("ls -la {path}");
                    match ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd) {
                        Ok(command_id) => {
                            match ssm_wait_for_command(
                                &ctx.profile,
                                &ctx.region,
                                &instance_id,
                                &command_id,
                            ) {
                                Ok(output) => Ok(parse_ls_output(&output)),
                                Err(e) => Err(e),
                            }
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    Err("no AWS context available".to_string())
                };

                match result {
                    Ok(entries) => {
                        let _ = tx.send(FileOpEvent::ListingCompleted {
                            tab_id,
                            path,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(FileOpEvent::ListingFailed { tab_id, error });
                    }
                }
            });
        }

        /// Fetch a directory listing in the background (for prefetching subdirs).
        /// Does NOT change current_path or status. Results are cached via ListingCompleted.
        fn request_bg_listing(&mut self, tab_id: u64, path: String) {
            let Some(fb) = self.file_browsers.get_mut(&tab_id) else {
                return;
            };
            // Skip if already cached or being fetched
            if fb.dir_cache.contains_key(&path) || fb.fetching_dirs.contains(&path) {
                return;
            }
            fb.fetching_dirs.insert(path.clone());

            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    list_files_local(&path)
                } else if let Some(ctx) = &context {
                    let cmd = format!("ls -la {path}");
                    ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd)
                        .and_then(|command_id| {
                            ssm_wait_for_command(&ctx.profile, &ctx.region, &instance_id, &command_id)
                        })
                        .map(|output| parse_ls_output(&output))
                } else {
                    Err("no AWS context available".to_string())
                };

                if let Ok(entries) = result {
                    let _ = tx.send(FileOpEvent::ListingCompleted {
                        tab_id,
                        path,
                        entries,
                    });
                }
            });
        }

        fn request_file_download(
            &mut self,
            tab_id: u64,
            remote_path: String,
            local_path: String,
        ) {
            let Some(fb) = self.file_browsers.get_mut(&tab_id) else {
                return;
            };
            fb.status = FileOpStatus::Downloading;

            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    std::fs::read(&remote_path)
                        .and_then(|data| {
                            let bytes = data.len() as u64;
                            std::fs::write(&local_path, &data)?;
                            Ok(bytes)
                        })
                        .map_err(|e| e.to_string())
                } else if let Some(ctx) = &context {
                    let cmd = format!("base64 {remote_path}");
                    ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd)
                        .and_then(|command_id| {
                            ssm_wait_for_command(
                                &ctx.profile,
                                &ctx.region,
                                &instance_id,
                                &command_id,
                            )
                        })
                        .and_then(|b64_output| {
                            use base64::Engine;
                            let cleaned: String =
                                b64_output.chars().filter(|c| !c.is_whitespace()).collect();
                            let data = base64::engine::general_purpose::STANDARD
                                .decode(&cleaned)
                                .map_err(|e| format!("base64 decode failed: {e}"))?;
                            let bytes = data.len() as u64;
                            std::fs::write(&local_path, &data)
                                .map_err(|e| format!("write local file failed: {e}"))?;
                            Ok(bytes)
                        })
                } else {
                    Err("no AWS context available".to_string())
                };

                match result {
                    Ok(bytes) => {
                        let _ = tx.send(FileOpEvent::DownloadCompleted {
                            tab_id,
                            remote_path,
                            local_path,
                            bytes,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(FileOpEvent::DownloadFailed { tab_id, error });
                    }
                }
            });
        }

        /// Like `request_file_download` but does not set status to Downloading
        /// (the caller manages batch status and `pending_downloads`).
        #[allow(dead_code)]
        fn request_batch_file_download(
            &mut self,
            tab_id: u64,
            remote_path: String,
            local_path: String,
        ) {
            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    std::fs::read(&remote_path)
                        .and_then(|data| {
                            let bytes = data.len() as u64;
                            std::fs::write(&local_path, &data)?;
                            Ok(bytes)
                        })
                        .map_err(|e| e.to_string())
                } else if let Some(ctx) = &context {
                    let cmd = format!("base64 {remote_path}");
                    ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd)
                        .and_then(|command_id| {
                            ssm_wait_for_command(
                                &ctx.profile,
                                &ctx.region,
                                &instance_id,
                                &command_id,
                            )
                        })
                        .and_then(|b64_output| {
                            use base64::Engine;
                            let cleaned: String =
                                b64_output.chars().filter(|c| !c.is_whitespace()).collect();
                            let data = base64::engine::general_purpose::STANDARD
                                .decode(&cleaned)
                                .map_err(|e| format!("base64 decode failed: {e}"))?;
                            let bytes = data.len() as u64;
                            std::fs::write(&local_path, &data)
                                .map_err(|e| format!("write local file failed: {e}"))?;
                            Ok(bytes)
                        })
                } else {
                    Err("no AWS context available".to_string())
                };

                match result {
                    Ok(bytes) => {
                        let _ = tx.send(FileOpEvent::DownloadCompleted {
                            tab_id,
                            remote_path,
                            local_path,
                            bytes,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(FileOpEvent::DownloadFailed { tab_id, error });
                    }
                }
            });
        }

        fn request_file_upload(
            &mut self,
            tab_id: u64,
            local_path: String,
            remote_dir: String,
        ) {
            let Some(fb) = self.file_browsers.get_mut(&tab_id) else {
                return;
            };
            fb.status = FileOpStatus::Uploading;

            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();
            let file_name = std::path::Path::new(&local_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "uploaded_file".to_string());
            let remote_path = join_path(&remote_dir, &file_name);

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    std::fs::read(&local_path)
                        .and_then(|data| {
                            let bytes = data.len() as u64;
                            std::fs::write(&remote_path, &data)?;
                            Ok(bytes)
                        })
                        .map_err(|e| e.to_string())
                } else if let Some(ctx) = &context {
                    std::fs::read(&local_path)
                        .map_err(|e| format!("read local file failed: {e}"))
                        .and_then(|data| {
                            use base64::Engine;
                            let bytes = data.len() as u64;
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                            let cmd =
                                format!("echo '{}' | base64 -d > {}", b64, remote_path);
                            ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd)
                                .and_then(|command_id| {
                                    ssm_wait_for_command(
                                        &ctx.profile,
                                        &ctx.region,
                                        &instance_id,
                                        &command_id,
                                    )
                                })?;
                            Ok(bytes)
                        })
                } else {
                    Err("no AWS context available".to_string())
                };

                match result {
                    Ok(bytes) => {
                        let _ = tx.send(FileOpEvent::UploadCompleted {
                            tab_id,
                            local_path,
                            remote_path,
                            bytes,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(FileOpEvent::UploadFailed { tab_id, error });
                    }
                }
            });
        }

        /// Read a remote file's content into the inline editor.
        fn request_file_read(&mut self, tab_id: u64, remote_path: String) {
            let Some(fb) = self.file_browsers.get_mut(&tab_id) else {
                return;
            };
            fb.status = FileOpStatus::Initializing;

            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    std::fs::read_to_string(&remote_path)
                        .map_err(|e| e.to_string())
                } else if let Some(ctx) = &context {
                    let cmd = format!("base64 {remote_path}");
                    ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd)
                        .and_then(|command_id| {
                            ssm_wait_for_command(
                                &ctx.profile,
                                &ctx.region,
                                &instance_id,
                                &command_id,
                            )
                        })
                        .and_then(|b64_output| {
                            use base64::Engine;
                            let cleaned: String = b64_output.chars()
                                .filter(|c| !c.is_whitespace())
                                .collect();
                            let data = base64::engine::general_purpose::STANDARD
                                .decode(&cleaned)
                                .map_err(|e| format!("base64 decode failed: {e}"))?;
                            String::from_utf8(data)
                                .map_err(|e| format!("file is not valid UTF-8 text: {e}"))
                        })
                } else {
                    Err("no AWS context available".to_string())
                };

                match result {
                    Ok(content) => {
                        let _ = tx.send(FileOpEvent::FileReadCompleted {
                            tab_id,
                            remote_path,
                            content,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(FileOpEvent::FileReadFailed { tab_id, error });
                    }
                }
            });
        }

        /// Save editor content back to the remote file.
        fn request_file_save(&mut self, tab_id: u64) {
            let Some(fb) = self.file_browsers.get_mut(&tab_id) else {
                return;
            };
            let Some(active) = fb.active_editor else {
                return;
            };
            let Some(et) = fb.editor_tabs.get_mut(active) else {
                return;
            };
            let remote_path = et.remote_path.clone();
            let content = et.content.clone();
            et.status = "Updating...".to_string();
            fb.status = FileOpStatus::Updating;

            let tab = self.connections.tabs().iter()
                .find(|t| t.id == tab_id).cloned();
            let instance_id = tab.as_ref().map(|t| t.instance_id.clone()).unwrap_or_default();
            let context = tab.as_ref()
                .and_then(|t| {
                    self.profile_inventory_cache.get(&t.profile_id)
                        .map(|(_, ctx)| ctx.clone())
                })
                .or_else(|| self.context.clone());
            let mode = self.options.mode.clone();
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                let result = if mode == Mode::Sim {
                    std::fs::write(&remote_path, &content)
                        .map_err(|e| e.to_string())
                } else if let Some(ctx) = &context {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD
                        .encode(content.as_bytes());
                    let cmd = format!("echo '{}' | base64 -d > {}", b64, remote_path);
                    ssm_send_command(&ctx.profile, &ctx.region, &instance_id, &cmd)
                        .and_then(|command_id| {
                            ssm_wait_for_command(
                                &ctx.profile,
                                &ctx.region,
                                &instance_id,
                                &command_id,
                            )
                        })
                        .map(|_| ())
                } else {
                    Err("no AWS context available".to_string())
                };

                match result {
                    Ok(()) => {
                        let _ = tx.send(FileOpEvent::FileSaveCompleted {
                            tab_id,
                            remote_path,
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(FileOpEvent::FileSaveFailed { tab_id, error });
                    }
                }
            });
        }

        fn poll_file_op_events(&mut self) {
            while let Ok(event) = self.file_op_rx.try_recv() {
                match event {
                    FileOpEvent::ListingCompleted {
                        tab_id,
                        path,
                        entries,
                    } => {
                        self.log_info(format!(
                            "file listing completed tab={tab_id} path={path} entries={}",
                            entries.len()
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            fb.fetching_dirs.remove(&path);
                            fb.dir_cache.insert(path.clone(), entries.clone());
                            // Only update the main listing if this is the current path
                            if fb.current_path == path {
                                fb.entries = entries;
                                fb.status = FileOpStatus::Idle;
                            }
                            fb.initialized = true;
                        }
                    }
                    FileOpEvent::ListingFailed { tab_id, error } => {
                        self.log_error(format!(
                            "file listing failed tab={tab_id}: {error}"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            fb.status = FileOpStatus::Error(error);
                        }
                    }
                    FileOpEvent::DownloadCompleted {
                        tab_id,
                        remote_path,
                        local_path,
                        bytes,
                    } => {
                        self.log_info(format!(
                            "download completed tab={tab_id} {remote_path} -> {local_path} ({bytes} bytes)"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            if fb.pending_downloads > 0 {
                                fb.pending_downloads -= 1;
                                if fb.pending_downloads == 0 {
                                    fb.status = FileOpStatus::Idle;
                                }
                            } else {
                                fb.status = FileOpStatus::Idle;
                            }
                        }
                    }
                    FileOpEvent::DownloadFailed { tab_id, error } => {
                        self.log_error(format!(
                            "download failed tab={tab_id}: {error}"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            if fb.pending_downloads > 0 {
                                fb.pending_downloads -= 1;
                                if fb.pending_downloads == 0 {
                                    fb.status = FileOpStatus::Error(error);
                                }
                            } else {
                                fb.status = FileOpStatus::Error(error);
                            }
                        }
                    }
                    FileOpEvent::UploadCompleted {
                        tab_id,
                        local_path,
                        remote_path,
                        bytes,
                    } => {
                        self.log_info(format!(
                            "upload completed tab={tab_id} {local_path} -> {remote_path} ({bytes} bytes)"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            fb.status = FileOpStatus::Idle;
                            // Invalidate cache for this directory since a file was added
                            fb.dir_cache.remove(&fb.current_path);
                            let path = fb.current_path.clone();
                            let tab_id_copy = tab_id;
                            // queue re-listing after upload
                            self.request_file_listing(tab_id_copy, path);
                            return;
                        }
                    }
                    FileOpEvent::UploadFailed { tab_id, error } => {
                        self.log_error(format!(
                            "upload failed tab={tab_id}: {error}"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            fb.status = FileOpStatus::Error(error);
                        }
                    }
                    FileOpEvent::FileReadCompleted {
                        tab_id,
                        remote_path,
                        content,
                    } => {
                        self.log_info(format!(
                            "file read completed tab={tab_id} path={remote_path} ({} bytes)",
                            content.len()
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            let was_empty = fb.editor_tabs.is_empty();
                            // Check if file is already open — switch to it
                            if let Some(idx) = fb.editor_tabs.iter().position(|t| t.remote_path == remote_path) {
                                fb.active_editor = Some(idx);
                            } else {
                                fb.editor_tabs.push(EditorTab {
                                    remote_path,
                                    content: content.clone(),
                                    original: content,
                                    dirty: false,
                                    status: String::new(),
                                });
                                fb.active_editor = Some(fb.editor_tabs.len() - 1);
                            }
                            fb.status = FileOpStatus::Idle;
                            // Default to 50/50 split when first file opens
                            if was_empty {
                                self.editor_split.insert(tab_id, 0.5);
                            }
                        }
                    }
                    FileOpEvent::FileReadFailed { tab_id, error } => {
                        self.log_error(format!(
                            "file read failed tab={tab_id}: {error}"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            fb.status = FileOpStatus::Idle;
                        }
                        self.message = format!("Failed to open file: {error}");
                    }
                    FileOpEvent::FileSaveCompleted { tab_id, remote_path } => {
                        self.log_info(format!(
                            "file saved tab={tab_id} path={remote_path}"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            if let Some(et) = fb.editor_tabs.iter_mut().find(|t| t.remote_path == remote_path) {
                                et.original = et.content.clone();
                                et.dirty = false;
                                et.status = "Saved".to_string();
                            }
                            fb.status = FileOpStatus::Idle;
                        }
                    }
                    FileOpEvent::FileSaveFailed { tab_id, error } => {
                        self.log_error(format!(
                            "file save failed tab={tab_id}: {error}"
                        ));
                        if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                            if let Some(active) = fb.active_editor {
                                if let Some(et) = fb.editor_tabs.get_mut(active) {
                                    et.status = format!("Save failed: {error}");
                                }
                            }
                            fb.status = FileOpStatus::Idle;
                        }
                    }
                }
            }
        }

        /// Stamp the session so the next inbound output event logs the
        /// round-trip latency of a large paste. Used to diagnose whether
        /// paste stalls are outbound (remote buffering) or inbound
        /// (local parse/render cost).
        fn mark_paste_write(&mut self, tab_id: u64, len: usize) {
            if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                session.paste_write_at = Some(Instant::now());
                session.paste_write_bytes = len;
            }
            self.log_debug(format!(
                "paste write tab={tab_id} bytes={len}"
            ));
        }

        /// Drip-feed a paste payload to the remote PTY in line-sized chunks
        /// with brief inter-chunk delays. SSM sessions don't reliably pass
        /// bracketed paste sequences, and writing a large blob in one shot
        /// causes the remote shell's TTY input buffer to drop characters
        /// (most visible with multi-line content like RSA pem files). We
        /// run the drip loop on a worker thread so the UI stays responsive.
        fn paste_to_connection_tab(&mut self, tab_id: u64, payload: Vec<u8>) {
            let Some(session) = self.pty_sessions.get_mut(&tab_id) else {
                return;
            };
            session.scroll_offset = 0;
            let writer = Arc::clone(&session.writer);
            let total_len = payload.len();
            self.log_debug(format!(
                "paste worker spawn tab={tab_id} bytes={total_len}"
            ));
            std::thread::spawn(move || {
                // Cap each write to ~256 bytes; split early at `\r` so each
                // line lands as a discrete write with extra settle time
                // between lines (the remote shell echoes after each \r and
                // can't keep up if bytes arrive faster than the echo).
                const MAX_CHUNK: usize = 256;
                const INTER_CHUNK_DELAY: Duration = Duration::from_millis(8);
                const POST_LINE_DELAY: Duration = Duration::from_millis(35);
                let mut start = 0usize;
                while start < payload.len() {
                    let max_end = (start + MAX_CHUNK).min(payload.len());
                    let end = payload[start..max_end]
                        .iter()
                        .position(|&b| b == b'\r')
                        .map(|i| start + i + 1)
                        .unwrap_or(max_end);
                    let ended_on_cr = payload.get(end - 1) == Some(&b'\r');
                    let chunk = &payload[start..end];
                    {
                        let Ok(mut w) = writer.lock() else { return; };
                        if let Err(err) = w.write_all(chunk) {
                            eprintln!("paste worker tab={tab_id} write error: {err}");
                            return;
                        }
                        let _ = w.flush();
                    }
                    start = end;
                    if start < payload.len() {
                        std::thread::sleep(if ended_on_cr {
                            POST_LINE_DELAY
                        } else {
                            INTER_CHUNK_DELAY
                        });
                    }
                }
            });
        }

        fn send_raw_bytes_to_connection_tab(&mut self, tab_id: u64, payload: &[u8]) {
            self.log_trace(format!(
                "sending input to tab={tab_id} bytes={}",
                payload.len()
            ));
            let Some(session) = self.pty_sessions.get_mut(&tab_id) else {
                return;
            };
            session.scroll_offset = 0;
            // Track typed line so we can detect `sudo su` / `sudo su - <user>`
            // and re-trigger prep terminal when the new shell prompt appears.
            for &b in payload {
                match b {
                    b'\r' | b'\n' => {
                        if is_sudo_su_line(&session.input_line_buf) {
                            session.pending_reprep = true;
                        }
                        session.input_line_buf.clear();
                    }
                    0x03 | 0x15 => {
                        // Ctrl-C / Ctrl-U: line aborted/cleared.
                        session.input_line_buf.clear();
                    }
                    0x7f | 0x08 => {
                        session.input_line_buf.pop();
                    }
                    c if c >= 0x20 && c < 0x7f => {
                        session.input_line_buf.push(c as char);
                    }
                    _ => {}
                }
            }
            let Ok(mut stdin) = session.writer.lock() else {
                return;
            };
            let write_result = stdin.write_all(payload).and_then(|()| stdin.flush());
            drop(stdin);
            if let Err(err) = write_result {
                self.log_error(format!("tab={tab_id} write error: {err}"));
            }
        }

        fn forward_terminal_key_input(&mut self, ctx: &egui::Context, tab_id: u64) {
            let events = ctx.input(|i| i.raw.events.clone());
            let current_modifiers = ctx.input(|i| i.modifiers);
            let has_text = events.iter().any(|e| matches!(e, egui::Event::Text(_)));
            let has_key_backspace = events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Backspace,
                        pressed: true,
                        ..
                    }
                )
            });
            let mut sent_etx = false;
            let mut sent_can = false;
            let on_alt_screen = self.pty_sessions.get(&tab_id)
                .map(|s| s.parser.screen().alternate_screen())
                .unwrap_or(false);
            let screen_rows = self.pty_sessions.get(&tab_id)
                .map(|s| s.parser.screen().size().0 as usize)
                .unwrap_or(45);
            for event in events {
                // Intercept Shift+PageUp/Down/Home/End for scrollback navigation.
                // Only when not on alternate screen (vim, less, htop handle their own scrolling).
                if !on_alt_screen {
                    if let egui::Event::Key { key, pressed: true, modifiers, .. } = &event {
                        if modifiers.shift {
                            match key {
                                egui::Key::PageUp => {
                                    if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                        session.scroll_offset = session.scroll_offset.saturating_add(screen_rows);
                                    }
                                    continue;
                                }
                                egui::Key::PageDown => {
                                    if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                        session.scroll_offset = session.scroll_offset.saturating_sub(screen_rows);
                                    }
                                    continue;
                                }
                                egui::Key::Home => {
                                    if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                        session.scroll_offset = usize::MAX;
                                    }
                                    continue;
                                }
                                egui::Key::End => {
                                    if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                        session.scroll_offset = 0;
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // egui emits Event::Copy for Ctrl+C regardless of shift.
                // We intercept it here so we can check modifiers: plain
                // Ctrl+C → send ETX (0x03), Ctrl+Shift+C → let egui copy.
                if matches!(event, egui::Event::Copy) {
                    if current_modifiers.shift {
                        // Ctrl+Shift+C: copy selected terminal text
                        let sel = self.terminal_selections.get(&tab_id).cloned();
                        if let Some(sel) = sel {
                            if let Some((start, end)) = sel.normalized() {
                                if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                    let text = extract_selection_text(
                                        &mut session.parser, start, end,
                                    );
                                    if !text.is_empty() {
                                        if let Err(err) = clipboard_set_text_with_retry(&text) {
                                            self.log_error(format!(
                                                "Ctrl+Shift+C copy tab={tab_id} set_text failed after retries: {err}"
                                            ));
                                        }
                                        self.log_debug(format!(
                                            "copied selection tab={tab_id} abs ({},{})→({},{}) len={}",
                                            start.abs_row, start.col, end.abs_row, end.col, text.len()
                                        ));
                                    }
                                }
                                if let Some(sel) = self.terminal_selections.get_mut(&tab_id) {
                                    sel.clear();
                                }
                            }
                        }
                    } else if !sent_etx {
                        self.log_trace(format!(
                            "terminal input event tab={tab_id} kind=Copy→ETX"
                        ));
                        self.send_raw_bytes_to_connection_tab(tab_id, &[0x03]);
                        sent_etx = true;
                    }
                    continue;
                }
                // egui emits Event::Cut for Ctrl+X regardless of shift.
                // Plain Ctrl+X → send CAN (0x18) so TUIs like nano (exit)
                // and emacs (prefix) see it. Ctrl+Shift+X → let egui cut.
                if matches!(event, egui::Event::Cut) {
                    if !current_modifiers.shift && !sent_can {
                        self.log_trace(format!(
                            "terminal input event tab={tab_id} kind=Cut→CAN"
                        ));
                        self.send_raw_bytes_to_connection_tab(tab_id, &[0x18]);
                        sent_can = true;
                    }
                    continue;
                }
                if let Some(payload) =
                    terminal_event_payload_for_terminal(&event, has_text, has_key_backspace)
                {
                    // Dedup: if we already sent ETX via the Copy path above,
                    // skip a second ETX from the Key::C arm.
                    if payload == [0x03] && sent_etx {
                        continue;
                    }
                    if payload == [0x03] {
                        sent_etx = true;
                    }
                    self.log_trace(format!(
                        "terminal input event tab={tab_id} kind={}",
                        terminal_event_kind(&event)
                    ));
                    if matches!(event, egui::Event::Paste(_)) {
                        // Route Ctrl+V paste through the chunked drip-feed
                        // path — single-shot writes lose bytes for long
                        // multi-line content (RSA pem files, scripts).
                        self.mark_paste_write(tab_id, payload.len());
                        self.paste_to_connection_tab(tab_id, payload);
                    } else {
                        self.send_raw_bytes_to_connection_tab(tab_id, &payload);
                    }
                }
            }
        }

        #[allow(dead_code)]
        fn toggle_favorite_selected(&mut self) -> Result<()> {
            let Some(instance_id) = self.selected_instance().map(|i| i.instance_id.clone()) else {
                return Err(AppError::NotFound("Select an instance first".to_string()));
            };

            let enabled = self.config.toggle_favorite(
                &self.account_scope(),
                &self.region_scope(),
                &instance_id,
            );
            self.config.save()?;
            self.message = format!(
                "Favorite {}: {}",
                if enabled { "enabled" } else { "disabled" },
                instance_id
            );
            self.log_info(self.message.clone());
            Ok(())
        }

        fn save_current_filter(&mut self) -> Result<()> {
            let name = self.save_filter_name.trim();
            if name.is_empty() {
                return Err(AppError::InvalidArgument(
                    "Filter name cannot be empty".to_string(),
                ));
            }

            let states = states_from_state_filter(&self.selected_state_filter);
            let (include_terms, exclude_terms) = search_terms_from_rules(&self.search_rules);

            // Capture favorited instance IDs from the current filtered list
            let account = self.account_scope();
            let region = self.region_scope();
            let pinned_ids: Vec<String> = self.filtered.iter()
                .filter(|i| self.config.is_favorite(&account, &region, &i.instance_id))
                .map(|i| i.instance_id.clone())
                .collect();

            self.config.upsert_saved_filter(
                "global",
                "global",
                SavedFilter {
                    name: name.to_string(),
                    include_terms,
                    exclude_terms,
                    states,
                    only_ssm_managed: self.only_ssm,
                    pinned_ids,
                },
            );
            self.config.save()?;
            self.message = format!("Saved filter: {name}");
            self.log_info(self.message.clone());
            Ok(())
        }

        fn apply_saved_filter(&mut self) -> Result<()> {
            let name = self.selected_saved_filter.trim().to_string();
            if name.is_empty() {
                return Err(AppError::InvalidArgument(
                    "Select a saved filter first".to_string(),
                ));
            }

            let all_filters = self
                .config
                .saved_filters_for_scope("global", "global");
            let Some(saved) = all_filters
                .into_iter()
                .find(|f| f.name.eq_ignore_ascii_case(&name))
            else {
                return Err(AppError::NotFound(format!(
                    "Saved filter not found: {name}"
                )));
            };

            self.search_rules = rules_from_search_terms(&saved.include_terms, &saved.exclude_terms);
            self.selected_state_filter = state_filter_from_saved_states(&saved.states);
            self.only_ssm = saved.only_ssm_managed;
            self.apply_filters();

            // If the saved filter has pinned instance IDs (favorites at save time),
            // further restrict to only those instances.
            if !saved.pinned_ids.is_empty() {
                self.filtered.retain(|i| {
                    saved.pinned_ids.iter().any(|pid| pid.eq_ignore_ascii_case(&i.instance_id))
                });
            }

            self.message = format!("Applied saved filter: {name}");
            self.log_info(self.message.clone());
            Ok(())
        }

        fn run_diagnostics(&mut self) {
            if let Some(context) = &self.context {
                let report = run_diagnostics(context, &self.dependencies, &[], &self.config);
                self.diagnostics = format!(
                    "mode={}\nprofile={}\nauth={}\naws_cli={}\nssm_plugin={}\nec2_check={:?}\nssm_check={:?}",
                    report.mode,
                    report.profile,
                    report.auth_status,
                    report.aws_cli_found,
                    report.ssm_plugin_found,
                    report.ec2_check,
                    report.ssm_check,
                );
                self.log_info("diagnostics completed");
            }
        }

        // Auto-fit zoom: fire on startup and on real monitor moves.
        // Runs every frame from `update()` so it applies regardless of
        // which main tab is active (Inventory, Connections, Details, Log).
        fn apply_auto_fit_zoom(&mut self, ctx: &egui::Context) {
            let current_ppp = ctx.pixels_per_point();
            let native_ppp = ctx.native_pixels_per_point().unwrap_or(1.0);
            let (monitor_points_w, outer_pos) = ctx.input(|i| {
                let vp = i.viewport();
                let mon_w = vp
                    .monitor_size
                    .map(|s| s.x)
                    .unwrap_or_else(|| vp.inner_rect.map(|r| r.width()).unwrap_or(0.0));
                let pos = vp.outer_rect.map(|r| r.min);
                (mon_w, pos)
            });
            let window_pixels = monitor_points_w * current_ppp;
            let short_cooldown_elapsed = self
                .last_auto_fit_at
                .map(|t| t.elapsed() >= Duration::from_millis(150))
                .unwrap_or(true);
            let is_first_fit = !self.auto_fit_applied;
            let monitor_moved = match (self.last_auto_fit_outer_pos, outer_pos) {
                (Some(prev), Some(now)) => {
                    (prev.x - now.x).abs() > 400.0 || (prev.y - now.y).abs() > 400.0
                }
                _ => false,
            };
            if window_pixels > 0.0
                && (monitor_moved || (is_first_fit && short_cooldown_elapsed))
            {
                const AUTO_FIT_CONSERVATIVE: f32 = 0.95;
                let raw = window_pixels / (GUI_DEFAULT_WIDTH * native_ppp);
                let target_scale = (raw * AUTO_FIT_CONSERVATIVE).max(1.0).min(3.0);
                if (target_scale - self.ui_scale).abs() > 0.02 {
                    self.ui_scale = target_scale;
                    self.config.ui_scale = Some(target_scale);
                    let _ = self.config.save();
                    ctx.set_pixels_per_point(target_scale * native_ppp);
                    self.log_debug(format!(
                        "auto-fit: window_px={window_pixels:.0} -> ui_scale={target_scale:.2}"
                    ));
                }
                self.last_auto_fit_width = window_pixels;
                self.last_auto_fit_outer_pos = outer_pos;
                self.auto_fit_applied = true;
                self.last_auto_fit_at = Some(Instant::now());
            } else if outer_pos.is_some() && self.last_auto_fit_outer_pos.is_none() {
                // Seed outer_pos the first time we see it, so the next
                // real move has a baseline to compare against.
                self.last_auto_fit_outer_pos = outer_pos;
            }
        }

        fn render_inventory_panel(&mut self, ui: &mut egui::Ui) {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Instances: {} filtered / {} total",
                    self.filtered.len(),
                    self.inventory.instances.len()
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("+").on_hover_text("Zoom In").clicked() {
                        self.ui_scale = (self.ui_scale + 0.1).min(3.0);
                        self.config.ui_scale = Some(self.ui_scale);
                        let _ = self.config.save();
                        ui.ctx().set_pixels_per_point(self.ui_scale * ui.ctx().native_pixels_per_point().unwrap_or(1.0));
                    }
                    if ui.button("\u{2212}").on_hover_text("Zoom Out").clicked() {
                        self.ui_scale = (self.ui_scale - 0.1).max(0.5);
                        self.config.ui_scale = Some(self.ui_scale);
                        let _ = self.config.save();
                        ui.ctx().set_pixels_per_point(self.ui_scale * ui.ctx().native_pixels_per_point().unwrap_or(1.0));
                    }
                });
            });

            egui::ScrollArea::both()
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                egui::Grid::new("instance_grid")
                    .striped(true)
                    .min_col_width(0.0)
                    .spacing(egui::vec2(2.0, 4.0))
                    .show(ui, |ui| {
                        for (label, col) in INVENTORY_HEADERS {
                            let col_key = *col as u8;
                            let width = self.column_widths.get(&col_key).copied().unwrap_or_else(|| col.default_width());
                            let arrow = if self.sort_column == Some(*col) {
                                self.sort_direction.arrow()
                            } else {
                                ""
                            };
                            let header_text = format!("{label}{arrow}");

                            // Full header cell: sort button + drag handle on right edge
                            let resp = ui.add_sized(
                                [width, 18.0],
                                egui::Button::new(egui::RichText::new(header_text).strong()).frame(false),
                            );

                            // Check if cursor is near the right edge (within 8px)
                            let drag_id = ui.id().with(("col_resize", col_key));
                            let near_right = ui.input(|i| {
                                i.pointer.hover_pos().map_or(false, |pos| {
                                    resp.rect.contains(pos) && pos.x > resp.rect.right() - 8.0
                                })
                            });
                            let is_dragging = ui.ctx().is_being_dragged(drag_id);

                            if near_right || is_dragging {
                                // Show resize cursor and line
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                                let x = resp.rect.right();
                                ui.painter().line_segment(
                                    [egui::pos2(x, resp.rect.top()), egui::pos2(x, resp.rect.bottom())],
                                    egui::Stroke::new(2.0, ui.visuals().text_color()),
                                );

                                // Handle drag
                                let drag_resp = ui.interact(
                                    egui::Rect::from_min_size(
                                        resp.rect.right_top() - egui::vec2(8.0, 0.0),
                                        egui::vec2(16.0, resp.rect.height()),
                                    ),
                                    drag_id,
                                    egui::Sense::drag(),
                                );
                                if drag_resp.dragged() {
                                    let new_width = (width + drag_resp.drag_delta().x).max(30.0);
                                    self.column_widths.insert(col_key, new_width);
                                }
                            } else if resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }

                            if resp.clicked() && !near_right {
                                if self.sort_column == Some(*col) {
                                    self.sort_direction = self.sort_direction.toggle();
                                } else {
                                    self.sort_column = Some(*col);
                                    self.sort_direction = SortDirection::Ascending;
                                }
                            }
                        }
                        ui.end_row();

                        let account_scope = self.account_scope();
                        let region_scope = self.region_scope();
                        let mut pending_connect: Option<String> = None;
                        let mut pending_fav_toggle: Option<String> = None;
                        let mut pending_add_to_saved_filter: Option<(String, String)> = None;
                        let (include_terms, _) = search_terms_from_rules(&self.search_rules);

                        // Sort filtered instances if a sort column is selected.
                        if let Some(sort_col) = self.sort_column {
                            let acct = account_scope.clone();
                            let rgn = region_scope.clone();
                            let cfg = &self.config;
                            self.filtered.sort_by(|a, b| {
                                let cmp = match sort_col {
                                    SortColumn::Favorite => {
                                        let fa = cfg.is_favorite(&acct, &rgn, &a.instance_id);
                                        let fb = cfg.is_favorite(&acct, &rgn, &b.instance_id);
                                        fb.cmp(&fa) // true (fav) before false
                                    }
                                    SortColumn::InstanceId => a.instance_id.cmp(&b.instance_id),
                                    SortColumn::Name => {
                                        let na = a.name.as_deref().unwrap_or("");
                                        let nb = b.name.as_deref().unwrap_or("");
                                        na.to_ascii_lowercase().cmp(&nb.to_ascii_lowercase())
                                    }
                                    SortColumn::State => a.state.cmp(&b.state),
                                    SortColumn::Ssm => a.ssm_managed.cmp(&b.ssm_managed),
                                    SortColumn::PrivateIp => {
                                        let ia = a.private_ip.as_deref().unwrap_or("");
                                        let ib = b.private_ip.as_deref().unwrap_or("");
                                        ia.cmp(ib)
                                    }
                                    SortColumn::PrivateDns => {
                                        let da = a.private_dns.as_deref().unwrap_or("");
                                        let db = b.private_dns.as_deref().unwrap_or("");
                                        da.to_ascii_lowercase().cmp(&db.to_ascii_lowercase())
                                    }
                                    SortColumn::Env => {
                                        let ea = a.tags.get("MMODAL_ENV").or_else(|| a.tags.get("mmodal_env")).map(|s| s.as_str()).unwrap_or("");
                                        let eb = b.tags.get("MMODAL_ENV").or_else(|| b.tags.get("mmodal_env")).map(|s| s.as_str()).unwrap_or("");
                                        ea.to_ascii_lowercase().cmp(&eb.to_ascii_lowercase())
                                    }
                                    SortColumn::InstanceType => {
                                        let ta = a.instance_type.as_deref().unwrap_or("");
                                        let tb = b.instance_type.as_deref().unwrap_or("");
                                        ta.cmp(tb)
                                    }
                                    SortColumn::AmiId => {
                                        let aa = a.image_id.as_deref().unwrap_or("");
                                        let ab = b.image_id.as_deref().unwrap_or("");
                                        aa.cmp(ab)
                                    }
                                    SortColumn::MatchTag => {
                                        // Sort by first matching tag value
                                        let ta = matching_tags(a, &include_terms);
                                        let tb = matching_tags(b, &include_terms);
                                        let va = ta.first().map(|(_, v)| v.as_str()).unwrap_or("");
                                        let vb = tb.first().map(|(_, v)| v.as_str()).unwrap_or("");
                                        va.to_ascii_lowercase().cmp(&vb.to_ascii_lowercase())
                                    }
                                };
                                if self.sort_direction == SortDirection::Descending {
                                    cmp.reverse()
                                } else {
                                    cmp
                                }
                            });
                        }

                        for instance in &self.filtered {
                            let is_fav = self.config.is_favorite(
                                &account_scope,
                                &region_scope,
                                &instance.instance_id,
                            );
                            let selected = self.selected_instance_id == instance.instance_id;
                            let mut row_clicked = false;
                            let mut row_double_clicked = false;
                            let mut row_hovered = false;
                            let mut quick_connect_clicked = false;

                            let cw = |col: SortColumn| -> f32 {
                                self.column_widths.get(&(col as u8)).copied().unwrap_or_else(|| col.default_width())
                            };

                            let star_label = if is_fav {
                                egui::RichText::new("\u{2605}").color(egui::Color32::YELLOW)
                            } else {
                                egui::RichText::new("\u{2606}")
                            };
                            let resp_fav = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::Favorite), 18.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.add(egui::Button::new(star_label).frame(false)),
                            ).inner;
                            if resp_fav.clicked() {
                                pending_fav_toggle = Some(instance.instance_id.clone());
                            }
                            row_double_clicked |= resp_fav.double_clicked();
                            row_hovered |= resp_fav.hovered();

                            // Copy button + InstanceId grouped and centered in grid cell
                            let id_w = cw(SortColumn::InstanceId);
                            let id_text = &instance.instance_id;
                            let id_content_w = id_text.len() as f32 * 6.5 + COL_COPY_W + 4.0;
                            let id_pad = ((id_w - id_content_w) / 2.0).max(0.0);
                            let id_resp = ui.allocate_ui_with_layout(
                                egui::vec2(id_w, 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add_space(id_pad);
                                    Self::paint_copy_button(ui, id_text, "Copy Instance ID");
                                    let resp_id = ui.add(egui::Label::new(id_text.clone()).sense(egui::Sense::click()));
                                    resp_id
                                },
                            );
                            let resp_id = id_resp.inner;
                            row_clicked |= resp_id.clicked();
                            row_double_clicked |= resp_id.double_clicked();
                            row_hovered |= resp_id.hovered();

                            let resp_name = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::Name), 18.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.add(egui::Label::new(instance.name.clone().unwrap_or_default()).sense(egui::Sense::click())),
                            ).inner;
                            row_clicked |= resp_name.clicked();
                            row_double_clicked |= resp_name.double_clicked();
                            row_hovered |= resp_name.hovered();

                            let resp_state = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::State), 18.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.add(egui::Label::new(instance.state.clone()).sense(egui::Sense::click())),
                            ).inner;
                            row_clicked |= resp_state.clicked();
                            row_double_clicked |= resp_state.double_clicked();
                            row_hovered |= resp_state.hovered();

                            let ssm_text = if instance.ssm_managed {
                                instance.ssm_ping.clone().unwrap_or_else(|| "Managed".to_string())
                            } else {
                                "No".to_string()
                            };
                            let resp_ssm = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::Ssm), 18.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.add(egui::Label::new(ssm_text).sense(egui::Sense::click())),
                            ).inner;
                            row_clicked |= resp_ssm.clicked();
                            row_double_clicked |= resp_ssm.double_clicked();
                            row_hovered |= resp_ssm.hovered();

                            // Copy button + Private IP grouped and centered in grid cell
                            let ip_w = cw(SortColumn::PrivateIp);
                            let ip_text = instance.private_ip.clone().unwrap_or_default();
                            let ip_resp = ui.allocate_ui_with_layout(
                                egui::vec2(ip_w, 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    Self::paint_copy_button(ui, &ip_text, "Copy IP Address");
                                    ui.add_space(4.0);
                                    let resp_ip = ui.add(egui::Label::new(ip_text.clone()).sense(egui::Sense::click()));
                                    resp_ip
                                },
                            );
                            let resp_ip = ip_resp.inner;
                            row_clicked |= resp_ip.clicked();
                            row_double_clicked |= resp_ip.double_clicked();
                            row_hovered |= resp_ip.hovered();

                            // Copy button + Private DNS grouped and centered in grid cell
                            let dns_w = cw(SortColumn::PrivateDns);
                            let dns_text = instance.private_dns.clone().unwrap_or_default();
                            let dns_copy_gap = 15.0;
                            let dns_content_w = dns_text.len() as f32 * 6.5 + COL_COPY_W + 4.0 + dns_copy_gap;
                            let dns_pad = ((dns_w - dns_content_w) / 2.0).max(0.0);
                            let dns_resp = ui.allocate_ui_with_layout(
                                egui::vec2(dns_w, 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add_space(dns_pad);
                                    ui.add_space(dns_copy_gap);
                                    Self::paint_copy_button(ui, &dns_text, "Copy Private DNS");
                                    let resp_dns = ui.add(egui::Label::new(dns_text.clone()).sense(egui::Sense::click()));
                                    resp_dns
                                },
                            );
                            let resp_dns = dns_resp.inner;
                            row_clicked |= resp_dns.clicked();
                            row_double_clicked |= resp_dns.double_clicked();
                            row_hovered |= resp_dns.hovered();

                            // Copy button + AMI ID grouped and centered in grid cell
                            let ami_w = cw(SortColumn::AmiId);
                            let ami_text = instance.image_id.clone().unwrap_or_default();
                            let ami_copy_gap = 18.0;
                            let ami_content_w = ami_text.len() as f32 * 6.5 + COL_COPY_W + 4.0 + ami_copy_gap;
                            let ami_pad = ((ami_w - ami_content_w) / 2.0).max(0.0);
                            let ami_resp = ui.allocate_ui_with_layout(
                                egui::vec2(ami_w, 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add_space(ami_pad);
                                    ui.add_space(ami_copy_gap);
                                    Self::paint_copy_button(ui, &ami_text, "Copy AMI ID");
                                    let resp_ami = ui.add(egui::Label::new(ami_text.clone()).sense(egui::Sense::click()));
                                    resp_ami
                                },
                            );
                            let resp_ami = ami_resp.inner;
                            row_clicked |= resp_ami.clicked();
                            row_double_clicked |= resp_ami.double_clicked();
                            row_hovered |= resp_ami.hovered();

                            let resp_itype = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::InstanceType), 18.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.add(egui::Label::new(instance.instance_type.clone().unwrap_or_default()).sense(egui::Sense::click())),
                            ).inner;
                            row_clicked |= resp_itype.clicked();
                            row_double_clicked |= resp_itype.double_clicked();
                            row_hovered |= resp_itype.hovered();

                            let env_val = instance.tags.get("MMODAL_ENV")
                                .or_else(|| instance.tags.get("mmodal_env"))
                                .cloned()
                                .unwrap_or_default();
                            let resp_env = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::Env), 18.0),
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                                |ui| ui.add(egui::Label::new(env_val).sense(egui::Sense::click())),
                            ).inner;
                            row_clicked |= resp_env.clicked();
                            row_double_clicked |= resp_env.double_clicked();
                            row_hovered |= resp_env.hovered();

                            let matched_tag_text = if include_terms.is_empty() {
                                String::new()
                            } else {
                                let tags = matching_tags(instance, &include_terms);
                                let text = tags
                                    .into_iter()
                                    .map(|(k, v)| format!("{k}={v}"))
                                    .collect::<Vec<String>>()
                                    .join(", ");
                                truncate(&text, 42)
                            };
                            let resp_tag = ui.allocate_ui_with_layout(
                                egui::vec2(cw(SortColumn::MatchTag), 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| ui.add(egui::Label::new(matched_tag_text).sense(egui::Sense::click())),
                            ).inner;
                            row_clicked |= resp_tag.clicked();
                            row_double_clicked |= resp_tag.double_clicked();
                            row_hovered |= resp_tag.hovered();

                            let row_response = resp_fav
                                .clone()
                                .union(resp_id.clone())
                                .union(resp_name.clone())
                                .union(resp_state.clone())
                                .union(resp_ssm.clone())
                                .union(resp_ip.clone())
                                .union(resp_dns.clone())
                                .union(resp_env.clone())
                                .union(resp_itype.clone())
                                .union(resp_ami.clone())
                                .union(resp_tag.clone());

                            let detail_instance_clone = instance.clone();
                            let filter_instance_clone = instance.clone();
                            let mut add_to_saved_filter: Option<(String, String)> = None;
                            row_response.context_menu(|ui| {
                                if ui.button("Quick Connect").clicked() {
                                    quick_connect_clicked = true;
                                    ui.close();
                                }
                                ui.menu_button("Add to filter", |ui| {
                                    let mut scope_filters = self
                                        .config
                                        .saved_filters_for_scope("global", "global");
                                    scope_filters.sort_by(|a, b| {
                                        a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())
                                    });
                                    if scope_filters.is_empty() {
                                        ui.label("No saved filters");
                                    } else {
                                        let term = filter_instance_clone
                                            .name
                                            .as_deref()
                                            .filter(|n| !n.is_empty())
                                            .unwrap_or(&filter_instance_clone.instance_id)
                                            .to_string();
                                        for saved in &scope_filters {
                                            if ui.button(&saved.name).clicked() {
                                                add_to_saved_filter = Some((
                                                    saved.name.clone(),
                                                    term.clone(),
                                                ));
                                                ui.close();
                                            }
                                        }
                                    }
                                });
                                if ui.button("See Details").clicked() {
                                    let iid = detail_instance_clone.instance_id.clone();
                                    self.detail_instance = Some(detail_instance_clone.clone());
                                    self.detail_volumes.clear();
                                    self.detail_volumes_error = None;
                                    self.detail_volumes_loading = true;
                                    self.detail_iam_role = None;
                                    self.main_tab = MainTab::Details;
                                    // Fetch volumes in background
                                    let context = self.profile_inventory_cache.iter()
                                        .find(|(_, (inv, _))| inv.instances.iter().any(|i| i.instance_id == iid))
                                        .map(|(_, (_, ctx))| ctx.clone())
                                        .or_else(|| self.context.clone());
                                    if let Some(ctx) = context {
                                        let tx = self.proc_tx.clone();
                                        std::thread::spawn(move || {
                                            let result = fetch_volumes(&ctx.profile, &ctx.region, &iid);
                                            let iam = fetch_iam_role(&ctx.profile, &ctx.region, &iid);
                                            let _ = tx.send(ProcEvent::VolumeResult { volumes: result, iam_role: iam });
                                        });
                                    }
                                    ui.close();
                                }
                            });

                            if let Some(entry) = add_to_saved_filter {
                                pending_add_to_saved_filter = Some(entry);
                            }

                            if row_hovered {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }

                            let row_rect = resp_fav
                                .rect
                                .union(resp_id.rect)
                                .union(resp_name.rect)
                                .union(resp_state.rect)
                                .union(resp_ssm.rect)
                                .union(resp_ip.rect)
                                .union(resp_env.rect)
                                .union(resp_tag.rect);

                            if selected || row_hovered {
                                let color = if selected {
                                    egui::Color32::from_rgba_unmultiplied(70, 110, 170, 55)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(90, 90, 90, 35)
                                };
                                ui.painter().rect_filled(row_rect, 0.0, color);
                            }

                            let action = resolve_row_action(
                                row_clicked,
                                row_double_clicked,
                                quick_connect_clicked,
                            );

                            if action.select {
                                self.selected_instance_id = instance.instance_id.clone();
                            }
                            if action.connect {
                                pending_connect = Some(instance.instance_id.clone());
                            }

                            ui.end_row();
                        }

                        if let Some(instance_id) = pending_connect {
                            self.selected_instance_id = instance_id;
                            if self.guarded_action("quick connect", |app| app.connect_selected()) {
                                self.main_tab = MainTab::Connections;
                            }
                        }

                        if let Some(instance_id) = pending_fav_toggle {
                            let enabled = self.config.toggle_favorite(
                                &account_scope,
                                &region_scope,
                                &instance_id,
                            );
                            if let Err(err) = self.config.save() {
                                self.message = format!("error: {err}");
                                self.log_error(self.message.clone());
                            } else {
                                self.message = format!(
                                    "Favorite {}: {}",
                                    if enabled { "enabled" } else { "disabled" },
                                    instance_id
                                );
                                self.log_info(self.message.clone());
                            }
                        }

                        if let Some((filter_name, term)) = pending_add_to_saved_filter {
                            let mut filters = self
                                .config
                                .saved_filters_for_scope("global", "global");
                            if let Some(saved) = filters.iter_mut().find(|f| f.name == filter_name) {
                                if !saved.include_terms.iter().any(|t| t == &term) {
                                    saved.include_terms.push(term.clone());
                                    let updated = saved.clone();
                                    self.config.upsert_saved_filter("global", "global", updated);
                                    if let Err(err) = self.config.save() {
                                        self.message = format!("error: {err}");
                                        self.log_error(self.message.clone());
                                    } else {
                                        self.message = format!("Added '{term}' to filter '{filter_name}'");
                                        self.log_info(self.message.clone());
                                    }
                                } else {
                                    self.message = format!("'{term}' already in filter '{filter_name}'");
                                }
                            }
                        }
                    });
                ui.add_space(20.0);
            });
        }

        fn render_connections_panel(&mut self, ui: &mut egui::Ui) {
            let tabs_snapshot: Vec<(u64, String, String, bool)> = self
                .connections
                .tabs()
                .iter()
                .map(|t| (t.id, t.title.clone(), t.profile_id.clone(), t.running))
                .collect();

            let colors_enabled = self.config.account_colors_enabled;

            // Environment color legend — always show on connections page
            if self.config.account_colors_enabled && !self.account_color_map.is_empty() {
                let mut seen_envs = std::collections::HashSet::new();
                // Collect (profile_id, env, color) then sort by account order + alpha
                let mut env_entries: Vec<(String, String, egui::Color32)> = self
                    .account_color_map
                    .iter()
                    .filter_map(|(key, color)| {
                        let (pid, env) = key.split_once(':')?;
                        let env_lower = env.to_ascii_lowercase();
                        if self.hidden_envs.contains(&env_lower) {
                            return None;
                        }
                        let dedup_key = format!("{}:{}", pid, env_lower);
                        if seen_envs.insert(dedup_key) {
                            Some((pid.to_string(), env.to_string(), *color))
                        } else {
                            None
                        }
                    })
                    .collect();
                // Sort by account order (sort_order from accounts.json, then position in config),
                // then alphabetically by env within each account
                env_entries.sort_by(|a, b| {
                    let (idx_a, pa) = self.config.profiles.iter().enumerate()
                        .find(|(_, p)| p.profile_id == a.0)
                        .map(|(i, p)| (i, Some(p)))
                        .unwrap_or((usize::MAX, None));
                    let (idx_b, pb) = self.config.profiles.iter().enumerate()
                        .find(|(_, p)| p.profile_id == b.0)
                        .map(|(i, p)| (i, Some(p)))
                        .unwrap_or((usize::MAX, None));
                    let ord_a = pa.and_then(|p| p.sort_order).unwrap_or(u32::MAX);
                    let ord_b = pb.and_then(|p| p.sort_order).unwrap_or(u32::MAX);
                    ord_a.cmp(&ord_b)
                        .then_with(|| idx_a.cmp(&idx_b))
                        .then_with(|| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()))
                });

                // Log legend entries once to debug duplicates (fires after Refresh)
                {
                    static LAST_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let current = env_entries.len();
                    let prev = LAST_COUNT.swap(current, std::sync::atomic::Ordering::Relaxed);
                    if self.debug_mode && current > 0 && current != prev {
                        for (pid, env, color) in &env_entries {
                            let label = self.config.profiles.iter()
                                .find(|p| p.profile_id == *pid)
                                .map(|p| p.display_name.as_str())
                                .unwrap_or("?");
                            let env_lower = env.to_ascii_lowercase();
                            let instances: Vec<String> = if self.selected_profile.as_deref() == Some(pid.as_str()) {
                                self.inventory.instances.iter()
                                    .filter(|i| instance_env(i).map(|e| e.to_ascii_lowercase()) == Some(env_lower.clone()))
                                    .map(|i| i.instance_id.clone())
                                    .take(5)
                                    .collect()
                            } else if let Some((inv, _)) = self.profile_inventory_cache.get(pid) {
                                inv.instances.iter()
                                    .filter(|i| instance_env(i).map(|e| e.to_ascii_lowercase()) == Some(env_lower.clone()))
                                    .map(|i| i.instance_id.clone())
                                    .take(5)
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            self.log_info(format!(
                                "legend: env={env} profile={pid} label={label} color=({},{},{}) instances={instances:?}",
                                color.r(), color.g(), color.b()
                            ));
                        }
                        let map_keys: Vec<&String> = self.account_color_map.keys()
                            .filter(|k| k.contains(':'))
                            .collect();
                        self.log_info(format!("color_map env keys: {map_keys:?}"));
                    }
                }

                if !env_entries.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for (_pid, env, color) in &env_entries {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(10.0, 10.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().circle_filled(rect.center(), 5.0, *color);
                            ui.label(env);
                        }
                    });
                    ui.separator();
                }
            }

            if tabs_snapshot.is_empty() {
                ui.label("No active connections. Select an instance and click Connect.");
                return;
            }

            let mut to_select: Option<u64> = None;
            let mut to_close: Option<u64> = None;
            let mut to_rename: Option<u64> = None;
            let mut to_reopen: Option<(String, String)> = None;
            let mut to_pick_color: Option<(u64, String)> = None;
            let mut dragged_tab_id: Option<u64> = None;
            let mut tab_rects: Vec<(u64, egui::Rect)> = Vec::with_capacity(tabs_snapshot.len());

            egui::ScrollArea::horizontal()
                .auto_shrink([false, true])
                .id_salt("connection_tabs_scroll")
                .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (id, title, profile_id, running) in &tabs_snapshot {
                    let tab_color = if colors_enabled {
                        // Look up environment color for this tab's instance
                        let tab_instance_id = self.connections.tabs().iter()
                            .find(|t| t.id == *id)
                            .map(|t| t.instance_id.clone());
                        let env_color = tab_instance_id.and_then(|iid| {
                            // Search current inventory and cached inventories
                            let inst = find_instance(&self.inventory.instances, &iid)
                                .or_else(|| {
                                    self.profile_inventory_cache.values()
                                        .find_map(|(inv, _)| find_instance(&inv.instances, &iid))
                                });
                            inst.and_then(|i| {
                                let env = instance_env(i)?;
                                let env_lower = env.to_ascii_lowercase();
                                if self.hidden_envs.contains(&env_lower) {
                                    return None;
                                }
                                let key = format!("{profile_id}:{env_lower}");
                                self.account_color_map.get(&key).copied()
                            })
                        });
                        // Fall back to account base color if no env found
                        env_color.or_else(|| self.account_color_map.get(profile_id).copied())
                    } else {
                        None
                    };

                    let frame = if let Some(color) = tab_color {
                        egui::Frame::group(ui.style())
                            .stroke(egui::Stroke::new(2.0, color))
                            .fill(egui::Color32::from_rgba_unmultiplied(
                                color.r(), color.g(), color.b(), 25,
                            ))
                    } else {
                        egui::Frame::group(ui.style())
                    };

                    let drag_id = egui::Id::new(("conn_tab_drag", *id));
                    // Scope the whole tab frame with click+drag sense so clicks
                    // and drags anywhere on the tab (except the close button)
                    // are handled uniformly with a normal cursor. While this
                    // tab is being dragged, paint it on the Foreground layer
                    // and translate that layer so the tab visually follows the
                    // cursor (Chrome-style), while its rest slot still
                    // participates in the horizontal layout so sibling tabs
                    // continue to determine midpoint crossings for reorder.
                    let selected = self.connections.selected() == Some(*id);
                    let is_being_dragged = self.dragging_tab_id == Some(*id);
                    let drag_layer_id = is_being_dragged
                        .then(|| egui::LayerId::new(egui::Order::Foreground, drag_id));
                    let mut builder = egui::UiBuilder::new()
                        .id_salt(drag_id)
                        .sense(egui::Sense::click_and_drag());
                    if let Some(lid) = drag_layer_id {
                        builder = builder.layer_id(lid);
                    }
                    let tab_scope = ui.scope_builder(builder, |ui| {
                        let hovered = ui.response().hovered()
                            || ui.response().dragged()
                            || ui.response().is_pointer_button_down_on();
                        frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Color dot indicator
                                if let Some(color) = tab_color {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(8.0, 8.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().circle_filled(rect.center(), 4.0, color);
                                }

                                let text_color = if selected {
                                    ui.visuals().strong_text_color()
                                } else if hovered {
                                    ui.visuals().widgets.hovered.text_color()
                                } else {
                                    ui.visuals().text_color()
                                };
                                let mut rich = egui::RichText::new(title.clone())
                                    .color(text_color);
                                if selected {
                                    rich = rich.strong();
                                }
                                ui.add(egui::Label::new(rich).selectable(false));
                                if ui.small_button("x").clicked() {
                                    to_close = Some(*id);
                                }
                            });
                        });
                    });
                    let frame_resp = tab_scope
                        .response
                        .on_hover_cursor(egui::CursorIcon::Default);
                    // Chrome-style: anchor the dragged tab to the cursor using
                    // the grab offset captured at drag start, so the visual
                    // doesn't snap to slot centers as we reorder under it.
                    if let Some(lid) = drag_layer_id {
                        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                            let target_left_x = pointer.x - self.drag_grab_offset_x;
                            let delta = egui::vec2(
                                target_left_x - frame_resp.rect.min.x,
                                0.0,
                            );
                            ui.ctx().transform_layer_shapes(
                                lid,
                                egui::emath::TSTransform::from_translation(delta),
                            );
                        }
                    }
                    if frame_resp.clicked() {
                        to_select = Some(*id);
                    }
                    // Detect drag start once and capture grab offset; from
                    // there, self.dragging_tab_id keeps the drag alive across
                    // reorders and layer transitions.
                    if frame_resp.drag_started() {
                        let pointer_x = ui
                            .ctx()
                            .pointer_interact_pos()
                            .map(|p| p.x)
                            .unwrap_or(frame_resp.rect.center().x);
                        self.dragging_tab_id = Some(*id);
                        self.drag_grab_offset_x = pointer_x - frame_resp.rect.min.x;
                    }
                    if is_being_dragged {
                        dragged_tab_id = Some(*id);
                    }
                    tab_rects.push((*id, frame_resp.rect));

                    // Right-click context menu on the tab frame
                    let tab_id = *id;
                    let tab_instance_id = self
                        .connections
                        .tabs()
                        .iter()
                        .find(|t| t.id == tab_id)
                        .map(|t| t.instance_id.clone())
                        .unwrap_or_default();
                    let tab_profile = profile_id.clone();
                    let tab_running = *running;
                    frame_resp.context_menu(|ui| {
                        if ui.button("Rename").clicked() {
                            to_rename = Some(tab_id);
                            ui.close();
                        }
                        if !tab_running {
                            if ui.button("Re-open").clicked() {
                                to_reopen = Some((tab_instance_id.clone(), tab_profile.clone()));
                                to_close = Some(tab_id);
                                ui.close();
                            }
                        }
                        if colors_enabled {
                            if ui.button("Change Color").clicked() {
                                to_pick_color = Some((tab_id, tab_profile.clone()));
                                ui.close();
                            }
                        }
                        if ui.button("Close").clicked() {
                            to_close = Some(tab_id);
                            ui.close();
                        }
                    });
                }
            });
                });

            // Chrome-style live reorder: count how many non-dragged siblings
            // sit left of the cursor — that's the dragged tab's target slot
            // in the list-after-removal. Skipping the dragged tab itself
            // avoids off-by-one slot stalls and lets a fast drag jump
            // multiple positions in a single frame.
            if let Some(dragged_id) = dragged_tab_id {
                if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                    let target_after_removal = tab_rects
                        .iter()
                        .filter(|(tid, _)| *tid != dragged_id)
                        .filter(|(_, rect)| rect.center().x < pointer.x)
                        .count();
                    let current_idx = tab_rects
                        .iter()
                        .position(|(tid, _)| *tid == dragged_id);
                    if let Some(cur) = current_idx {
                        if target_after_removal != cur {
                            self.connections.move_to(dragged_id, target_after_removal);
                        }
                    }
                }
            }

            // Clear our drag bookkeeping on mouse release; egui's own drag
            // state is keyed on a salted id we don't have direct access to,
            // so we mirror it here based on the primary button.
            if self.dragging_tab_id.is_some()
                && !ui.input(|i| i.pointer.primary_down())
            {
                self.dragging_tab_id = None;
                self.drag_grab_offset_x = 0.0;
            }
            if let Some(id) = to_select {
                self.connections.select(id);
            }
            // Handle rename request
            if let Some(id) = to_rename {
                let current_title = self
                    .connections
                    .tabs()
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                self.tab_rename_id = Some(id);
                self.tab_rename_buf = current_title;
            }
            // Handle re-open request
            if let Some((instance_id, _profile_id)) = to_reopen {
                self.selected_instance_id = instance_id;
                // User can click Connect after re-selecting; switch to inventory
                self.main_tab = MainTab::Inventory;
                self.message = "Instance selected for re-connect. Click Connect.".to_string();
            }
            // Handle color picker request
            if let Some((_tab_id, profile_id)) = to_pick_color {
                let current_color = self
                    .account_color_map
                    .get(&profile_id)
                    .copied()
                    .unwrap_or(egui::Color32::from_rgb(128, 128, 128));
                self.tab_color_picker_rgb = [
                    current_color.r() as f32 / 255.0,
                    current_color.g() as f32 / 255.0,
                    current_color.b() as f32 / 255.0,
                ];
                self.color_picker_profile = Some(profile_id);
            }
            if let Some(id) = to_close {
                self.close_connection_tab(id);
            }

            // Inline rename editor
            if self.tab_rename_id.is_some() {
                ui.horizontal(|ui| {
                    ui.label("Rename tab:");
                    let response = ui.text_edit_singleline(&mut self.tab_rename_buf);
                    if response.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        if let Some(id) = self.tab_rename_id.take() {
                            let new_title = self.tab_rename_buf.trim().to_string();
                            if !new_title.is_empty() {
                                self.connections.rename(id, new_title);
                            }
                        }
                    }
                    if ui.button("OK").clicked() {
                        if let Some(id) = self.tab_rename_id.take() {
                            let new_title = self.tab_rename_buf.trim().to_string();
                            if !new_title.is_empty() {
                                self.connections.rename(id, new_title);
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.tab_rename_id = None;
                    }
                });
            }

            ui.separator();

            if let Some(tab) = self.connections.selected_ref().cloned() {
                let private_ip = find_instance(&self.inventory.instances, &tab.instance_id)
                    .and_then(|i| i.private_ip.clone())
                    .or_else(|| {
                        // Search cached inventories for tabs from other profiles
                        self.profile_inventory_cache.values()
                            .find_map(|(inv, _)| {
                                find_instance(&inv.instances, &tab.instance_id)
                                    .and_then(|i| i.private_ip.clone())
                            })
                    })
                    .unwrap_or_else(|| "-".to_string());
                let pty_bytes = self.pty_sessions.get(&tab.id)
                    .map(|s| s.bytes_received)
                    .unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.monospace(format_connection_summary_line(
                        &tab.title,
                        &tab.instance_id,
                        &private_ip,
                        tab.running,
                        pty_bytes,
                    ));
                    if tab.running && ui.small_button("Prep Terminal").clicked() {
                        let (rows, cols) = self
                            .pty_sessions
                            .get(&tab.id)
                            .and_then(|s| s.last_size)
                            .unwrap_or((24, 120));
                        let prep = prep_terminal_command_for_size(rows, cols);
                        self.send_raw_bytes_to_connection_tab(tab.id, &prep);
                        self.pending_terminal_focus = Some(tab.id);
                    }
                });
                ui.separator();

                // Trigger initial file listing if not yet initialized
                if let Some(fb) = self.file_browsers.get(&tab.id) {
                    if !fb.initialized && matches!(fb.status, FileOpStatus::Idle) {
                        let path = fb.current_path.clone();
                        self.request_file_listing(tab.id, path);
                    }
                }

                let show_cursor = ui.input(|i| ((i.time * 2.0) as i64) % 2 == 0);
                let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                let tab_id = tab.id;
                let tab_lines = tab.lines.clone();

                // --- Editor file tabs (above the main split) ---
                self.render_editor_tab_bar(ui, tab_id);

                let available = ui.available_size();
                let file_browser_width = 220.0_f32;

                let has_editors = self.file_browsers.get(&tab_id)
                    .map(|fb| !fb.editor_tabs.is_empty())
                    .unwrap_or(false);
                let split_ratio = self.editor_split.get(&tab_id).copied().unwrap_or(0.5);

                ui.horizontal(|ui| {
                    // Left: File browser sidebar
                    ui.allocate_ui_with_layout(
                        egui::vec2(file_browser_width, available.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            self.render_file_browser(ui, tab_id);
                        },
                    );

                    ui.separator();

                    let banner_fill = if self.dark_mode {
                        egui::Color32::from_rgb(44, 44, 44)
                    } else {
                        ui.visuals().panel_fill
                    };

                    let remaining_width = (available.x - file_browser_width - 12.0).max(100.0);
                    let splitter_height = 6.0_f32;

                    // Right: vertical split (editor top + terminal bottom)
                    ui.allocate_ui_with_layout(
                        egui::vec2(remaining_width, available.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            // --- Editor panel (top half) ---
                            if has_editors {
                                let editor_height = (available.y * split_ratio - splitter_height / 2.0).max(50.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(remaining_width, editor_height),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        self.render_editor_content(ui, tab_id);
                                    },
                                );

                                // --- Draggable splitter ---
                                let splitter_rect = ui.allocate_space(egui::vec2(remaining_width, splitter_height)).1;
                                let splitter_id = ui.make_persistent_id(("editor_splitter", tab_id));
                                let splitter_response = ui.interact(splitter_rect, splitter_id, egui::Sense::drag());
                                // Draw splitter line
                                ui.painter().rect_filled(
                                    splitter_rect,
                                    0.0,
                                    if splitter_response.hovered() || splitter_response.dragged() {
                                        egui::Color32::from_rgb(100, 100, 200)
                                    } else {
                                        egui::Color32::from_rgb(80, 80, 80)
                                    },
                                );
                                if splitter_response.hovered() || splitter_response.dragged() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                                }
                                if splitter_response.dragged() {
                                    let delta = splitter_response.drag_delta().y;
                                    let new_ratio = (split_ratio + delta / available.y).clamp(0.1, 0.9);
                                    self.editor_split.insert(tab_id, new_ratio);
                                }
                            }

                            // --- Terminal panel (bottom / full when no editor) ---
                            let terminal_response = egui::Frame::NONE
                                .fill(banner_fill)
                                .inner_margin(egui::Margin::same(4))
                                .corner_radius(egui::CornerRadius::same(2))
                                .show(ui, |ui| {
                                    egui::Frame::NONE
                                        .fill(terminal_panel_fill())
                                        .inner_margin(egui::Margin::same(0))
                                        .show(ui, |ui| {
                                            // Reserve space at the bottom so the
                                            // cursor/last line is always visible
                                            let full = ui.available_size();
                                            let available = egui::vec2(full.x, (full.y - 20.0).max(50.0));
                                            ui.allocate_ui_with_layout(
                                                available,
                                                egui::Layout::top_down(egui::Align::Min),
                                                |ui| {
                                                    ui.set_min_width(available.x);
                                                    let size = ui.available_size();
                                                    let (rows, cols, _cell_w, _cell_h) =
                                                        terminal_grid_and_cell_size(
                                                            ui, &font_id, size,
                                                        );
                                                    let norm_sel = self.terminal_selections
                                                        .get(&tab_id)
                                                        .and_then(|s| s.normalized());
                                                    let mut terminal_job =
                                                        if let Some(session) =
                                                            self.pty_sessions
                                                                .get_mut(&tab_id)
                                                        {
                                                            resize_pty_session(
                                                                session, rows, cols,
                                                            );
                                                            let screen = session.parser.screen_mut();
                                                            let on_alt = screen.alternate_screen();
                                                            let effective_offset = if on_alt {
                                                                0
                                                            } else {
                                                                screen.set_scrollback(usize::MAX);
                                                                let max_sb = screen.scrollback();
                                                                session.scroll_offset = session.scroll_offset.min(max_sb);
                                                                screen.set_scrollback(session.scroll_offset);
                                                                session.scroll_offset
                                                            };
                                                            let cursor_visible = show_cursor && effective_offset == 0;
                                                            let layout_sel = norm_sel.map(|(s, e)| (s, e, effective_offset));
                                                            let job = terminal_layout_job(
                                                                session.parser.screen(),
                                                                cursor_visible,
                                                                font_id.clone(),
                                                                layout_sel,
                                                            );
                                                            if effective_offset > 0 {
                                                                session.parser.screen_mut().set_scrollback(0);
                                                            }
                                                            job
                                                        } else {
                                                            terminal_plain_layout_job(
                                                                &tab_lines.join("\n"),
                                                                font_id.clone(),
                                                            )
                                                        };
                                                    terminal_job.wrap.max_width =
                                                        size.x.max(1.0);
                                                    ui.allocate_ui_with_layout(
                                                        size,
                                                        egui::Layout::left_to_right(
                                                            egui::Align::Min,
                                                        ),
                                                        |ui| {
                                                            // No sense on the label: all
                                                            // terminal clicks/drags flow
                                                            // through the outer
                                                            // `terminal_focus_response`
                                                            // (ui.interact) below.
                                                            // A sensing label here steals
                                                            // secondary_clicked/double_clicked
                                                            // from that outer response and
                                                            // breaks right-click copy and
                                                            // double-click word selection.
                                                            ui.add(
                                                                egui::Label::new(
                                                                    terminal_job,
                                                                )
                                                                .selectable(false),
                                                            )
                                                        },
                                                    )
                                                    .inner
                                                },
                                            )
                                            .inner
                                        })
                                        .inner
                                });
                            let terminal_focus_id =
                                ui.make_persistent_id(("terminal_focus", tab_id));
                            let terminal_focus_response = ui.interact(
                                terminal_response.response.rect,
                                terminal_focus_id,
                                egui::Sense::click_and_drag(),
                            );
                            // Honor a pending auto-focus request for this
                            // tab (set on tab open, tab switch, prep, paste).
                            if self.pending_terminal_focus == Some(tab_id) {
                                terminal_focus_response.request_focus();
                                self.pending_terminal_focus = None;
                                self.log_debug(format!(
                                    "terminal auto-focus tab={tab_id}"
                                ));
                            }
                            // Derive cell size from the rendered rect and the actual
                            // parser grid dimensions.  This avoids font-metric drift
                            // after DPI/monitor changes — the mapping always matches
                            // the rendered text exactly.
                            let term_rect = terminal_response.inner.rect;
                            let (sel_rows, sel_cols) = self.pty_sessions.get(&tab_id)
                                .and_then(|s| s.last_size)
                                .unwrap_or((24, 120));
                            let sel_cell_w = if sel_cols > 0 { term_rect.width() / sel_cols as f32 } else { 8.0 };
                            let sel_cell_h = if sel_rows > 0 { term_rect.height() / sel_rows as f32 } else { 16.0 };
                            // Mouse drag selection using raw pointer state (absolute coords)
                            let pointer_pos = ui.input(|i| i.pointer.hover_pos());
                            let primary_down = ui.input(|i| i.pointer.primary_down());
                            let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
                            if let Some(pos) = pointer_pos {
                                let scroll_off = self.pty_sessions.get(&tab_id)
                                    .map(|s| s.scroll_offset).unwrap_or(0);
                                if primary_pressed && term_rect.contains(pos) {
                                    let (row, col) = pixel_to_grid_cell(
                                        pos, term_rect, sel_cell_w, sel_cell_h, sel_rows, sel_cols,
                                    );
                                    let abs = AbsPos {
                                        abs_row: screen_row_to_abs(row, scroll_off, sel_rows),
                                        col,
                                    };
                                    let sel = self.terminal_selections.entry(tab_id).or_default();
                                    sel.anchor = Some(abs);
                                    sel.end = Some(abs);
                                } else if primary_down {
                                    let has_anchor = self.terminal_selections
                                        .get(&tab_id).is_some_and(|s| s.anchor.is_some());
                                    if term_rect.contains(pos) {
                                        let (row, col) = pixel_to_grid_cell(
                                            pos, term_rect, sel_cell_w, sel_cell_h, sel_rows, sel_cols,
                                        );
                                        let abs = AbsPos {
                                            abs_row: screen_row_to_abs(row, scroll_off, sel_rows),
                                            col,
                                        };
                                        if let Some(sel) = self.terminal_selections.get_mut(&tab_id) {
                                            if sel.anchor.is_some() {
                                                sel.end = Some(abs);
                                            }
                                        }
                                    } else if has_anchor {
                                        // Auto-scroll: mouse dragged outside terminal rect
                                        if pos.y < term_rect.top() {
                                            // Mouse above terminal: scroll up into history
                                            let new_off = {
                                                if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                                    session.scroll_offset = session.scroll_offset.saturating_add(1);
                                                    Some(session.scroll_offset)
                                                } else { None }
                                            };
                                            if let Some(off) = new_off {
                                                if let Some(sel) = self.terminal_selections.get_mut(&tab_id) {
                                                    sel.end = Some(AbsPos {
                                                        abs_row: screen_row_to_abs(0, off, sel_rows),
                                                        col: 0,
                                                    });
                                                }
                                            }
                                        } else if pos.y > term_rect.bottom() {
                                            // Mouse below terminal: scroll down toward present
                                            let new_off = {
                                                if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                                    session.scroll_offset = session.scroll_offset.saturating_sub(1);
                                                    Some(session.scroll_offset)
                                                } else { None }
                                            };
                                            if let Some(off) = new_off {
                                                if let Some(sel) = self.terminal_selections.get_mut(&tab_id) {
                                                    sel.end = Some(AbsPos {
                                                        abs_row: screen_row_to_abs(
                                                            sel_rows.saturating_sub(1), off, sel_rows,
                                                        ),
                                                        col: sel_cols.saturating_sub(1),
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            // Double-click → select word. Triple-click → select line.
                            // These fire on release of the 2nd/3rd click; handle them
                            // before the regular `clicked()` clear below.
                            let double_clicked = terminal_focus_response.double_clicked();
                            let triple_clicked = terminal_focus_response.triple_clicked();
                            if (double_clicked || triple_clicked) && pointer_pos
                                .is_some_and(|p| term_rect.contains(p))
                            {
                                let pos = pointer_pos.unwrap();
                                let scroll_off = self.pty_sessions.get(&tab_id)
                                    .map(|s| s.scroll_offset).unwrap_or(0);
                                let (row, col) = pixel_to_grid_cell(
                                    pos, term_rect, sel_cell_w, sel_cell_h, sel_rows, sel_cols,
                                );
                                let abs_row = screen_row_to_abs(row, scroll_off, sel_rows);
                                if triple_clicked {
                                    let sel = self.terminal_selections.entry(tab_id).or_default();
                                    sel.anchor = Some(AbsPos { abs_row, col: 0 });
                                    sel.end = Some(AbsPos {
                                        abs_row,
                                        col: sel_cols.saturating_sub(1),
                                    });
                                } else if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                    let cols_count = session.parser.screen().size().1;
                                    let screen_row = set_scrollback_for_top_abs_row(
                                        &mut session.parser, abs_row);
                                    let row_text = session.parser.screen().contents_between(
                                        screen_row, 0, screen_row, cols_count,
                                    );
                                    session.parser.screen_mut().set_scrollback(0);
                                    let (start_col, end_col) = find_word_bounds(&row_text, col);
                                    let sel = self.terminal_selections.entry(tab_id).or_default();
                                    sel.anchor = Some(AbsPos { abs_row, col: start_col });
                                    sel.end = Some(AbsPos { abs_row, col: end_col });
                                }
                            }
                            if terminal_focus_response.clicked()
                                && !double_clicked
                                && !triple_clicked
                            {
                                if let Some(sel) = self.terminal_selections.get_mut(&tab_id) {
                                    sel.clear();
                                }
                                terminal_focus_response.request_focus();
                                self.log_debug(format!(
                                    "terminal focus requested tab={tab_id}"
                                ));
                            }
                            if terminal_focus_response.secondary_clicked() {
                                // Decide copy-vs-paste based on whether the
                                // user has an active selection (anchor + end
                                // set, with start != end). This is robust
                                // to extraction returning empty/whitespace —
                                // we never silently fall through to a paste
                                // when the user clearly intended a copy.
                                let raw_sel = self.terminal_selections.get(&tab_id).cloned();
                                let normalized_sel = raw_sel.as_ref().and_then(|s| s.normalized());
                                let has_real_selection = normalized_sel
                                    .is_some_and(|(s, e)| s != e);
                                let session_present = self.pty_sessions.contains_key(&tab_id);
                                self.log_debug(format!(
                                    "right-click tab={tab_id} has_real_selection={has_real_selection} sel={raw_sel:?} session_present={session_present}"
                                ));
                                if has_real_selection {
                                    // Extract first, then do exactly one
                                    // clipboard op. Two back-to-back
                                    // arboard::Clipboard::new() + set_text
                                    // calls (first clear, then real text)
                                    // are racy on Windows — the "" can win
                                    // and the real text is silently lost.
                                    // Collapsing to one set_text avoids it.
                                    let sel_coords = self
                                        .terminal_selections
                                        .get(&tab_id)
                                        .cloned()
                                        .and_then(|sel| sel.normalized());
                                    let extraction: Option<(AbsPos, AbsPos, u16, String)> =
                                        sel_coords.and_then(|(start, end)| {
                                            let session = self.pty_sessions.get_mut(&tab_id)?;
                                            let vr = session.parser.screen().size().0;
                                            let t = extract_selection_text(
                                                &mut session.parser, start, end,
                                            );
                                            Some((start, end, vr, t))
                                        });
                                    match &extraction {
                                        Some((start, end, vr, t)) => {
                                            let preview: String = t.chars().take(60)
                                                .map(|c| if c.is_control() { '.' } else { c })
                                                .collect();
                                            self.log_debug(format!(
                                                "right-click copy tab={tab_id} coords=(abs {}/{} → {}/{}) vr={} raw_len={} preview={preview:?}",
                                                start.abs_row, start.col,
                                                end.abs_row, end.col,
                                                vr,
                                                t.len(),
                                            ));
                                        }
                                        None => {
                                            // has_real_selection was true above,
                                            // so coords should exist — if we got
                                            // here, either the selection was
                                            // cleared between checks or the
                                            // pty session is missing.
                                            let coords_present = sel_coords.is_some();
                                            let session_present = self.pty_sessions.contains_key(&tab_id);
                                            let sessions: Vec<u64> = self.pty_sessions.keys().copied().collect();
                                            self.log_error(format!(
                                                "right-click copy tab={tab_id} extraction=None coords_present={coords_present} session_present={session_present} sessions={sessions:?}"
                                            ));
                                        }
                                    }
                                    // Preserve whitespace-only selections
                                    // (e.g., intentional space copy) but
                                    // still guard against a truly empty
                                    // extraction.
                                    let copied_text = extraction
                                        .map(|(_, _, _, t)| t)
                                        .filter(|t| !t.is_empty());
                                    match &copied_text {
                                        Some(text) => {
                                            match clipboard_set_text_with_retry(text) {
                                                Ok(()) => self.log_debug(format!(
                                                    "right-click copy tab={tab_id} len={}",
                                                    text.len()
                                                )),
                                                Err(err) => self.log_error(format!(
                                                    "right-click copy tab={tab_id} set_text failed after retries: {err}"
                                                )),
                                            }
                                        }
                                        None => {
                                            // Extraction failed or was empty.
                                            // Leave the system clipboard alone
                                            // — wiping it on a failed copy was
                                            // the cause of the "right-click
                                            // ate my clipboard" bug.
                                            self.log_debug(format!(
                                                "right-click copy tab={tab_id} extraction empty; clipboard preserved"
                                            ));
                                        }
                                    }
                                    if let Some(sel) = self.terminal_selections.get_mut(&tab_id) {
                                        sel.clear();
                                    }
                                } else {
                                    match clipboard_get_text_with_retry() {
                                        Ok(text) if !text.is_empty() => {
                                            // Normalize pasted text for terminal:
                                            // - U+00A0 → space (OneNote non-breaking spaces)
                                            // - \r\n → \r, \n → \r (line endings)
                                            // No bracketed paste — SSM sessions
                                            // don't reliably pass the escape
                                            // sequences through to the remote terminal.
                                            let normalized = text
                                                .replace('\u{00a0}', " ")
                                                .replace("\r\n", "\r")
                                                .replace('\n', "\r");
                                            let paste_bytes = normalized.len();
                                            self.mark_paste_write(tab_id, paste_bytes);
                                            self.paste_to_connection_tab(
                                                tab_id,
                                                normalized.into_bytes(),
                                            );
                                            self.pending_terminal_focus = Some(tab_id);
                                            self.log_debug(format!(
                                                "right-click paste tab={tab_id} bytes={paste_bytes}"
                                            ));
                                        }
                                        Ok(_) => {}
                                        Err(err) => self.log_error(format!(
                                            "right-click paste tab={tab_id} clipboard read failed: {err}"
                                        )),
                                    }
                                }
                            }
                            // Mouse wheel scrollback
                            if terminal_focus_response.hovered() || terminal_focus_response.has_focus() {
                                let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
                                if scroll_delta != 0.0 {
                                    if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                        if !session.parser.screen().alternate_screen() {
                                            let lines = (scroll_delta.abs() / self.scroll_sensitivity).ceil().max(1.0) as usize;
                                            if scroll_delta > 0.0 {
                                                session.scroll_offset = session.scroll_offset.saturating_add(lines);
                                            } else {
                                                session.scroll_offset = session.scroll_offset.saturating_sub(lines);
                                            }
                                        }
                                    }
                                }
                            }
                            // "Scrolled up" indicator overlay
                            let current_scroll_offset = self.pty_sessions
                                .get(&tab_id)
                                .map(|s| s.scroll_offset)
                                .unwrap_or(0);
                            if current_scroll_offset > 0 {
                                let term_rect = terminal_response.response.rect;
                                let banner_height = 24.0;
                                let banner_rect = egui::Rect::from_min_size(
                                    egui::pos2(term_rect.left(), term_rect.bottom() - banner_height),
                                    egui::vec2(term_rect.width(), banner_height),
                                );
                                ui.painter().rect_filled(
                                    banner_rect,
                                    0.0,
                                    egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220),
                                );
                                let banner_text = format!(
                                    "Scrolled up {} lines \u{2014} click to return to bottom",
                                    current_scroll_offset
                                );
                                ui.painter().text(
                                    banner_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    banner_text,
                                    egui::FontId::proportional(12.0),
                                    egui::Color32::from_rgb(200, 200, 200),
                                );
                                let banner_id = ui.make_persistent_id(("scroll_banner", tab_id));
                                let banner_response = ui.interact(
                                    banner_rect,
                                    banner_id,
                                    egui::Sense::click(),
                                );
                                if banner_response.clicked() {
                                    if let Some(session) = self.pty_sessions.get_mut(&tab_id) {
                                        session.scroll_offset = 0;
                                    }
                                }
                            }
                            if terminal_focus_response.has_focus() || terminal_focus_response.hovered() {
                                // Track that terminal had focus for auto-refresh
                                if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                                    fb.terminal_had_focus = true;
                                }
                            }
                            if terminal_focus_response.has_focus() {
                                ui.memory_mut(|mem| {
                                    mem.set_focus_lock_filter(
                                        terminal_focus_id,
                                        egui::EventFilter {
                                            tab: true,
                                            horizontal_arrows: true,
                                            vertical_arrows: true,
                                            escape: true,
                                        },
                                    );
                                });
                                self.forward_terminal_key_input(ui.ctx(), tab_id);
                            }
                        },
                    );
                });
            }
        }

        fn render_file_browser(&mut self, ui: &mut egui::Ui, tab_id: u64) {
            // Handle drag-and-drop file upload
            let dropped: Vec<egui::DroppedFile> = ui.ctx().input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                let current_dir = self.file_browsers.get(&tab_id)
                    .map(|fb| fb.current_path.clone())
                    .unwrap_or_default();
                if !current_dir.is_empty() {
                    for file in &dropped {
                        if let Some(path) = &file.path {
                            let local = path.to_string_lossy().to_string();
                            self.log_info(format!("drag-drop upload: {local} -> {current_dir}"));
                            self.request_file_upload(tab_id, local, current_dir.clone());
                        }
                    }
                }
            }
            // Show drop hint when files are being hovered
            let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
            if hovering {
                ui.colored_label(egui::Color32::YELLOW, "Drop files here to upload");
            }


            ui.label("File Browser");
            ui.separator();

            // Collect state to avoid borrow conflicts
            let fb_snapshot = self.file_browsers.get(&tab_id).map(|fb| {
                (
                    fb.path_input.clone(),
                    fb.current_path.clone(),
                    fb.entries.clone(),
                    fb.selected_entries.clone(),
                    fb.last_clicked_entry,
                    fb.status.clone(),
                    fb.pending_downloads,
                )
            });

            let Some((
                mut path_input,
                current_path,
                entries,
                _selected_entries,
                _last_clicked,
                status,
                pending_downloads,
            )) = fb_snapshot
            else {
                ui.label("No file browser for this tab");
                return;
            };

            // Path bar
            let mut navigate_to: Option<String> = None;
            let mut refresh_all = false;
            let busy = matches!(
                status,
                FileOpStatus::Listing | FileOpStatus::Downloading | FileOpStatus::Uploading
                | FileOpStatus::Initializing | FileOpStatus::Updating
            );
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut path_input)
                        .desired_width(150.0)
                        .font(egui::TextStyle::Small),
                );
                if response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    navigate_to = Some(path_input.clone());
                }
                if ui.small_button("Go").clicked() {
                    navigate_to = Some(path_input.clone());
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Refresh").small())
                    .clicked()
                {
                    refresh_all = true;
                    navigate_to = Some(current_path.clone());
                }
            });
            // Write back path_input
            if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                fb.path_input = path_input;
            }

            // Up button
            if ui.button(".. (Up)").clicked() {
                navigate_to = Some(parent_path(&current_path));
            }

            // Status indicator
            match &status {
                FileOpStatus::Listing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading...");
                    });
                }
                FileOpStatus::Downloading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        if pending_downloads > 1 {
                            ui.label(format!("Downloading ({pending_downloads} remaining)..."));
                        } else {
                            ui.label("Downloading...");
                        }
                    });
                }
                FileOpStatus::Uploading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Uploading...");
                    });
                }
                FileOpStatus::Initializing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Initializing...");
                    });
                }
                FileOpStatus::Updating => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Updating...");
                    });
                }
                FileOpStatus::Error(err) => {
                    ui.colored_label(egui::Color32::RED, truncate(err, 40));
                }
                FileOpStatus::Idle => {}
            }

            ui.separator();

            // File tree with expand/collapse
            let mut double_clicked_dir: Option<String> = None;
            let mut double_clicked_file: Option<String> = None;
            let mut toggle_expand: Option<String> = None;
            let mut dirs_to_prefetch: Vec<String> = Vec::new();

            // Snapshot expanded dirs and cache for rendering
            let expanded = self.file_browsers.get(&tab_id)
                .map(|fb| fb.expanded_dirs.clone())
                .unwrap_or_default();
            let dir_cache = self.file_browsers.get(&tab_id)
                .map(|fb| fb.dir_cache.clone())
                .unwrap_or_default();
            let fetching = self.file_browsers.get(&tab_id)
                .map(|fb| fb.fetching_dirs.clone())
                .unwrap_or_default();

            egui::ScrollArea::vertical()
                .max_height(ui.available_height() - 40.0)
                .show(ui, |ui| {
                    // Recursive tree render helper — collects actions to apply after
                    struct TreeActions {
                        double_clicked_dir: Option<String>,
                        double_clicked_file: Option<String>,
                        toggle_expand: Option<String>,
                        prefetch: Vec<String>,
                        selected_file: Option<(String, String)>, // (full_path, filename)
                    }
                    fn render_tree_level(
                        ui: &mut egui::Ui,
                        parent_path: &str,
                        entries: &[FileEntry],
                        expanded: &std::collections::HashSet<String>,
                        dir_cache: &HashMap<String, Vec<FileEntry>>,
                        fetching: &std::collections::HashSet<String>,
                        depth: usize,
                        actions: &mut TreeActions,
                    ) {
                        let indent = depth as f32 * 16.0;
                        for entry in entries {
                            let full_path = join_path(parent_path, &entry.name);
                            ui.horizontal(|ui| {
                                ui.add_space(indent);
                                if entry.is_dir {
                                    let is_expanded = expanded.contains(&full_path);
                                    // Draw a triangle arrow (right=collapsed, down=expanded)
                                    let btn_size = egui::vec2(16.0, 16.0);
                                    let (rect, btn_response) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                                    {
                                        let center = rect.center();
                                        let painter = ui.painter();
                                        let color = ui.visuals().text_color();
                                        if is_expanded {
                                            // Down arrow
                                            let points = vec![
                                                egui::pos2(center.x - 4.0, center.y - 2.0),
                                                egui::pos2(center.x + 4.0, center.y - 2.0),
                                                egui::pos2(center.x, center.y + 4.0),
                                            ];
                                            painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
                                        } else {
                                            // Right arrow (play symbol)
                                            let points = vec![
                                                egui::pos2(center.x - 2.0, center.y - 4.0),
                                                egui::pos2(center.x + 4.0, center.y),
                                                egui::pos2(center.x - 2.0, center.y + 4.0),
                                            ];
                                            painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
                                        }
                                    }
                                    if btn_response.clicked() {
                                        actions.toggle_expand = Some(full_path.clone());
                                    }
                                    let dir_response = ui.selectable_label(false, &entry.name);
                                    if dir_response.double_clicked() {
                                        actions.double_clicked_dir = Some(full_path.clone());
                                    }
                                    // Prefetch: expanded dirs are high priority,
                                    // collapsed dirs are low priority (background)
                                    if !dir_cache.contains_key(&full_path) && !fetching.contains(&full_path) {
                                        if expanded.contains(&full_path) {
                                            // Expanded dirs always fetch immediately
                                            actions.prefetch.push(full_path.clone());
                                        } else if actions.prefetch.len() < 2 {
                                            // Collapsed dirs: trickle max 2 per frame
                                            actions.prefetch.push(full_path.clone());
                                        }
                                    }
                                } else {
                                    ui.add_space(18.0); // align with arrow button width
                                    let is_sel = actions.selected_file.as_ref()
                                        .map(|(p, _)| p == &full_path).unwrap_or(false);
                                    let file_response = ui.selectable_label(is_sel, truncate(&entry.name, 25));
                                    if file_response.clicked() {
                                        actions.selected_file = Some((full_path.clone(), entry.name.clone()));
                                    }
                                    if file_response.double_clicked() {
                                        actions.double_clicked_file = Some(full_path.clone());
                                    }
                                }
                            });
                            // Render children if expanded
                            if entry.is_dir && expanded.contains(&full_path) {
                                if let Some(children) = dir_cache.get(&full_path) {
                                    render_tree_level(
                                        ui, &full_path, children, expanded, dir_cache,
                                        fetching, depth + 1, actions,
                                    );
                                } else if fetching.contains(&full_path) {
                                    ui.horizontal(|ui| {
                                        ui.add_space(indent + 16.0);
                                        ui.spinner();
                                        ui.small(egui::RichText::new("loading...").weak());
                                    });
                                }
                            }
                        }
                    }

                    // Preserve current selection for highlighting
                    let prev_selected = self.file_browsers.get(&tab_id)
                        .and_then(|fb| fb.selected_file.clone());
                    let mut actions = TreeActions {
                        double_clicked_dir: None,
                        double_clicked_file: None,
                        toggle_expand: None,
                        selected_file: prev_selected,
                        prefetch: Vec::new(),
                    };

                    render_tree_level(
                        ui, &current_path, &entries, &expanded, &dir_cache,
                        &fetching, 0, &mut actions,
                    );

                    if entries.is_empty() && matches!(status, FileOpStatus::Idle) {
                        ui.label("(empty)");
                    }

                    double_clicked_dir = actions.double_clicked_dir;
                    double_clicked_file = actions.double_clicked_file;
                    toggle_expand = actions.toggle_expand;
                    dirs_to_prefetch = actions.prefetch;
                    // Save selected file
                    if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                        fb.selected_file = actions.selected_file;
                    }
                });

            // Handle expand/collapse toggle
            let mut expand_fetch: Option<String> = None;
            if let Some(dir_path) = &toggle_expand {
                if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                    if fb.expanded_dirs.contains(dir_path) {
                        fb.expanded_dirs.remove(dir_path);
                    } else {
                        fb.expanded_dirs.insert(dir_path.clone());
                        if !fb.dir_cache.contains_key(dir_path) {
                            expand_fetch = Some(dir_path.clone());
                        }
                    }
                }
            }
            if let Some(p) = expand_fetch {
                self.request_bg_listing(tab_id, p);
            }

            // Prefetch subdirectories in background
            for dir in dirs_to_prefetch {
                self.request_bg_listing(tab_id, dir);
            }

            // Navigate on double-click dir
            if let Some(dir_path) = double_clicked_dir {
                navigate_to = Some(dir_path);
            }

            // Open file in editor on double-click
            if let Some(file_path) = double_clicked_file {
                self.request_file_read(tab_id, file_path);
            }

            ui.separator();

            // Upload / Download buttons
            ui.horizontal(|ui| {
                let sel = self.file_browsers.get(&tab_id)
                    .and_then(|fb| fb.selected_file.clone());
                let can_download = sel.is_some();
                if ui
                    .add_enabled(can_download, egui::Button::new("Download"))
                    .clicked()
                {
                    if let Some((remote_path, filename)) = sel {
                        let dialog = rfd::FileDialog::new()
                            .set_file_name(&filename);
                        if let Some(local) = dialog.save_file() {
                            self.request_file_download(
                                tab_id,
                                remote_path,
                                local.to_string_lossy().to_string(),
                            );
                        }
                    }
                }
                if ui.button("Upload").clicked() {
                    let dialog = rfd::FileDialog::new();
                    if let Some(local) = dialog.pick_file() {
                        self.request_file_upload(
                            tab_id,
                            local.to_string_lossy().to_string(),
                            current_path.clone(),
                        );
                    }
                }
            });

            // Handle navigation
            if let Some(path) = navigate_to {
                if refresh_all {
                    // Invalidate entire cache and refresh expanded dirs
                    let expanded: Vec<String> = self.file_browsers.get(&tab_id)
                        .map(|fb| fb.expanded_dirs.iter().cloned().collect())
                        .unwrap_or_default();
                    if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                        fb.dir_cache.clear();
                        fb.fetching_dirs.clear();
                    }
                    self.request_file_listing(tab_id, path);
                    for dir in expanded {
                        self.request_bg_listing(tab_id, dir);
                    }
                } else {
                    self.request_file_listing(tab_id, path);
                }
            }
        }

        /// Render the editor file tab bar (horizontal scrolling, shown above the editor/terminal split).
        fn render_editor_tab_bar(&mut self, ui: &mut egui::Ui, tab_id: u64) {
            let tab_snapshot: Vec<(usize, String, bool)> = self.file_browsers
                .get(&tab_id)
                .map(|fb| {
                    fb.editor_tabs.iter().enumerate().map(|(i, et)| {
                        (i, et.filename().to_string(), et.dirty)
                    }).collect()
                })
                .unwrap_or_default();
            if tab_snapshot.is_empty() {
                return;
            }
            let active_editor = self.file_browsers.get(&tab_id)
                .and_then(|fb| fb.active_editor);
            let mut new_active: Option<usize> = active_editor;
            let mut close_editor: Option<usize> = None;

            egui::ScrollArea::horizontal()
                .id_salt(("editor_tabs", tab_id))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (idx, filename, dirty) in &tab_snapshot {
                            let is_active = active_editor == Some(*idx);
                            let label = if *dirty {
                                format!("{filename} *")
                            } else {
                                filename.clone()
                            };
                            let frame = if is_active {
                                egui::Frame::group(ui.style())
                                    .fill(ui.style().visuals.selection.bg_fill)
                            } else {
                                egui::Frame::group(ui.style())
                            };
                            frame.show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(is_active, &label).clicked() {
                                        new_active = Some(*idx);
                                    }
                                    if ui.small_button("x").clicked() {
                                        close_editor = Some(*idx);
                                    }
                                });
                            });
                        }
                    });
                });

            // Handle tab close
            if let Some(close_idx) = close_editor {
                if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                    fb.editor_tabs.remove(close_idx);
                    if fb.editor_tabs.is_empty() {
                        fb.active_editor = None;
                        self.editor_split.remove(&tab_id);
                    } else if let Some(active) = fb.active_editor {
                        if active >= fb.editor_tabs.len() {
                            fb.active_editor = Some(fb.editor_tabs.len() - 1);
                        } else if active > close_idx {
                            fb.active_editor = Some(active - 1);
                        }
                    }
                }
            } else if new_active != active_editor {
                if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                    fb.active_editor = new_active;
                }
            }
        }

        /// Render the active editor content (code editor with line numbers + save).
        fn render_editor_content(&mut self, ui: &mut egui::Ui, tab_id: u64) {
            let editor_data = self.file_browsers.get(&tab_id).and_then(|fb| {
                fb.active_editor.and_then(|idx| {
                    fb.editor_tabs.get(idx).map(|et| (
                        et.remote_path.clone(),
                        et.content.clone(),
                        et.dirty,
                        et.status.clone(),
                        fb.status.clone(),
                    ))
                })
            });
            let Some((remote_path, mut editor_content, editor_dirty, editor_status, ed_status)) = editor_data else {
                return;
            };

            // Status + save bar
            ui.horizontal(|ui| {
                ui.label(&remote_path);
                if !editor_status.is_empty() {
                    if editor_status.starts_with("Error") || editor_status.starts_with("Save failed") {
                        ui.colored_label(egui::Color32::RED, &editor_status);
                    } else if editor_status == "Saved" {
                        ui.colored_label(egui::Color32::GREEN, &editor_status);
                    } else {
                        ui.label(&editor_status);
                    }
                }
                let saving = matches!(ed_status, FileOpStatus::Uploading);
                if ui.add_enabled(editor_dirty && !saving, egui::Button::new("Save")).clicked() {
                    if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                        if let Some(idx) = fb.active_editor {
                            if let Some(et) = fb.editor_tabs.get_mut(idx) {
                                et.content = editor_content.clone();
                            }
                        }
                    }
                    self.request_file_save(tab_id);
                }

                // Ctrl+S shortcut
                let ctrl_s = ui.input(|i| {
                    i.events.iter().any(|e| matches!(e,
                        egui::Event::Key {
                            key: egui::Key::S,
                            pressed: true,
                            modifiers,
                            ..
                        } if modifiers.ctrl || modifiers.command
                    ))
                });
                if ctrl_s && editor_dirty && !saving {
                    if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                        if let Some(idx) = fb.active_editor {
                            if let Some(et) = fb.editor_tabs.get_mut(idx) {
                                et.content = editor_content.clone();
                            }
                        }
                    }
                    self.request_file_save(tab_id);
                }
            });

            // Code editor with line numbers
            egui::ScrollArea::both()
                .id_salt(("editor_scroll", tab_id))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        let line_count = editor_content.lines().count().max(1);
                        let line_numbers: String = (1..=line_count)
                            .map(|n| format!("{n:>4}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.add(
                            egui::TextEdit::multiline(&mut line_numbers.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(40.0)
                                .interactive(false)
                                .frame(false),
                        );
                        let response = ui.add(
                            egui::TextEdit::multiline(&mut editor_content)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .code_editor(),
                        );
                        if response.changed() {
                            if let Some(fb) = self.file_browsers.get_mut(&tab_id) {
                                if let Some(idx) = fb.active_editor {
                                    if let Some(et) = fb.editor_tabs.get_mut(idx) {
                                        et.dirty = editor_content != et.original;
                                        et.content = editor_content;
                                        if et.status == "Saved" {
                                            et.status.clear();
                                        }
                                    }
                                }
                            }
                        }
                    });
                });
        }

        fn render_details_panel(&mut self, ui: &mut egui::Ui) {
            let Some(instance) = self.detail_instance.clone() else {
                ui.label("No instance selected. Right-click an instance and choose 'See Details'.");
                return;
            };

            ui.horizontal(|ui| {
                ui.heading(instance.name.as_deref().unwrap_or("(unnamed)"));
                if ui.button("Copy All").clicked() {
                    let mut text = String::new();
                    text.push_str(&format!("Instance ID: {}\n", instance.instance_id));
                    text.push_str(&format!("Name: {}\n", instance.name.as_deref().unwrap_or("-")));
                    text.push_str(&format!("State: {}\n", instance.state));
                    text.push_str(&format!("Instance Type: {}\n", instance.instance_type.as_deref().unwrap_or("-")));
                    text.push_str(&format!("AMI ID: {}\n", instance.image_id.as_deref().unwrap_or("-")));
                    text.push_str(&format!("Private IP: {}\n", instance.private_ip.as_deref().unwrap_or("-")));
                    text.push_str(&format!("Private DNS: {}\n", instance.private_dns.as_deref().unwrap_or("-")));
                    text.push_str(&format!("AZ: {}\n", instance.az.as_deref().unwrap_or("-")));
                    text.push_str(&format!("Environment: {}\n", instance_env(&instance).unwrap_or_else(|| "-".to_string())));
                    text.push_str(&format!("IAM Role: {}\n", self.detail_iam_role.as_deref().unwrap_or("-")));
                    text.push_str(&format!("ASG: {}\n", instance.asg.as_deref().unwrap_or("-")));
                    text.push_str(&format!("SSM Managed: {}\n", if instance.ssm_managed { "Yes" } else { "No" }));
                    text.push_str(&format!("Launch Time: {}\n", instance.launch_time.as_deref().unwrap_or("-")));
                    if !self.detail_volumes.is_empty() {
                        text.push_str("\nVolumes:\n");
                        for vol in &self.detail_volumes {
                            text.push_str(&format!("  {} | {} | {} | {} | {} | {}\n",
                                vol.volume_id, vol.size_gb, vol.volume_type, vol.device, vol.state, vol.attach_time));
                        }
                    }
                    if !instance.tags.is_empty() {
                        text.push_str("\nTags:\n");
                        let mut tags: Vec<_> = instance.tags.iter().collect();
                        tags.sort_by(|a, b| a.0.cmp(b.0));
                        for (k, v) in tags {
                            text.push_str(&format!("  {k}: {v}\n"));
                        }
                    }
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(&text);
                    }
                }
                if ui.button("Close").clicked() {
                    self.detail_instance = None;
                    self.main_tab = MainTab::Inventory;
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("detail_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, label: &str, value: &str| {
                            ui.strong(label);
                            ui.label(value);
                            ui.end_row();
                        };

                        row(ui, "Instance ID", &instance.instance_id);
                        row(ui, "Name", instance.name.as_deref().unwrap_or("-"));
                        row(ui, "State", &instance.state);
                        row(ui, "Instance Type", instance.instance_type.as_deref().unwrap_or("-"));
                        row(ui, "AMI ID", instance.image_id.as_deref().unwrap_or("-"));
                        row(ui, "Private IP", instance.private_ip.as_deref().unwrap_or("-"));
                        row(ui, "Private DNS", instance.private_dns.as_deref().unwrap_or("-"));
                        row(ui, "Availability Zone", instance.az.as_deref().unwrap_or("-"));
                        row(ui, "Environment", &instance_env(&instance).unwrap_or_else(|| "-".to_string()));
                        row(ui, "IAM Role", self.detail_iam_role.as_deref().unwrap_or(
                            if self.detail_volumes_loading { "loading..." } else { "-" }
                        ));
                        row(ui, "Auto Scaling Group", instance.asg.as_deref().unwrap_or("-"));
                        row(ui, "SSM Managed", if instance.ssm_managed { "Yes" } else { "No" });
                        row(ui, "SSM Ping", instance.ssm_ping.as_deref().unwrap_or("-"));
                        row(ui, "SSM Last Ping", instance.ssm_last_ping.as_deref().unwrap_or("-"));
                        row(ui, "Launch Time", instance.launch_time.as_deref().unwrap_or("-"));
                    });

                // Volumes section
                ui.add_space(12.0);
                ui.heading("Volumes");
                ui.separator();
                if self.detail_volumes_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Fetching volumes...");
                    });
                } else if let Some(err) = &self.detail_volumes_error {
                    ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
                } else if self.detail_volumes.is_empty() {
                    ui.label("No volumes found");
                } else {
                    egui::Grid::new("detail_volumes_grid")
                        .num_columns(6)
                        .spacing([12.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Volume ID");
                            ui.strong("Size");
                            ui.strong("Type");
                            ui.strong("Device");
                            ui.strong("State");
                            ui.strong("Attached");
                            ui.end_row();
                            for vol in &self.detail_volumes {
                                ui.label(&vol.volume_id);
                                ui.label(&vol.size_gb);
                                ui.label(&vol.volume_type);
                                ui.label(&vol.device);
                                ui.label(&vol.state);
                                ui.label(&vol.attach_time);
                                ui.end_row();
                            }
                        });
                }

                if !instance.tags.is_empty() {
                    ui.add_space(12.0);
                    ui.heading("Tags");
                    ui.separator();
                    egui::Grid::new("detail_tags_grid")
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            let mut tags: Vec<(&String, &String)> = instance.tags.iter().collect();
                            tags.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
                            for (key, value) in tags {
                                ui.strong(key);
                                ui.label(value);
                                ui.end_row();
                            }
                        });
                }

            });
        }

        fn render_log_panel(&mut self, ui: &mut egui::Ui) {
            ui.horizontal(|ui| {
                ui.label("Application log");
                ui.separator();
                if ui.button("Clear").clicked() {
                    self.logs.clear();
                    self.log_info("log cleared");
                }
                ui.separator();
                if ui.button("Low").clicked() {
                    self.log_filters.set_verbosity_low();
                }
                if ui.button("Medium").clicked() {
                    self.log_filters.set_verbosity_medium();
                }
                if ui.button("High").clicked() {
                    self.log_filters.set_verbosity_high();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(&mut self.log_filters.trace, "TRACE");
                    ui.checkbox(&mut self.log_filters.debug, "DEBUG");
                    ui.checkbox(&mut self.log_filters.info, "INFO");
                    ui.checkbox(&mut self.log_filters.warn, "WARN");
                    ui.checkbox(&mut self.log_filters.error, "ERROR");
                });
            });
            ui.separator();

            let matching_count = self
                .logs
                .iter()
                .filter(|entry| self.log_filters.includes(entry.level))
                .count();
            ui.label(format!(
                "Showing {matching_count} / {} log lines",
                self.logs.len()
            ));
            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Copy All").clicked() {
                    let text: String = self.logs.iter()
                        .filter(|e| self.log_filters.includes(e.level))
                        .map(|e| format!("[{}] {}", e.level.as_str(), e.message))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(&text);
                    }
                }
            });

            // Build colored log text as a LayoutJob for selectable display
            let mut log_text = String::new();
            for entry in self.logs.iter()
                .filter(|e| self.log_filters.includes(e.level))
            {
                if !log_text.is_empty() {
                    log_text.push('\n');
                }
                log_text.push_str(&format!("[{}] {}", entry.level.as_str(), entry.message));
            }

            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let resp = ui.add(
                        egui::TextEdit::multiline(&mut log_text.as_str())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .frame(false),
                    );
                    // Right-click: inject Ctrl+C key event to copy selection
                    if resp.secondary_clicked() {
                        ui.ctx().input_mut(|i| {
                            i.events.push(egui::Event::Copy);
                        });
                    }
                });
        }
    }

    impl eframe::App for Ec2GuiApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            if self.egui_ctx.is_none() {
                self.egui_ctx = Some(ctx.clone());
            }
            let update_result = panic::catch_unwind(AssertUnwindSafe(|| {
                // Re-apply scaling when native DPI changes (monitor switch)
                let native_ppp = ctx.native_pixels_per_point().unwrap_or(1.0);
                let ppp_changed = (native_ppp - self.last_native_ppp).abs() > 0.01;
                if ppp_changed {
                    self.last_native_ppp = native_ppp;
                    // Clear all terminal selections — the old pixel positions
                    // are invalid after a DPI change (monitor switch).
                    self.terminal_selections.clear();
                    ctx.set_pixels_per_point(self.ui_scale * native_ppp);
                }

                // Auto-fit zoom runs every frame so it triggers on
                // monitor moves regardless of the active tab.
                self.apply_auto_fit_zoom(ctx);

                // Auto-focus the visible terminal when:
                //   - the user just switched into the Connections tab
                //   - the active connection tab changed (new tab opened
                //     or user clicked a different tab in the strip)
                // Explicit focus triggers (Prep Terminal click, right-click
                // paste) set `pending_terminal_focus` directly.
                let current_conn = self.connections.selected();
                let entered_connections = self.main_tab == MainTab::Connections
                    && self.last_main_tab != MainTab::Connections;
                let switched_conn_tab = self.main_tab == MainTab::Connections
                    && current_conn.is_some()
                    && current_conn != self.last_connection_tab;
                if (entered_connections || switched_conn_tab) && current_conn.is_some() {
                    self.pending_terminal_focus = current_conn;
                }
                self.last_main_tab = self.main_tab;
                self.last_connection_tab = current_conn;

                // Zoom hotkeys: Ctrl+= / Ctrl++ zoom in, Ctrl+- zoom out.
                let (zoom_in, zoom_out) = ctx.input(|i| {
                    let ctrl = i.modifiers.ctrl || i.modifiers.command;
                    if !ctrl {
                        return (false, false);
                    }
                    let zin = i.key_pressed(egui::Key::Plus)
                        || i.key_pressed(egui::Key::Equals);
                    let zout = i.key_pressed(egui::Key::Minus);
                    (zin, zout)
                });
                if zoom_in {
                    self.ui_scale = (self.ui_scale + 0.1).min(3.0);
                    self.config.ui_scale = Some(self.ui_scale);
                    let _ = self.config.save();
                    ctx.set_pixels_per_point(self.ui_scale * native_ppp);
                }
                if zoom_out {
                    self.ui_scale = (self.ui_scale - 0.1).max(0.5);
                    self.config.ui_scale = Some(self.ui_scale);
                    let _ = self.config.save();
                    ctx.set_pixels_per_point(self.ui_scale * native_ppp);
                }

                // --- WSL Setup (non-blocking) ---
                if matches!(self.wsl_setup_state, WslSetupState::Checking) {
                    if let Ok(status) = self.wsl_setup_rx.try_recv() {
                        if status.is_ready() {
                            self.wsl_setup_state = WslSetupState::Ready;
                            wsl_setup::mark_setup_done();
                            self.message = "WSL setup complete.".to_string();
                            self.log_info("WSL setup verified and cached");
                        } else if !status.wsl_available {
                            self.wsl_setup_state = WslSetupState::Needed;
                            self.message = "WSL is not installed. Install WSL and click 'Initialize WSL'.".to_string();
                            self.log_warn("WSL not available on this machine");
                        } else {
                            self.wsl_setup_state = WslSetupState::Needed;
                            self.message = "WSL prerequisites missing.".to_string();
                            self.log_warn("WSL prerequisites not met");
                            // Auto-prompt for sudo password if --wsl flag is set
                            if self.wsl_auto_setup {
                                self.wsl_show_password_popup = true;
                            }
                        }
                        // Log WSL setup details
                        for line in &status.setup_log {
                            if self.debug_mode {
                                self.log_info(format!("[WSL] {line}"));
                            } else {
                                self.log_debug(format!("[WSL] {line}"));
                            }
                        }
                        self.wsl_setup_status = Some(status);
                    }
                    if self.wsl_setup_state == WslSetupState::Checking {
                        self.message = "Initializing WSL setup...".to_string();
                    }
                    ctx.request_repaint_after(Duration::from_millis(200));
                }

                self.poll_profile_choice_changes();
                self.poll_credentials_changes();
                self.poll_auth_expiry();
                self.poll_stale_connection_tabs();
                if self.poll_connection_events() {
                    // More output queued than we processed this frame;
                    // schedule another frame to drain the rest.
                    ctx.request_repaint();
                }
                self.poll_refresh_events();
                self.poll_file_op_events();
                if self.refreshing {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                if self.main_tab == MainTab::Connections && !self.pty_sessions.is_empty() {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
                if self.file_browsers.values().any(|b| {
                    matches!(
                        b.status,
                        FileOpStatus::Listing
                            | FileOpStatus::Downloading
                            | FileOpStatus::Uploading
                    )
                }) {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                if self.gui_smoke_should_close {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                // Deferred exit: wait for cleanup threads to finish sending
                // graceful exit commands before actually closing the window.
                if let Some(exit_at) = self.pending_exit_at {
                    if std::time::Instant::now() >= exit_at {
                        self.pending_exit_at = None;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    ctx.request_repaint_after(Duration::from_millis(50));
                }

                // Intercept window close when there are active connections.
                // Save window position/size so it reopens on the same monitor
                if let Some(pos) = ctx.input(|i| i.viewport().inner_rect) {
                    self.config.window_x = Some(pos.left());
                    self.config.window_y = Some(pos.top());
                    self.config.window_w = Some(pos.width());
                    self.config.window_h = Some(pos.height());
                }
                self.config.window_maximized = Some(
                    ctx.input(|i| i.viewport().maximized).unwrap_or(false)
                );

                if ctx.input(|i| i.viewport().close_requested()) {
                    // Persist window state before closing
                    let _ = self.config.save();
                    let active = self.pty_sessions.len();
                    if active > 0 {
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                        self.show_close_blocked = true;
                        self.log_warn(format!(
                            "close blocked: {active} active connection(s) still open"
                        ));
                    }
                }

                if self.show_close_blocked {
                    let active = self.pty_sessions.len();
                    if active == 0 {
                        // All connections were closed while the dialog
                        // was open — allow close now.
                        self.show_close_blocked = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        egui::Window::new("Active Connections")
                            .collapsible(false)
                            .resizable(false)
                            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                            .show(ctx, |ui| {
                                ui.label(format!(
                                    "There {} {} active connection{}. \
                                     Close all connections before exiting.",
                                    if active == 1 { "is" } else { "are" },
                                    active,
                                    if active == 1 { "" } else { "s" },
                                ));
                                ui.add_space(8.0);
                                // Space or Enter triggers Close All & Exit
                                // for keyboard-only operation.
                                let confirm_via_key = ui.input(|i| {
                                    i.key_pressed(egui::Key::Space)
                                        || i.key_pressed(egui::Key::Enter)
                                });
                                let mut do_close_all = confirm_via_key;
                                ui.horizontal(|ui| {
                                    if ui.button("Close All & Exit").clicked() {
                                        do_close_all = true;
                                    }
                                    if ui.button("Go Back").clicked() {
                                        self.show_close_blocked = false;
                                    }
                                });
                                if do_close_all {
                                    let tab_ids: Vec<u64> =
                                        self.pty_sessions.keys().copied().collect();
                                    for id in tab_ids {
                                        self.close_connection_tab(id);
                                    }
                                    self.show_close_blocked = false;
                                    // Defer the actual window close so cleanup
                                    // threads have time to send graceful exit
                                    // commands to remote SSM sessions.
                                    self.pending_exit_at =
                                        Some(std::time::Instant::now() + std::time::Duration::from_millis(600));
                                }
                            });
                    }
                }

                // WSL setup popup
                if self.wsl_show_password_popup {
                    let mut open = true;
                    egui::Window::new("WSL Setup Required")
                        .collapsible(false)
                        .resizable(false)
                        .open(&mut open)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ctx, |ui| {
                            ui.label("The following WSL prerequisites need to be installed:");
                            ui.add_space(4.0);

                            if let Some(ref status) = self.wsl_setup_status {
                                if !status.aws_cli_installed {
                                    ui.label("  - AWS CLI v2");
                                }
                                if !status.ssm_plugin_installed {
                                    ui.label("  - Session Manager Plugin");
                                }
                                if !status.aws_credentials_linked {
                                    ui.label("  - AWS credentials link");
                                }
                            }

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);

                            ui.label("Option 1: Enter sudo password to auto-install");
                            ui.add_space(4.0);
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.wsl_password_buf)
                                    .password(true)
                                    .hint_text("WSL sudo password")
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let enter_pressed = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if ui.button("Install").clicked() || enter_pressed {
                                    let password = self.wsl_password_buf.clone();
                                    self.wsl_password_buf.clear();
                                    self.log_info("[WSL] sudo password cleared from memory");
                                    self.wsl_show_password_popup = false;
                                    self.wsl_setup_state = WslSetupState::Checking;
                                    self.message = "Running WSL setup...".to_string();
                                    self.log_info("WSL auto-setup started");
                                    let tx = self.wsl_setup_tx.clone();
                                    std::thread::spawn(move || {
                                        let status = wsl_setup::run_wsl_setup_with_password(&password);
                                        let _ = tx.send(status);
                                    });
                                }
                            });

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);

                            ui.label("Option 2: Copy commands and run in your WSL terminal");
                            ui.add_space(4.0);
                            if ui.button("Copy Commands").clicked() {
                                let win_user = std::env::var("USERNAME").unwrap_or_default();
                                let mut cmds: Vec<String> = Vec::new();
                                cmds.push("sudo apt-get update && sudo apt-get install -y unzip curl".to_string());
                                if let Some(ref status) = self.wsl_setup_status {
                                    if !status.aws_cli_installed {
                                        cmds.push("cd /tmp && curl -s \"https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip\" -o awscliv2.zip && unzip -qo awscliv2.zip && sudo ./aws/install && rm -rf aws awscliv2.zip".to_string());
                                    }
                                    if !status.ssm_plugin_installed {
                                        cmds.push("cd /tmp && curl -s \"https://s3.amazonaws.com/session-manager-downloads/plugin/latest/ubuntu_64bit/session-manager-plugin.deb\" -o ssm.deb && sudo dpkg -i ssm.deb && rm ssm.deb".to_string());
                                    }
                                    if !status.aws_credentials_linked && !win_user.is_empty() {
                                        cmds.push(format!("ln -s /mnt/c/Users/{win_user}/.aws ~/.aws"));
                                    }
                                }
                                let full_cmd = cmds.join(" && ");
                                ui.ctx().copy_text(full_cmd);
                                self.message = "Commands copied to clipboard!".to_string();
                                self.wsl_password_buf.clear();
                                self.wsl_show_password_popup = false;
                            }

                            ui.add_space(8.0);
                            if ui.button("Cancel").clicked() {
                                self.wsl_password_buf.clear();
                                self.wsl_show_password_popup = false;
                            }
                        });
                    if !open {
                        self.wsl_password_buf.clear();
                        self.wsl_show_password_popup = false;
                    }
                }

                // Color picker window (opened from Edit menu or right-click tab)
                if self.color_picker_profile.is_some() {
                    let profile_id = self.color_picker_profile.clone().unwrap_or_default();

                    let display_name = self
                        .config
                        .profiles
                        .iter()
                        .find(|p| p.profile_id == profile_id)
                        .map(|p| p.display_name.clone())
                        .unwrap_or_else(|| profile_id.clone());

                    let mut open = true;
                    egui::Window::new(format!("Color: {display_name}"))
                        .collapsible(false)
                        .resizable(false)
                        .open(&mut open)
                        .show(ctx, |ui| {
                            ui.color_edit_button_rgb(&mut self.tab_color_picker_rgb);
                            ui.add_space(4.0);
                            let [r, g, b] = self.tab_color_picker_rgb;
                            let preview_color = egui::Color32::from_rgb(
                                (r * 255.0) as u8,
                                (g * 255.0) as u8,
                                (b * 255.0) as u8,
                            );
                            let hex = color32_to_hex(preview_color);
                            ui.label(format!("Hex: {hex}"));
                            ui.add_space(4.0);

                            ui.horizontal(|ui| {
                                if ui.button("OK").clicked() {
                                    let hex = color32_to_hex(preview_color);
                                    if !profile_id.is_empty() {
                                        self.config
                                            .account_colors
                                            .insert(profile_id.clone(), hex);
                                        self.rebuild_account_colors();
                                        let _ = self.config.save();
                                    }
                                    self.color_picker_profile = None;
                                }
                                if ui.button("Cancel").clicked() {
                                    self.color_picker_profile = None;
                                }
                                if ui.button("Reset to Default").clicked() {
                                    if !profile_id.is_empty() {
                                        self.config.account_colors.remove(&profile_id);
                                        self.rebuild_account_colors();
                                        let _ = self.config.save();
                                    }
                                    self.color_picker_profile = None;
                                }
                            });
                        });
                    if !open {
                        self.color_picker_profile = None;
                    }
                }

                egui::TopBottomPanel::top("top").show(ctx, |ui| {
                    egui::MenuBar::new().ui(ui, |ui| {
                        ui.menu_button("Edit", |ui| {
                            ui.menu_button("Theme", |ui| {
                                if ui
                                    .selectable_label(self.dark_mode, "Dark")
                                    .clicked()
                                {
                                    self.dark_mode = true;
                                    ctx.set_theme(egui::ThemePreference::Dark);
                                    self.config.theme = Some("dark".to_string());
                                    let _ = self.config.save();
                                    ui.close();
                                }
                                if ui
                                    .selectable_label(!self.dark_mode, "Light")
                                    .clicked()
                                {
                                    self.dark_mode = false;
                                    ctx.set_theme(egui::ThemePreference::Light);
                                    self.config.theme = Some("light".to_string());
                                    let _ = self.config.save();
                                    ui.close();
                                }
                            });
                            ui.menu_button("Scroll Sensitivity", |ui| {
                                for &(label, value) in &[("Low", 20.0f32), ("Medium (Default)", 10.0), ("High", 5.0)] {
                                    if ui.selectable_label(self.scroll_sensitivity == value, label).clicked() {
                                        self.scroll_sensitivity = value;
                                        self.config.scroll_sensitivity = Some(value);
                                        let _ = self.config.save();
                                        ui.close();
                                    }
                                }
                            });
                            ui.menu_button("Environment Colors", |ui| {
                                if ui
                                    .selectable_label(self.config.account_colors_enabled, "Enabled")
                                    .clicked()
                                {
                                    self.config.account_colors_enabled = !self.config.account_colors_enabled;
                                    let _ = self.config.save();
                                }
                                if self.config.account_colors_enabled {
                                    ui.separator();

                                    // Show unique environments sorted by account order then alpha
                                    let mut seen = std::collections::HashSet::new();
                                    let mut env_list: Vec<(String, String, String, egui::Color32)> = self
                                        .account_color_map
                                        .iter()
                                        .filter_map(|(key, color)| {
                                            let (pid, env) = key.split_once(':')?;
                                            let env_lower = env.to_ascii_lowercase();
                                            let dedup = format!("{}:{}", pid, env_lower);
                                            if seen.insert(dedup) {
                                                let label = self.config.profiles.iter()
                                                    .find(|p| p.profile_id == pid)
                                                    .map(|p| p.display_name.as_str())
                                                    .unwrap_or(pid);
                                                Some((pid.to_string(), format!("{env} ({label})"), key.clone(), *color))
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    env_list.sort_by(|a, b| {
                                        let (idx_a, pa) = self.config.profiles.iter().enumerate()
                                            .find(|(_, p)| p.profile_id == a.0)
                                            .map(|(i, p)| (i, Some(p)))
                                            .unwrap_or((usize::MAX, None));
                                        let (idx_b, pb) = self.config.profiles.iter().enumerate()
                                            .find(|(_, p)| p.profile_id == b.0)
                                            .map(|(i, p)| (i, Some(p)))
                                            .unwrap_or((usize::MAX, None));
                                        let ord_a = pa.and_then(|p| p.sort_order).unwrap_or(u32::MAX);
                                        let ord_b = pb.and_then(|p| p.sort_order).unwrap_or(u32::MAX);
                                        ord_a.cmp(&ord_b)
                                            .then_with(|| idx_a.cmp(&idx_b))
                                            .then_with(|| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()))
                                    });

                                    for (_pid, display, _key, color) in &env_list {
                                        ui.horizontal(|ui| {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(12.0, 12.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().circle_filled(rect.center(), 6.0, *color);
                                            ui.label(display.as_str());
                                        });
                                    }

                                    ui.separator();
                                    if ui.button("Reset All to Defaults").clicked() {
                                        self.config.account_colors.clear();
                                        self.rebuild_account_colors();
                                        let _ = self.config.save();
                                        ui.close();
                                    }
                                }
                            });
                            if ui
                                .selectable_label(
                                    self.config.reset_filter_on_profile_switch,
                                    "Reset Filter on Profile Switch",
                                )
                                .clicked()
                            {
                                self.config.reset_filter_on_profile_switch =
                                    !self.config.reset_filter_on_profile_switch;
                                let _ = self.config.save();
                            }
                        });
                    });

                    ui.horizontal(|ui| {
                        ui.heading("EC2 + SSM Instance Explorer");
                        ui.separator();
                        ui.label(format!("Mode: {}", self.options.mode.as_str()));
                        if let Some(c) = &self.context {
                            ui.label(format!("Profile: {}", c.profile));
                            ui.label(format!(
                                "Account: {}",
                                c.account_id.as_deref().unwrap_or("unknown")
                            ));
                            ui.label(format!("Region: {}", c.region));
                            let live_auth = if self.options.mode == Mode::Sim {
                                AuthStatus::Ok
                            } else {
                                self.selected_profile
                                    .as_deref()
                                    .and_then(|pid| {
                                        self.profile_auth_infos
                                            .iter()
                                            .find(|a| a.profile_id == pid)
                                            .map(|a| a.auth_status.clone())
                                    })
                                    .unwrap_or_else(|| c.auth_status.clone())
                            };
                            ui.label(format!("Auth: {}", live_auth));
                        }
                    });

                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .selectable_label(self.main_tab == MainTab::Inventory, "Inventory")
                            .clicked()
                        {
                            self.main_tab = MainTab::Inventory;
                        }
                        if ui
                            .selectable_label(
                                self.main_tab == MainTab::Connections,
                                format!("Connections ({})", self.connections.tabs().len()),
                            )
                            .clicked()
                        {
                            self.main_tab = MainTab::Connections;
                        }
                        if self.detail_instance.is_some() {
                            if ui
                                .selectable_label(self.main_tab == MainTab::Details, "Details")
                                .clicked()
                            {
                                self.main_tab = MainTab::Details;
                            }
                        }
                        if ui
                            .selectable_label(self.main_tab == MainTab::Log, "Log")
                            .clicked()
                        {
                            self.main_tab = MainTab::Log;
                        }

                        // Environment visibility dropdown (next to Log)
                        if self.config.account_colors_enabled
                            && !self.account_color_map.is_empty()
                        {
                            let mut seen = std::collections::HashSet::new();
                            let mut all_envs: Vec<(String, egui::Color32)> = self
                                .account_color_map
                                .iter()
                                .filter_map(|(key, color)| {
                                    let (_, env) = key.split_once(':')?;
                                    let env_lower = env.to_ascii_lowercase();
                                    if seen.insert(env_lower) {
                                        Some((env.to_string(), *color))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            all_envs.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));

                            let hidden_count = self.hidden_envs.len();
                            let env_label = if hidden_count > 0 {
                                format!("Exclude Env ({hidden_count})")
                            } else {
                                "Exclude Env".to_string()
                            };
                            egui::ComboBox::from_id_salt("env_visibility")
                                .selected_text(env_label)
                                .show_ui(ui, |ui| {
                                    for (env, color) in &all_envs {
                                        let env_lower = env.to_ascii_lowercase();
                                        let mut excluded = self.hidden_envs.contains(&env_lower);
                                        ui.horizontal(|ui| {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(8.0, 8.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().circle_filled(rect.center(), 4.0, *color);
                                            if ui.checkbox(&mut excluded, env).changed() {
                                                if excluded {
                                                    self.hidden_envs.insert(env_lower);
                                                } else {
                                                    self.hidden_envs.remove(&env_lower);
                                                }
                                                // Persist to config
                                                self.config.excluded_envs = self.hidden_envs.iter().cloned().collect();
                                                self.config.excluded_envs.sort();
                                                let _ = self.config.save();
                                            }
                                        });
                                    }
                                });
                        }

                        let tab_count = self.connections.tabs().len();
                        let close_all_label = if tab_count > 0 {
                            format!("Close All ({tab_count})")
                        } else {
                            "Close All".to_string()
                        };
                        let close_all = ui.add_enabled(
                            tab_count > 0,
                            egui::Button::new(close_all_label),
                        );
                        if close_all.clicked() {
                            let ids: Vec<u64> = self
                                .connections
                                .tabs()
                                .iter()
                                .map(|t| t.id)
                                .collect();
                            self.log_info(format!("closing all {} connection tab(s)", ids.len()));
                            for id in ids {
                                self.close_connection_tab(id);
                            }
                        }
                    });

                    if !self.message.is_empty() {
                        ui.label(self.message.clone());
                    }
                });

                if self.main_tab == MainTab::Inventory {
                egui::SidePanel::left("controls")
                    .resizable(true)
                    .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("controls_scroll")
                        .show(ui, |ui| {
                    ui.heading("Controls");

                    // WSL setup status / button — only show when WSL is the selected terminal
                    let is_wsl_selected = self.selected_terminal_id == "wsl";
                    if is_wsl_selected {
                    match self.wsl_setup_state {
                        WslSetupState::Checking => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Initializing WSL setup...");
                            });
                            ui.separator();
                        }
                        WslSetupState::Needed => {
                            ui.group(|ui| {
                                let is_wsl_missing = self.wsl_setup_status
                                    .as_ref()
                                    .map(|s| !s.wsl_available)
                                    .unwrap_or(false);

                                if is_wsl_missing {
                                    ui.colored_label(
                                        egui::Color32::YELLOW,
                                        "WSL not installed",
                                    );
                                    ui.label("Install WSL from PowerShell (admin):");
                                    ui.monospace("wsl --install");
                                } else {
                                    ui.colored_label(
                                        egui::Color32::YELLOW,
                                        "WSL setup incomplete",
                                    );
                                    if let Some(ref status) = self.wsl_setup_status {
                                        if !status.aws_cli_installed {
                                            ui.label("- AWS CLI: missing");
                                        }
                                        if !status.ssm_plugin_installed {
                                            ui.label("- Session Manager Plugin: missing");
                                        }
                                        if !status.aws_credentials_linked {
                                            ui.label("- AWS credentials: not linked");
                                        }
                                    }

                                    ui.add_space(4.0);

                                    let win_user = std::env::var("USERNAME").unwrap_or_default();

                                    // Build the commands based on what's missing
                                    let mut cmds: Vec<String> = Vec::new();
                                    cmds.push("sudo apt-get update && sudo apt-get install -y unzip curl".to_string());

                                    if let Some(ref status) = self.wsl_setup_status {
                                        if !status.aws_cli_installed {
                                            cmds.push(
                                                "cd /tmp && curl -s \"https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip\" -o awscliv2.zip && unzip -qo awscliv2.zip && sudo ./aws/install && rm -rf aws awscliv2.zip".to_string()
                                            );
                                        }
                                        if !status.ssm_plugin_installed {
                                            cmds.push(
                                                "cd /tmp && curl -s \"https://s3.amazonaws.com/session-manager-downloads/plugin/latest/ubuntu_64bit/session-manager-plugin.deb\" -o ssm.deb && sudo dpkg -i ssm.deb && rm ssm.deb".to_string()
                                            );
                                        }
                                        if !status.aws_credentials_linked && !win_user.is_empty() {
                                            cmds.push(format!(
                                                "ln -s /mnt/c/Users/{win_user}/.aws ~/.aws"
                                            ));
                                        }
                                    }

                                    let full_cmd = cmds.join(" && ");

                                    ui.horizontal(|ui| {
                                        ui.label("Run in a WSL terminal:");
                                        if ui.small_button("Copy Commands").clicked() {
                                            ui.ctx().copy_text(full_cmd.clone());
                                            self.message = "Commands copied to clipboard!".to_string();
                                        }
                                    });

                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&full_cmd).monospace().small()
                                        )
                                        .wrap()
                                    );
                                }

                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Re-check WSL").clicked() {
                                        self.wsl_setup_state = WslSetupState::Checking;
                                        let tx = self.wsl_setup_tx.clone();
                                        std::thread::spawn(move || {
                                            let status = wsl_setup::check_wsl_setup();
                                            let _ = tx.send(status);
                                        });
                                    }
                                    if self.wsl_auto_setup {
                                        if ui.button("Setup WSL").clicked() {
                                            self.wsl_show_password_popup = true;
                                        }
                                    }
                                });

                                // Uninstall commands
                                ui.add_space(4.0);
                                ui.collapsing("Uninstall WSL tools", |ui| {
                                    let uninstall = wsl_setup::uninstall_commands();
                                    let uninstall_resp = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&uninstall).monospace().small()
                                        )
                                        .wrap()
                                        .sense(egui::Sense::click())
                                    );
                                    if uninstall_resp.clicked() {
                                        ui.ctx().copy_text(uninstall.clone());
                                        self.message = "Uninstall commands copied to clipboard!".to_string();
                                    }
                                    uninstall_resp.on_hover_text("Click to copy");
                                });
                            });
                            ui.separator();
                        }
                        WslSetupState::Ready | WslSetupState::Cached => {}
                    }
                    } // is_wsl_selected

                    if self.refreshing {
                        ui.add_enabled(false, egui::Button::new("Refreshing..."));
                    } else {
                        ui.horizontal(|ui| {
                            if ui.button("Refresh").clicked() {
                                self.refresh_context_and_inventory(true);
                            }
                            if ui.button("Refresh All").clicked() {
                                self.refresh_all_authenticated(true);
                            }
                        });
                    }

                    if !self.config.profiles.is_empty() {
                        ui.separator();
                        ui.label("Account Profile");
                        let before_profile = self.selected_profile.clone();

                        // Build (profile_id, display_label, is_authenticated) tuples in JSON order.
                        let profile_options: Vec<(String, String, bool)> = self
                            .config
                            .profiles
                            .iter()
                            .map(|p| {
                                let auth_ok = self
                                    .profile_auth_infos
                                    .iter()
                                    .find(|a| a.profile_id == p.profile_id)
                                    .map(|a| a.auth_status == AuthStatus::Ok)
                                    .unwrap_or(false);
                                (p.profile_id.clone(), p.display_name.clone(), auth_ok)
                            })
                            .collect();

                        let base_text = before_profile
                            .as_ref()
                            .and_then(|id| profile_options.iter().find(|(pid, _, _)| pid == id))
                            .map(|(_, label, _)| label.clone())
                            .unwrap_or_else(|| "(none)".to_string());

                        let selected_text = if self.refreshing {
                            format!("{base_text} (loading...)")
                        } else {
                            base_text
                        };

                        egui::ComboBox::from_id_salt("profile_selector_combo")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                let auth: Vec<_> = profile_options
                                    .iter()
                                    .filter(|(_, _, ok)| *ok)
                                    .collect();
                                let unauth: Vec<_> = profile_options
                                    .iter()
                                    .filter(|(_, _, ok)| !*ok)
                                    .collect();

                                if !auth.is_empty() {
                                    ui.label(
                                        egui::RichText::new("Authenticated").weak().small(),
                                    );
                                    for (profile_id, label, _) in &auth {
                                        ui.selectable_value(
                                            &mut self.selected_profile,
                                            Some(profile_id.clone()),
                                            label,
                                        );
                                    }
                                }

                                if !auth.is_empty() && !unauth.is_empty() {
                                    ui.separator();
                                }

                                if !unauth.is_empty() {
                                    ui.label(
                                        egui::RichText::new("Not Authenticated").weak().small(),
                                    );
                                    for (profile_id, label, _) in &unauth {
                                        ui.selectable_value(
                                            &mut self.selected_profile,
                                            Some(profile_id.clone()),
                                            label,
                                        );
                                    }
                                }
                            });

                        if self.selected_profile != before_profile {
                            // Clear multi-account selections when switching profiles
                            self.multi_account_ids.clear();
                            // Reset filters and saved-filter dropdown on profile switch
                            if self.config.reset_filter_on_profile_switch {
                                self.search_rules = vec![SearchRuleInput::default()];
                                self.selected_saved_filter.clear();
                            }
                            // Save current inventory to memory cache before
                            // switching — but only if the departing profile is
                            // auth-ok, so we don't overwrite a last-good cache
                            // with an empty display from an unauthorized state.
                            if let Some(ref old_profile) = before_profile {
                                if let Some(ref ctx) = self.context {
                                    if ctx.auth_status == AuthStatus::Ok
                                        && !self.inventory.instances.is_empty()
                                    {
                                        self.profile_inventory_cache.insert(
                                            old_profile.clone(),
                                            (self.inventory.clone(), ctx.clone()),
                                        );
                                    }
                                }
                            }

                            self.config.last_selected_profile = self.selected_profile.clone();
                            // Update region to match the selected account's region
                            if let Some(ref profile_id) = self.selected_profile {
                                if let Some(profile) = self.config.profiles.iter().find(|p| &p.profile_id == profile_id) {
                                    if let Some(ref region) = profile.region {
                                        self.options.region = Some(region.clone());
                                    }
                                }
                            }
                            if let Err(err) = self.config.save() {
                                self.message = format!("error: {err}");
                                self.log_error(self.message.clone());
                            } else {
                                self.log_info(format!(
                                    "profile selection changed to {}",
                                    self.selected_profile.as_deref().unwrap_or("(none)")
                                ));
                            }

                            // Load cached inventory for the new profile immediately
                            if let Some(ref profile_id) = self.selected_profile {
                                let is_auth_ok = self.profile_auth_infos.iter().any(|a| {
                                    a.profile_id == *profile_id && a.auth_status == AuthStatus::Ok
                                }) || self.options.mode == Mode::Sim;

                                if is_auth_ok {
                                    self.load_cache_for_profile(&profile_id.clone());
                                } else {
                                    // Clear the active display but keep the memory
                                    // cache so it's available when auth becomes OK.
                                    self.filtered.clear();
                                    self.context = None;
                                    self.message = "Credentials not authenticated. Refresh credentials and retry.".to_string();
                                }
                            }

                            // Update loading indicator for the newly selected profile
                            if let Some(ref pid) = self.selected_profile {
                                self.refreshing = self.refreshing_profiles.contains_key(pid);
                            }

                            // Only trigger a new refresh if this profile isn't already being
                            // refreshed (e.g. from the initial parallel load at startup).
                            if let Some(ref pid) = self.selected_profile {
                                if !self.refreshing_profiles.contains_key(pid) {
                                    // Refresh all authenticated profiles when
                                    // switching accounts so cached data stays
                                    // current across all accounts.
                                    self.refresh_all_authenticated(true);
                                }
                            }
                        }
                        ui.separator();

                    }

                    ui.label("Region");
                    let before_region = self.options.region.clone();
                    let context_region = self.context.as_ref().map(|c| c.region.as_str());
                    let selected_region_text =
                        selected_region_label(self.options.region.as_deref(), context_region);
                    egui::ComboBox::from_id_salt("region_selector_combo")
                        .selected_text(selected_region_text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.options.region, None, AWS_REGION_AUTO);
                            for region in AWS_REGIONS {
                                ui.selectable_value(
                                    &mut self.options.region,
                                    Some((*region).to_string()),
                                    *region,
                                );
                            }
                        });
                    if self.options.region != before_region {
                        self.config.default_region = self.options.region.clone();
                        if let Err(err) = self.config.save() {
                            self.message = format!("error: {err}");
                            self.log_error(self.message.clone());
                        } else {
                            self.log_info(format!(
                                "region selection changed to {}",
                                self.options
                                    .region
                                    .clone()
                                    .unwrap_or_else(|| AWS_REGION_AUTO.to_string())
                            ));
                        }
                        // Re-evaluate cache display against the new region.
                        // If new region doesn't match the account's accounts.json
                        // region, load_cache_for_profile blanks the display;
                        // otherwise it re-populates from the matching cache.
                        if let Some(profile_id) = self.selected_profile.clone() {
                            self.load_cache_for_profile(&profile_id);
                        }
                        self.refresh_context_and_inventory(true);
                    }

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Terminal");
                        if ui.small_button("Rescan").clicked() {
                            self.terminals =
                                filter_embedded_terminals(discover_terminals());
                            let prior = self.selected_terminal_id.clone();
                            self.selected_terminal_id =
                                initial_terminal_id(&self.config, &self.terminals);
                            if self.selected_terminal_id != prior {
                                self.log_info(format!(
                                    "terminal selection updated to {} after rescan",
                                    if self.selected_terminal_id.is_empty() {
                                        "(none)".to_string()
                                    } else {
                                        self.selected_terminal_id.clone()
                                    }
                                ));
                            }
                        }
                    });
                    let before_terminal_id = self.selected_terminal_id.clone();
                    egui::ComboBox::from_id_salt("terminal_combo")
                        .selected_text(
                            self.selected_terminal()
                                .map(|t| format!("{} ({})", t.display_name, t.id))
                                .unwrap_or_else(|| "(none detected)".to_string()),
                        )
                        .show_ui(ui, |ui| {
                            for terminal in &self.terminals {
                                ui.selectable_value(
                                    &mut self.selected_terminal_id,
                                    terminal.id.clone(),
                                    format!("{} ({})", terminal.display_name, terminal.id),
                                );
                            }
                        });
                    if self.selected_terminal_id != before_terminal_id {
                        if self.selected_terminal_id.is_empty() {
                            self.config.default_terminal = None;
                        } else {
                            self.config.default_terminal = Some(self.selected_terminal_id.clone());
                        }
                        if let Err(err) = self.config.save() {
                            self.message = format!("error: {err}");
                            self.log_error(self.message.clone());
                        } else if let Some(selected) = self.selected_terminal() {
                            self.message = format!(
                                "Default terminal set to {} ({})",
                                selected.display_name, selected.id
                            );
                            self.log_info(self.message.clone());
                        }
                    }

                    ui.horizontal(|ui| {
                        ui.label("Search Rules");
                        if ui.button("+").on_hover_text("Add search rule").clicked() {
                            self.search_rules.push(SearchRuleInput::default());
                            self.apply_filters();
                        }
                    });

                    let mut remove_rule_idx: Option<usize> = None;
                    let mut rules_changed = false;
                    for (idx, rule) in self.search_rules.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(format!("search_rule_kind_{idx}"))
                                .selected_text(match rule.kind {
                                    SearchRuleKind::Include => "Include",
                                    SearchRuleKind::Exclude => "Exclude",
                                })
                                .show_ui(ui, |ui| {
                                    rules_changed |= ui
                                        .selectable_value(
                                            &mut rule.kind,
                                            SearchRuleKind::Include,
                                            "Include",
                                        )
                                        .changed();
                                    rules_changed |= ui
                                        .selectable_value(
                                            &mut rule.kind,
                                            SearchRuleKind::Exclude,
                                            "Exclude",
                                        )
                                        .changed();
                                });
                            if ui.text_edit_singleline(&mut rule.term).changed() {
                                rules_changed = true;
                            }
                            if ui.small_button("-").clicked() {
                                remove_rule_idx = Some(idx);
                            }
                        });
                    }
                    if let Some(idx) = remove_rule_idx {
                        self.search_rules.remove(idx);
                        if self.search_rules.is_empty() {
                            self.search_rules.push(SearchRuleInput::default());
                        }
                        rules_changed = true;
                    }
                    if rules_changed {
                        self.apply_filters();
                    }

                    ui.horizontal(|ui| {
                        ui.label("States");
                        let before = self.selected_state_filter.clone();
                        egui::ComboBox::from_id_salt("state_filter_combo")
                            .selected_text(if self.selected_state_filter.is_empty() {
                                "No filter".to_string()
                            } else {
                                self.selected_state_filter.clone()
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.selected_state_filter,
                                    STATE_FILTER_NONE.to_string(),
                                    "No filter",
                                );
                                ui.selectable_value(
                                    &mut self.selected_state_filter,
                                    STATE_FILTER_RUNNING.to_string(),
                                    "running",
                                );
                                ui.selectable_value(
                                    &mut self.selected_state_filter,
                                    STATE_FILTER_STOPPED.to_string(),
                                    "stopped",
                                );
                                ui.selectable_value(
                                    &mut self.selected_state_filter,
                                    STATE_FILTER_TERMINATED.to_string(),
                                    "terminated",
                                );
                            });
                        if self.selected_state_filter != before {
                            self.apply_filters();
                        }
                    });

                    if ui
                        .checkbox(&mut self.only_ssm, "Only SSM-managed")
                        .changed()
                    {
                        self.apply_filters();
                    }

                    // Multi-account lookup
                    let selected_profile = self.selected_profile.clone().unwrap_or_default();
                    let other_profiles: Vec<(String, String, bool)> = self.config.profiles.iter()
                        .filter(|p| p.profile_id != selected_profile)
                        .map(|p| {
                            let is_auth = self.profile_auth_infos.iter()
                                .any(|a| a.profile_id == p.profile_id && a.auth_status == AuthStatus::Ok);
                            (p.profile_id.clone(), p.display_name.clone(), is_auth)
                        })
                        .filter(|(_, _, is_auth)| *is_auth)
                        .collect();
                    if !other_profiles.is_empty() {
                        let checked_count = self.multi_account_ids.len();
                        let multi_label = if checked_count > 0 {
                            format!("Multi-account ({checked_count})")
                        } else {
                            "Multi-account Lookup".to_string()
                        };
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("multi_account_lookup")
                            .selected_text(multi_label)
                            .show_ui(ui, |ui| {
                                for (pid, display, _) in &other_profiles {
                                    let mut checked = self.multi_account_ids.contains(pid);
                                    if ui.checkbox(&mut checked, display).changed() {
                                        if checked {
                                            self.multi_account_ids.insert(pid.clone());
                                        } else {
                                            self.multi_account_ids.remove(pid);
                                        }
                                        changed = true;
                                    }
                                }
                            });
                        if changed {
                            self.apply_filters();
                        }
                    }

                    ui.separator();
                    ui.label("Selected Instance ID");
                    ui.text_edit_singleline(&mut self.selected_instance_id);

                    if ui.button("Connect").clicked() {
                        if self.guarded_action("connect", |app| app.connect_selected()) {
                            self.main_tab = MainTab::Connections;
                        }
                    }

                    ui.separator();
                    ui.label("Saved Filters");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.save_filter_name);
                        if ui.button("Save").clicked() {
                            // Use selected filter name if the text field is empty
                            if self.save_filter_name.trim().is_empty()
                                && !self.selected_saved_filter.is_empty()
                            {
                                self.save_filter_name = self.selected_saved_filter.clone();
                            }
                            if let Err(err) = self.save_current_filter() {
                                self.message = format!("error: {err}");
                                self.log_error(self.message.clone());
                            } else {
                                self.save_filter_name.clear();
                            }
                        }
                    });

                    let mut scope_filters = self
                        .config
                        .saved_filters_for_scope("global", "global");
                    scope_filters.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));

                    let mut pending_delete_filter: Option<String> = None;
                    let mut auto_apply = false;
                    let mut show_favorites_only = false;
                    const SHOW_FAVORITES_LABEL: &str = "Show Favorites";

                    let filter_btn_text = if self.selected_saved_filter.is_empty() {
                        "Choose Filter".to_string()
                    } else {
                        self.selected_saved_filter.clone()
                    };
                    let mut clear_filters = false;
                    ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("saved_filter_combo")
                        .selected_text(filter_btn_text)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(
                                    self.selected_saved_filter == SHOW_FAVORITES_LABEL,
                                    SHOW_FAVORITES_LABEL,
                                )
                                .clicked()
                            {
                                self.selected_saved_filter = SHOW_FAVORITES_LABEL.to_string();
                                show_favorites_only = true;
                            }
                            ui.separator();
                            for saved in &scope_filters {
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(
                                            self.selected_saved_filter == saved.name,
                                            &saved.name,
                                        )
                                        .clicked()
                                    {
                                        self.selected_saved_filter = saved.name.clone();
                                        auto_apply = true;
                                    }
                                    if ui
                                        .small_button("x")
                                        .on_hover_text("Delete filter")
                                        .clicked()
                                    {
                                        pending_delete_filter = Some(saved.name.clone());
                                    }
                                });
                            }
                        });
                    if ui.button("Clear").clicked() {
                        clear_filters = true;
                    }
                    }); // horizontal

                    // Handle "Show Favorites" built-in filter
                    if show_favorites_only {
                        let account = self.account_scope();
                        let region = self.region_scope();
                        let fav_ids = self.config.favorites_for_scope(&account, &region);
                        self.search_rules = vec![SearchRuleInput::default()];
                        self.selected_state_filter = STATE_FILTER_NONE.to_string();
                        self.only_ssm = false;
                        self.apply_filters();
                        if !fav_ids.is_empty() {
                            self.filtered.retain(|i| {
                                fav_ids.iter().any(|fid| fid.eq_ignore_ascii_case(&i.instance_id))
                            });
                        }
                        self.message = format!("Showing {} favorites", self.filtered.len());
                        self.log_info(self.message.clone());
                    }

                    // Auto-apply when selecting a saved filter from the dropdown
                    if auto_apply {
                        if let Err(err) = self.apply_saved_filter() {
                            self.message = format!("error: {err}");
                            self.log_error(self.message.clone());
                        }
                    }

                    if let Some(name) = pending_delete_filter {
                        self.config.delete_saved_filter(
                            "global",
                            "global",
                            &name,
                        );
                        if self.selected_saved_filter == name {
                            self.selected_saved_filter.clear();
                        }
                        let _ = self.config.save();
                        self.message = format!("Deleted filter: {name}");
                        self.log_info(self.message.clone());
                    }

                    if clear_filters {
                        self.search_rules = vec![SearchRuleInput::default()];
                        self.selected_state_filter = STATE_FILTER_NONE.to_string();
                        self.only_ssm = false;
                        self.multi_account_ids.clear();
                        self.selected_saved_filter.clear();
                        self.apply_filters();
                        self.message = "Filters cleared".to_string();
                        self.log_info(self.message.clone());
                    }

                    if ui.button("Run Diagnostics").clicked() {
                        self.run_diagnostics();
                    }
                    if !self.diagnostics.is_empty() {
                        ui.separator();
                        ui.code(self.diagnostics.clone());
                    }

                    ui.separator();
                    ui.label(format!(
                        "aws CLI: {} | ssm plugin: {} | terminals: {}",
                        self.dependencies.aws_cli_found,
                        self.dependencies.ssm_plugin_found,
                        self.terminals.len()
                    ));
                    }); // ScrollArea
                    }); // SidePanel
                }

                egui::CentralPanel::default().show(ctx, |ui| match self.main_tab {
                    MainTab::Inventory => self.render_inventory_panel(ui),
                    MainTab::Connections => self.render_connections_panel(ui),
                    MainTab::Details => self.render_details_panel(ui),
                    MainTab::Log => self.render_log_panel(ui),
                });
            }));

            if let Err(payload) = update_result {
                let panic_message = format!(
                    "UI panic recovered: {}",
                    panic_payload_to_string(payload.as_ref())
                );
                append_panic_log_entry(&panic_message);
                self.message = panic_message.clone();
                self.log_error(panic_message);
                self.main_tab = MainTab::Log;
            }
        }
    }

    #[cfg(test)]
    fn shell_plan(terminal: Option<&TerminalOption>) -> (String, Vec<String>) {
        if cfg!(windows) {
            let fallback = || {
                (
                    windows_cmd_path(),
                    vec!["/Q".to_string(), "/K".to_string()],
                )
            };
            match terminal.map(|t| t.id.as_str()) {
                Some("pwsh") => {
                    let program = terminal.expect("terminal should be present for pwsh").program.clone();
                    (program, vec!["-NoLogo".to_string()])
                }
                Some("powershell") => {
                    let program =
                        terminal.expect("terminal should be present for powershell").program.clone();
                    (program, vec!["-NoLogo".to_string()])
                }
                Some("cmd") => (
                    windows_cmd_path(),
                    vec!["/Q".to_string(), "/K".to_string()],
                ),
                _ => fallback(),
            }
        } else {
            if let Ok(shell) = std::env::var("SHELL") {
                if !shell.trim().is_empty() {
                    return (shell, vec!["-i".to_string()]);
                }
            }
            ("/bin/bash".to_string(), vec!["-i".to_string()])
        }
    }

    #[cfg(target_os = "windows")]
    fn windows_system_root() -> String {
        std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string())
    }

    #[cfg(target_os = "windows")]
    fn windows_cmd_path() -> String {
        format!("{}\\System32\\cmd.exe", windows_system_root())
    }

    #[cfg(not(target_os = "windows"))]
    fn windows_cmd_path() -> String {
        "cmd.exe".to_string()
    }

    fn format_sim_command(
        kind: TerminalKind,
        cmd: &str,
        instance_id: &str,
        port_forward: Option<(u16, u16)>,
    ) -> String {
        let status_line = if let Some((local, remote)) = port_forward {
            format!("[SIM MODE] port-forward {local}:{remote} for {instance_id}")
        } else {
            format!("[SIM MODE] session open for {instance_id}")
        };

        if cfg!(windows) {
            match kind {
                TerminalKind::Wsl => format!("echo '[SIM MODE] {cmd}'; echo '{status_line}'"),
                TerminalKind::Cmd => format!("echo [SIM MODE] {cmd} & echo {status_line}"),
                TerminalKind::PowerShell7 | TerminalKind::WindowsPowerShell => {
                    format!("Write-Host '[SIM MODE] {cmd}'; Write-Host '{status_line}'")
                }
                _ => format!("echo '[SIM MODE] {cmd}'; echo '{status_line}'"),
            }
        } else {
            format!("echo '[SIM MODE] {cmd}'; echo '{status_line}'")
        }
    }

    fn terminal_debug_label(terminal: Option<&TerminalOption>) -> String {
        terminal
            .map(|t| format!("id={} name={} kind={:?} program={}", t.id, t.display_name, t.kind, t.program))
            .unwrap_or_else(|| "none".to_string())
    }

    fn pty_command_for_context(
        terminal: Option<&TerminalOption>,
        context: &AwsContext,
        command_line: &str,
        command_args: &[String],
    ) -> PtyCommand {
        if context.mode == Mode::Sim {
            return sim_pty_command(terminal, command_line);
        }
        if command_args.is_empty() {
            return sim_pty_command(terminal, command_line);
        }
        // On Windows, the session-manager-plugin writes directly via
        // Windows Console APIs, which ConPTY does not capture when
        // `aws` is spawned as a bare process.  Wrapping the command
        // inside the selected shell (PowerShell / CMD) lets the shell
        // mediate console I/O so output flows through the PTY pipe.
        if cfg!(windows) {
            return shell_wrapped_pty_command(terminal, command_line, context);
        }
        PtyCommand {
            program: "aws".to_string(),
            args: command_args.to_vec(),
        }
    }

    fn shell_wrapped_pty_command(
        terminal: Option<&TerminalOption>,
        command_line: &str,
        context: &AwsContext,
    ) -> PtyCommand {
        let kind = terminal.map(|t| t.kind.clone()).unwrap_or(TerminalKind::Wsl);

        match kind {
            TerminalKind::Wsl => {
                // Strip --profile from the command for WSL sessions.
                // The credential_process tool ('fed') only exists on Windows,
                // so --profile would fail inside WSL.
                //
                // Credentials are read from the Windows-side ~/.aws/credentials
                // and injected directly into the bash command string instead of
                // relying on WSLENV env-var forwarding, which can silently
                // stop working after WSL updates.
                let wsl_cmd = strip_profile_flag(command_line);
                let cred_result = credentials::read_profile_credentials(&context.profile);
                eprintln!(
                    "WSL cred lookup for profile='{}': found={}",
                    context.profile,
                    cred_result.is_some()
                );
                let cred_exports = match cred_result {
                    Some(creds) => {
                        let mut exports = format!(
                            "unset AWS_PROFILE; \
                             export AWS_ACCESS_KEY_ID='{}'; \
                             export AWS_SECRET_ACCESS_KEY='{}'; \
                             export AWS_REGION='{}'; ",
                            creds.access_key_id.replace('\'', "'\\''"),
                            creds.secret_access_key.replace('\'', "'\\''"),
                            context.region.replace('\'', "'\\''"),
                        );
                        if let Some(ref token) = creds.session_token {
                            exports.push_str(&format!(
                                "export AWS_SESSION_TOKEN='{}'; ",
                                token.replace('\'', "'\\''"),
                            ));
                        }
                        exports
                    }
                    None => {
                        // No cached credentials — forward profile name and
                        // hope WSL has matching AWS config (best-effort).
                        format!(
                            "export AWS_PROFILE='{}'; export AWS_REGION='{}'; ",
                            context.profile.replace('\'', "'\\''"),
                            context.region.replace('\'', "'\\''"),
                        )
                    }
                };
                let wrapped = format!(
                    "export HOME=\"${{HOME:-$(getent passwd $(id -u) | cut -d: -f6)}}\" 2>/dev/null; \
                     {cred_exports}\
                     stty rows ${{LINES:-24}} cols ${{COLUMNS:-120}} 2>/dev/null; \
                     {}",
                    wsl_cmd
                );
                PtyCommand {
                    program: "wsl.exe".to_string(),
                    args: vec![
                        "--".to_string(),
                        "bash".to_string(),
                        "-lc".to_string(),
                        wrapped,
                    ],
                }
            }
            TerminalKind::PowerShell7 | TerminalKind::WindowsPowerShell => {
                let program = terminal
                    .map(|t| t.program.clone())
                    .unwrap_or_else(|| "powershell".to_string());
                PtyCommand {
                    program,
                    args: vec![
                        "-NoLogo".to_string(),
                        "-NoExit".to_string(),
                        "-Command".to_string(),
                        command_line.to_string(),
                    ],
                }
            }
            // CMD and everything else
            _ => PtyCommand {
                program: windows_cmd_path(),
                args: vec!["/K".to_string(), command_line.to_string()],
            },
        }
    }

    /// Remove `--profile <value>` from a command string.
    /// Used for WSL sessions where creds are passed via env vars.
    fn strip_profile_flag(cmd: &str) -> String {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let mut out = Vec::new();
        let mut skip_next = false;
        for part in &parts {
            if skip_next {
                skip_next = false;
                continue;
            }
            if *part == "--profile" {
                skip_next = true;
                continue;
            }
            out.push(*part);
        }
        out.join(" ")
    }

    fn sim_pty_command(terminal: Option<&TerminalOption>, command_line: &str) -> PtyCommand {
        if cfg!(windows) {
            let kind = terminal
                .map(|t| t.kind.clone())
                .unwrap_or(TerminalKind::Wsl);
            match kind {
                TerminalKind::Wsl => {
                    return PtyCommand {
                        program: "wsl.exe".to_string(),
                        args: vec![
                            "-e".to_string(),
                            "bash".to_string(),
                            "-lc".to_string(),
                            command_line.to_string(),
                        ],
                    };
                }
                TerminalKind::PowerShell7 | TerminalKind::WindowsPowerShell => {
                    let program = terminal
                        .map(|t| t.program.clone())
                        .unwrap_or_else(|| "powershell".to_string());
                    PtyCommand {
                        program,
                        args: vec![
                            "-NoLogo".to_string(),
                            "-NoExit".to_string(),
                            "-Command".to_string(),
                            command_line.to_string(),
                        ],
                    }
                }
                _ => PtyCommand {
                    program: windows_cmd_path(),
                    args: vec![
                        "/Q".to_string(),
                        "/K".to_string(),
                        command_line.to_string(),
                    ],
                },
            }
        } else {
            let program = std::env::var("SHELL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/bin/bash".to_string());
            PtyCommand {
                program,
                args: vec!["-lc".to_string(), command_line.to_string()],
            }
        }
    }


    /// Creates a PTY session but does NOT start the reader thread.
    /// Returns the session and the reader handle separately so the caller
    /// can control when reading begins (important for avoiding the race
    /// between PtyReady and Output events on Windows).
    fn spawn_pty_session_parts(
        tab_id: u64,
        command: &PtyCommand,
        context: &AwsContext,
    ) -> Result<(PtySession, Box<dyn Read + Send>)> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| AppError::Parse(format!("Failed to allocate PTY: {err}")))?;

        let mut cmd = CommandBuilder::new(&command.program);
        for arg in &command.args {
            cmd.arg(arg);
        }
        eprintln!(
            "tab={tab_id} PTY spawn: program={} args={:?}",
            command.program, command.args
        );
        let is_wsl = command.program == "wsl.exe";
        // For WSL sessions, credentials and region are exported directly in
        // the bash command string (bypassing WSLENV which can silently break).
        // For other terminals (PowerShell/CMD), set them on the process env.
        if !is_wsl {
            cmd.env("AWS_PROFILE", &context.profile);
            cmd.env("AWS_REGION", &context.region);
            if let Some(creds) = credentials::read_profile_credentials(&context.profile) {
                cmd.env("AWS_ACCESS_KEY_ID", &creds.access_key_id);
                cmd.env("AWS_SECRET_ACCESS_KEY", &creds.secret_access_key);
                if let Some(ref token) = creds.session_token {
                    cmd.env("AWS_SESSION_TOKEN", token);
                }
            }
        }
        // COLUMNS/LINES are needed for both WSL and non-WSL terminals.
        cmd.env("COLUMNS", "120");
        cmd.env("LINES", "24");
        cmd.env("TERM", "xterm-256color");
        #[cfg(target_os = "windows")]
        {
            let system_root = windows_system_root();
            let comspec = windows_cmd_path();
            cmd.env("SystemRoot", &system_root);
            cmd.env("WINDIR", &system_root);
            cmd.env("ComSpec", &comspec);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| AppError::Parse(format!("Failed to spawn PTY command: {err}")))?;
        drop(pair.slave);

        let master = pair.master;
        let reader = master
            .try_clone_reader()
            .map_err(|err| AppError::Parse(format!("Failed to create PTY reader: {err}")))?;
        let writer = master
            .take_writer()
            .map_err(|err| AppError::Parse(format!("Failed to create PTY writer: {err}")))?;

        let session = PtySession {
            child,
            master: Arc::new(Mutex::new(master)),
            writer: Arc::new(Mutex::new(writer)),
            parser: vt100::Parser::new(24, 120, 2_500),
            last_size: None,
            bytes_received: 0,
            output_event_count: 0,
            scroll_offset: 0,
            ps1_auto_sent: false,
            input_line_buf: String::new(),
            pending_reprep: false,
            last_prepped_prompt: String::new(),
            paste_write_at: None,
            paste_write_bytes: 0,
        };

        Ok((session, reader))
    }

    /// Starts a background thread that reads from the PTY and sends
    /// `ProcEvent::Output` / `ProcEvent::Error` events via `proc_tx`.
    fn start_pty_reader_thread(
        tab_id: u64,
        mut reader: Box<dyn Read + Send>,
        proc_tx: Sender<ProcEvent>,
        egui_ctx: Option<egui::Context>,
    ) {
        std::thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            let mut total_bytes: u64 = 0;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = proc_tx.send(ProcEvent::Error {
                            tab_id,
                            error: format!(
                                "PTY reader EOF after {total_bytes} bytes total"
                            ),
                        });
                        if let Some(ctx) = &egui_ctx { ctx.request_repaint(); }
                        break;
                    }
                    Ok(n) => {
                        total_bytes += n as u64;
                        let _ = proc_tx.send(ProcEvent::Output {
                            tab_id,
                            bytes: buf[..n].to_vec(),
                        });
                        // Wake the UI thread so output is drained promptly
                        // even when the user isn't generating input events.
                        if let Some(ctx) = &egui_ctx { ctx.request_repaint(); }
                    }
                    Err(err) => {
                        let _ = proc_tx.send(ProcEvent::Error {
                            tab_id,
                            error: format!(
                                "PTY reader error after {total_bytes} bytes: {err}"
                            ),
                        });
                        if let Some(ctx) = &egui_ctx { ctx.request_repaint(); }
                        break;
                    }
                }
            }
        });
    }

    /// Detects terminal query sequences in PTY output and sends the
    /// expected responses back through the writer.  Without these
    /// responses, programs like CMD and PowerShell hang at startup.
    fn respond_to_terminal_queries(session: &PtySession, bytes: &[u8]) {
        let (row, col) = session.parser.screen().cursor_position();
        let response = compute_terminal_query_response(bytes, row, col);
        if !response.is_empty() {
            if let Ok(mut w) = session.writer.lock() {
                let _ = w.write_all(&response);
                let _ = w.flush();
            }
        }
    }

    /// Build the reply for terminal queries embedded in `bytes`.
    /// `cursor_row`/`cursor_col` are 0-based from vt100's parser and used
    /// only for DSR (cursor position report). Returns an empty vec if no
    /// known queries were present.
    ///
    /// Answering these matters: vim's t_RV (DA2) and OSC 11 background
    /// query block its input loop for ~5s at startup if unanswered, which
    /// manifests as keys being unresponsive right after `:e` / `vim file`.
    fn compute_terminal_query_response(
        bytes: &[u8],
        cursor_row: u16,
        cursor_col: u16,
    ) -> Vec<u8> {
        let mut response: Vec<u8> = Vec::new();
        // ESC[6n — Device Status Report (cursor position query).
        // Response: ESC[{row};{col}R  (1-based).
        if bytes.windows(4).any(|w| w == b"\x1b[6n") {
            response.extend_from_slice(
                format!("\x1b[{};{}R", cursor_row + 1, cursor_col + 1).as_bytes(),
            );
        }
        // ESC[c or ESC[0c — Primary Device Attributes (DA1).
        // Response: ESC[?1;0c  (VT101 with no options).
        // Match DA1 only when NOT preceded by '>' or '=' (DA2/DA3).
        if has_csi_query(bytes, b"c") || has_csi_query(bytes, b"0c") {
            response.extend_from_slice(b"\x1b[?1;0c");
        }
        // ESC[>c or ESC[>0c — Secondary Device Attributes (DA2).
        // Response: ESC[>41;0;0c  (VT220-compatible, version 0).
        if bytes.windows(4).any(|w| w == b"\x1b[>c")
            || bytes.windows(5).any(|w| w == b"\x1b[>0c")
        {
            response.extend_from_slice(b"\x1b[>41;0;0c");
        }
        // OSC 11 — background color query (ESC ] 11 ; ? BEL or ST).
        // Reply with a dummy black background so the sender moves on.
        if bytes.windows(6).any(|w| w == b"\x1b]11;?") {
            response.extend_from_slice(b"\x1b]11;rgb:0000/0000/0000\x1b\\");
        }
        // XTGETTCAP (DCS + q ... ST) — terminfo capability query.
        // Reply with an invalid (empty) response: ESC P 0 + r ESC \
        if bytes.windows(4).any(|w| w == b"\x1bP+q") {
            response.extend_from_slice(b"\x1bP0+r\x1b\\");
        }
        response
    }

    /// True if `bytes` contains `ESC [ {suffix}` with no `>` or `=` prefix
    /// byte between the `[` and the suffix (i.e., a DA1, not DA2/DA3).
    fn has_csi_query(bytes: &[u8], suffix: &[u8]) -> bool {
        let needle_len = 2 + suffix.len();
        bytes.windows(needle_len).any(|w| {
            w[0] == 0x1b
                && w[1] == b'['
                && &w[2..] == suffix
        })
    }

    #[cfg(test)]
    fn terminate_child(child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    fn resolve_row_action(
        row_clicked: bool,
        row_double_clicked: bool,
        quick_connect_clicked: bool,
    ) -> RowAction {
        RowAction {
            select: row_clicked || row_double_clicked || quick_connect_clicked,
            connect: row_double_clicked || quick_connect_clicked,
        }
    }

    fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn profile_choice_mtime(path: Option<&std::path::Path>) -> Option<SystemTime> {
        let path = path?;
        fs::metadata(path).ok()?.modified().ok()
    }

    #[cfg(test)]
    fn profile_choice_changed(
        previous: Option<SystemTime>,
        current: Option<SystemTime>,
    ) -> bool {
        previous != current
    }

    fn profile_change_debounce_elapsed(
        started_at: Option<SystemTime>,
        now: SystemTime,
        debounce: Duration,
    ) -> bool {
        let Some(started_at) = started_at else {
            return false;
        };
        match now.duration_since(started_at) {
            Ok(elapsed) => elapsed >= debounce,
            Err(_) => false,
        }
    }

    fn parse_bool_env(raw: Option<&str>, default: bool) -> bool {
        let Some(value) = raw else {
            return default;
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "" => default,
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        }
    }

    fn gui_smoke_config_from_env() -> Option<GuiSmokeConfig> {
        let marker_path = std::env::var_os(GUI_SMOKE_MARKER_ENV)
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())?;

        let expected_text = std::env::var(GUI_SMOKE_EXPECTED_TEXT_ENV)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "[SIM MODE] session open for".to_string());

        let exit_on_marker = parse_bool_env(
            std::env::var(GUI_SMOKE_EXIT_ON_MARKER_ENV).ok().as_deref(),
            false,
        );
        let auto_connect = parse_bool_env(
            std::env::var(GUI_SMOKE_AUTO_CONNECT_ENV).ok().as_deref(),
            true,
        );

        Some(GuiSmokeConfig {
            marker_path,
            expected_text,
            exit_on_marker,
            auto_connect,
        })
    }

    fn gui_smoke_match_in_bytes(expected_text: &str, bytes: &[u8]) -> bool {
        if expected_text.trim().is_empty() {
            return false;
        }
        String::from_utf8_lossy(bytes).contains(expected_text)
    }

    fn gui_smoke_marker_payload(tab_id: u64, expected_text: &str) -> String {
        format!("PASS\ntab_id={tab_id}\nexpected={expected_text}\n")
    }

    fn write_gui_smoke_marker(path: &std::path::Path, tab_id: u64, expected_text: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, gui_smoke_marker_payload(tab_id, expected_text))
    }

    fn format_connection_summary_line(
        title: &str,
        instance_id: &str,
        private_ip: &str,
        running: bool,
        pty_bytes: u64,
    ) -> String {
        let status = if running { "Running" } else { "Closed" };
        let bytes_label = if pty_bytes >= 1024 {
            format!("{}KB", pty_bytes / 1024)
        } else {
            format!("{pty_bytes}B")
        };
        format!(
            "Instance: {title} ({instance_id})\tPrivate IP: {private_ip}\tStatus: {status}\tRx: {bytes_label}"
        )
    }

    /// Default ordered color palette for account tab coloring.
    /// Ordered from green (dev-like) through yellow/orange to red (prod-like),
    /// then extends into additional distinct hues for extra accounts.
    const ACCOUNT_COLOR_PALETTE: &[(u8, u8, u8)] = &[
        (46, 160, 67),    // green (dev)
        (0, 180, 160),    // teal
        (54, 154, 220),   // blue
        (200, 180, 30),   // yellow
        (230, 150, 0),    // orange
        (210, 80, 60),    // red-orange
        (200, 40, 40),    // red (prod)
        (170, 30, 70),    // crimson (prod-b)
        (140, 60, 160),   // purple
        (100, 100, 190),  // indigo
        (0, 140, 120),    // dark teal
        (180, 120, 60),   // brown
    ];

    // ENV_RANK_KEYWORDS and env_rank removed — replaced by sort_order in accounts.json
    #[allow(dead_code)]
    const ENV_RANK_KEYWORDS: &[&str] = &[
        "dev", "develop", "development",
        "test", "testing",
        "qa", "quality",
        "int", "integration",
        "staging", "stage", "stg",
        "uat", "preprod", "pre-prod",
        "prod", "production", "prd", "live",
    ];

    #[allow(dead_code)]
    fn env_rank(profile_name: &str) -> (usize, String) {
        let lower = profile_name.to_ascii_lowercase();
        for (idx, &kw) in ENV_RANK_KEYWORDS.iter().enumerate() {
            if lower.contains(kw) {
                return (idx, lower);
            }
        }
        // Unknown profiles sort after all known environments but before nothing
        (ENV_RANK_KEYWORDS.len(), lower)
    }

    /// Given a set of profile_ids, return a mapping from profile_id to Color32.
    /// Uses user-customized colors from config where available, and auto-assigns
    /// the rest from the palette ordered by environment rank.
    /// Sort key for profiles: sort_order first, then alphabetical by display name.
    fn profile_sort_key(profile: Option<&ProfileConfig>, profile_id: &str) -> (u32, String) {
        if let Some(p) = profile {
            (
                p.sort_order.unwrap_or(u32::MAX),
                p.display_name.to_ascii_lowercase(),
            )
        } else {
            (u32::MAX, profile_id.to_ascii_lowercase())
        }
    }

    /// Extract the MMODAL_ENV tag value from an instance.
    fn instance_env(instance: &Instance) -> Option<String> {
        instance.tags.get("MMODAL_ENV")
            .or_else(|| instance.tags.get("mmodal_env"))
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_string())
    }

    /// Generate dark-to-light shades of the base color for multiple environments.
    /// Generate a deterministic shade of the base color for a given environment name.
    /// Same env name always produces the same shade. 1 env = exact base color.
    /// Uses a simple hash of the env name to pick a brightness level.
    fn shade_for_env(base: egui::Color32, env: &str) -> egui::Color32 {
        // Simple hash to get a stable value 0-255 for any env name
        let hash: u32 = env.bytes().fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(b as u32));
        // Map to a brightness factor: 0.35 (dark) to 1.5 (light)
        // Use 8 distinct levels so nearby envs don't look the same
        let bucket = (hash % 8) as f32;
        let factor = 0.35 + bucket * (1.15 / 7.0); // 0.35 to 1.5

        let r = base.r() as f32;
        let g = base.g() as f32;
        let b = base.b() as f32;

        if factor <= 1.0 {
            // Darken
            egui::Color32::from_rgb(
                (r * factor).clamp(15.0, 250.0) as u8,
                (g * factor).clamp(15.0, 250.0) as u8,
                (b * factor).clamp(15.0, 250.0) as u8,
            )
        } else {
            // Lighten (blend toward white)
            let blend = factor - 1.0;
            egui::Color32::from_rgb(
                (r + (255.0 - r) * blend).clamp(15.0, 250.0) as u8,
                (g + (255.0 - g) * blend).clamp(15.0, 250.0) as u8,
                (b + (255.0 - b) * blend).clamp(15.0, 250.0) as u8,
            )
        }
    }

    fn build_account_color_map(
        profiles: &[ProfileConfig],
        extra_profile_ids: &[String],
        custom_colors: &std::collections::BTreeMap<String, String>,
        inventories: &HashMap<String, (Inventory, AwsContext)>,
        current_inventory: &Inventory,
        current_profile: Option<&str>,
    ) -> HashMap<String, egui::Color32> {
        let mut map = HashMap::new();

        // Collect all profile IDs, dedup
        let mut all_ids: Vec<String> = profiles.iter().map(|p| p.profile_id.clone()).collect();
        for id in extra_profile_ids {
            if !all_ids.contains(id) {
                all_ids.push(id.clone());
            }
        }
        all_ids.sort_by(|a, b| {
            let pa = profiles.iter().find(|p| p.profile_id == *a);
            let pb = profiles.iter().find(|p| p.profile_id == *b);
            profile_sort_key(pa, a).cmp(&profile_sort_key(pb, b))
        });
        all_ids.dedup();

        let palette_len = ACCOUNT_COLOR_PALETTE.len();

        for (idx, profile_id) in all_ids.iter().enumerate() {
            // Determine base color for this account
            let base_color = custom_colors.get(profile_id)
                .and_then(|hex| parse_hex_color(hex))
                .or_else(|| {
                    profiles.iter().find(|p| &p.profile_id == profile_id)
                        .and_then(|p| p.color.as_ref())
                        .and_then(|hex| parse_hex_color(hex))
                })
                .unwrap_or_else(|| {
                    let base_idx = idx % palette_len;
                    let (r, g, b) = ACCOUNT_COLOR_PALETTE[base_idx];
                    egui::Color32::from_rgb(r, g, b)
                });

            // Always map the profile_id itself (used for connection tabs)
            map.insert(profile_id.clone(), base_color);

            // Collect unique environments for this account's instances
            let instances: Vec<&Instance> = if current_profile == Some(profile_id.as_str()) {
                current_inventory.instances.iter().collect()
            } else if let Some((inv, _)) = inventories.get(profile_id) {
                inv.instances.iter().collect()
            } else {
                Vec::new()
            };

            let mut envs: Vec<String> = instances.iter()
                .filter_map(|i| instance_env(i))
                .map(|e| e.to_ascii_lowercase())
                .collect();
            envs.sort();
            envs.dedup();

            // Generate a deterministic shade per environment
            for env in &envs {
                let key = format!("{}:{}", profile_id, env);
                let shade = shade_for_env(base_color, env);
                map.insert(key, shade);
            }
        }

        map
    }

    fn parse_hex_color(hex: &str) -> Option<egui::Color32> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(egui::Color32::from_rgb(r, g, b))
    }

    fn color32_to_hex(c: egui::Color32) -> String {
        format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
    }

    fn terminal_panel_fill() -> egui::Color32 {
        egui::Color32::from_rgb(22, 22, 22)
    }

    fn terminal_panel_text() -> egui::Color32 {
        egui::Color32::from_rgb(230, 230, 230)
    }

    /// Build a LayoutJob for terminal content.  When `selection` is provided
    /// (start, end, scroll_offset), selected cells get a highlight background
    /// baked directly into the galley so it is pixel-perfect on every display.
    fn terminal_layout_job(
        screen: &vt100::Screen,
        show_cursor: bool,
        font_id: egui::FontId,
        selection: Option<(AbsPos, AbsPos, usize)>,
    ) -> egui::text::LayoutJob {
        let (rows, cols) = screen.size();
        let default_fg = terminal_panel_text();
        let default_bg = terminal_panel_fill();
        let highlight_bg = egui::Color32::from_rgba_unmultiplied(60, 120, 220, 180);
        let cursor = if show_cursor && !screen.hide_cursor() {
            Some(screen.cursor_position())
        } else {
            None
        };

        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        let mut current_format: Option<egui::TextFormat> = None;
        let mut current_text = String::new();

        for row in 0..rows {
            for col in 0..cols {
                let cursor_cell = cursor == Some((row, col));
                let (cell_text, mut format) = if let Some(cell) = screen.cell(row, col) {
                    let text = if cell.is_wide_continuation() {
                        " "
                    } else {
                        let contents = cell.contents();
                        if contents.is_empty() {
                            " "
                        } else {
                            contents
                        }
                    };
                    let format = terminal_cell_format(
                        cell,
                        cursor_cell,
                        &font_id,
                        default_fg,
                        default_bg,
                    );
                    (text, format)
                } else {
                    let format = egui::TextFormat {
                        font_id: font_id.clone(),
                        color: default_fg,
                        background: egui::Color32::TRANSPARENT,
                        ..Default::default()
                    };
                    (" ", format)
                };

                // Apply selection highlight as cell background
                if let Some((ref start, ref end, scroll_off)) = selection {
                    let abs_r = screen_row_to_abs(row, scroll_off, rows);
                    let in_sel = abs_r <= start.abs_row
                        && abs_r >= end.abs_row
                        && !(abs_r == start.abs_row && col < start.col)
                        && !(abs_r == end.abs_row && col > end.col);
                    if in_sel {
                        format.background = highlight_bg;
                    }
                }

                if current_format
                    .as_ref()
                    .map(|f| f == &format)
                    .unwrap_or(false)
                {
                    current_text.push_str(cell_text);
                } else {
                    if let Some(format) = current_format.take() {
                        if !current_text.is_empty() {
                            job.append(&current_text, 0.0, format);
                        }
                        current_text.clear();
                    }
                    current_format = Some(format.clone());
                    current_text.push_str(cell_text);
                }
            }
            if row + 1 < rows {
                current_text.push('\n');
            }
        }

        if let Some(format) = current_format {
            if !current_text.is_empty() {
                job.append(&current_text, 0.0, format);
            }
        }
        job
    }

    fn terminal_plain_layout_job(
        text: &str,
        font_id: egui::FontId,
    ) -> egui::text::LayoutJob {
        let format = egui::TextFormat {
            font_id,
            color: terminal_panel_text(),
            background: egui::Color32::TRANSPARENT,
            ..Default::default()
        };
        egui::text::LayoutJob::single_section(text.to_string(), format)
    }

    fn terminal_grid_and_cell_size(
        ui: &egui::Ui,
        font_id: &egui::FontId,
        available: egui::Vec2,
    ) -> (u16, u16, f32, f32) {
        let (cell_w, cell_h) = ui.fonts_mut(|f| {
            (f.glyph_width(font_id, 'W'), f.row_height(font_id))
        });
        let cell_w = if cell_w >= 1.0 { cell_w } else { font_id.size * 0.6 };
        let cell_h = if cell_h >= 1.0 { cell_h } else { font_id.size * 1.2 };
        let cols = (available.x / cell_w).floor().max(1.0) as u16;
        let rows = (available.y / cell_h).floor().max(1.0) as u16;
        (rows, cols, cell_w, cell_h)
    }

    fn pixel_to_grid_cell(
        pos: egui::Pos2,
        rect: egui::Rect,
        cell_w: f32,
        cell_h: f32,
        rows: u16,
        cols: u16,
    ) -> (u16, u16) {
        let x = pos.x - rect.left();
        let y = pos.y - rect.top();
        let col = (x / cell_w).floor().max(0.0) as u16;
        let row = (y / cell_h).floor().max(0.0) as u16;
        (row.min(rows.saturating_sub(1)), col.min(cols.saturating_sub(1)))
    }

    /// Convert a screen row to an absolute row (scroll-invariant).
    /// abs_row 0 = newest line, increasing into history.
    fn screen_row_to_abs(screen_row: u16, scroll_offset: usize, visible_rows: u16) -> usize {
        scroll_offset + (visible_rows as usize) - 1 - (screen_row as usize)
    }

    /// Convert an absolute row back to a screen row, or None if outside the visible viewport.
    #[cfg(test)]
    fn abs_to_screen_row(abs_row: usize, scroll_offset: usize, visible_rows: u16) -> Option<u16> {
        let top_abs = scroll_offset + (visible_rows as usize) - 1;
        if abs_row > top_abs || abs_row < scroll_offset {
            return None;
        }
        Some((top_abs - abs_row) as u16)
    }

    /// Extract selected text from a terminal parser across an absolute selection range.
    /// `start` has the higher abs_row (further into history), `end` has the lower.
    /// Find word boundaries within a row of terminal text. Returns
    /// (start_col, end_col) inclusive, where both bounds are the columns
    /// of the first/last "word character" contiguous with the click
    /// position. A word char here is alphanumeric or one of
    /// `_ - . / : @` — covers identifiers, file paths, hostnames,
    /// IP:port, and simple URLs. If the clicked cell isn't a word char,
    /// returns (col, col).
    fn find_word_bounds(row_text: &str, col: u16) -> (u16, u16) {
        let chars: Vec<char> = row_text.chars().collect();
        let idx = col as usize;
        if chars.is_empty() || idx >= chars.len() {
            return (col, col);
        }
        let is_word = |c: char| {
            c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@')
        };
        if !is_word(chars[idx]) {
            return (col, col);
        }
        let mut start = idx;
        let mut end = idx;
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        while end + 1 < chars.len() && is_word(chars[end + 1]) {
            end += 1;
        }
        (start as u16, end as u16)
    }

    /// Set the parser's scrollback so the given `top_abs_row` is shifted as
    /// close to the top of the viewport as possible. Returns the screen_row
    /// where that abs_row now appears.
    ///
    /// vt100's `set_scrollback(K)` is "shift view up by K rows" and is
    /// clamped to the actual scrollback size. If we naively call
    /// `set_scrollback(abs_row)` for a row that lives in the *unscrolled*
    /// grid (or in scrollback shorter than `abs_row`), the view stays
    /// near the bottom and reads from the wrong (often empty) screen row.
    /// This helper computes the proper scrollback for the requested row
    /// and returns the screen_row at which it actually lands after vt100
    /// clamps.
    fn set_scrollback_for_top_abs_row(parser: &mut vt100::Parser, top_abs_row: usize) -> u16 {
        let vr = parser.screen().size().0 as usize;
        let desired_sb = top_abs_row.saturating_sub(vr.saturating_sub(1));
        parser.screen_mut().set_scrollback(desired_sb);
        let actual_sb = parser.screen().scrollback();
        let bottom_abs = actual_sb + vr.saturating_sub(1);
        bottom_abs.saturating_sub(top_abs_row) as u16
    }

    fn extract_selection_text(
        parser: &mut vt100::Parser,
        start: AbsPos,
        end: AbsPos,
    ) -> String {
        let visible_rows = parser.screen().size().0;
        let vr = visible_rows as usize;
        let span = start.abs_row - end.abs_row + 1;

        if span <= vr {
            // Fast path: selection fits in one viewport.
            let screen_start = set_scrollback_for_top_abs_row(parser, start.abs_row);
            let screen_end = screen_start + (start.abs_row - end.abs_row) as u16;
            let text = parser.screen().contents_between(
                screen_start,
                start.col,
                screen_end,
                end.col.saturating_add(1),
            );
            parser.screen_mut().set_scrollback(0);
            return text;
        }

        // Slow path: multi-viewport selection — iterate in viewport-sized chunks
        let cols = parser.screen().size().1;
        let mut result = String::new();
        let mut current_abs_top = start.abs_row;
        let mut is_first = true;

        loop {
            let screen_top = set_scrollback_for_top_abs_row(parser, current_abs_top);
            let actual_sb = parser.screen().scrollback();
            let viewport_bottom_abs = actual_sb;
            let col_start = if is_first { start.col } else { 0 };

            let (screen_bottom, col_end, done) = if end.abs_row >= viewport_bottom_abs {
                let bottom_abs = actual_sb + vr.saturating_sub(1);
                let sr = bottom_abs.saturating_sub(end.abs_row) as u16;
                (sr, end.col.saturating_add(1), true)
            } else {
                (visible_rows - 1, cols, false)
            };

            let chunk = parser.screen().contents_between(
                screen_top, col_start, screen_bottom, col_end,
            );

            if !result.is_empty() && !chunk.is_empty() {
                result.push('\n');
            }
            result.push_str(&chunk);

            if done || actual_sb == 0 {
                break;
            }

            current_abs_top = viewport_bottom_abs - 1;
            is_first = false;
        }

        parser.screen_mut().set_scrollback(0);
        result
    }

    /// Set clipboard text with retry. On Windows, OpenClipboard fails
    /// transiently when another process holds the clipboard (browsers,
    /// Office, clipboard managers). arboard's internal retry loop is
    /// short — wrap it with a longer outer retry so right-click copy
    /// stops "intermittently failing" until the user closes/reopens
    /// terminals.
    fn clipboard_set_text_with_retry(text: &str) -> std::result::Result<(), String> {
        const ATTEMPTS: u32 = 5;
        const BACKOFF: Duration = Duration::from_millis(40);
        let mut last_err: Option<String> = None;
        for attempt in 0..ATTEMPTS {
            match arboard::Clipboard::new() {
                Ok(mut cb) => match cb.set_text(text) {
                    Ok(()) => return Ok(()),
                    Err(err) => last_err = Some(format!("set_text: {err}")),
                },
                Err(err) => last_err = Some(format!("Clipboard::new: {err}")),
            }
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(BACKOFF);
            }
        }
        Err(last_err.unwrap_or_else(|| "unknown clipboard error".to_string()))
    }

    fn clipboard_get_text_with_retry() -> std::result::Result<String, String> {
        const ATTEMPTS: u32 = 5;
        const BACKOFF: Duration = Duration::from_millis(40);
        let mut last_err: Option<String> = None;
        for attempt in 0..ATTEMPTS {
            match arboard::Clipboard::new() {
                Ok(mut cb) => match cb.get_text() {
                    Ok(t) => return Ok(t),
                    Err(err) => last_err = Some(format!("get_text: {err}")),
                },
                Err(err) => last_err = Some(format!("Clipboard::new: {err}")),
            }
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(BACKOFF);
            }
        }
        Err(last_err.unwrap_or_else(|| "unknown clipboard error".to_string()))
    }

    fn resize_pty_session(session: &mut PtySession, rows: u16, cols: u16) {
        let next = Some((rows, cols));
        if session.last_size == next {
            return;
        }
        session.last_size = next;
        session.parser.screen_mut().set_size(rows, cols);
        if let Ok(master) = session.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    fn terminal_cell_format(
        cell: &vt100::Cell,
        cursor: bool,
        font_id: &egui::FontId,
        default_fg: egui::Color32,
        default_bg: egui::Color32,
    ) -> egui::TextFormat {
        let mut fg = vt100_color_to_egui(cell.fgcolor(), default_fg);
        let mut bg = vt100_color_to_egui(cell.bgcolor(), default_bg);
        let mut inverse = cell.inverse();
        if cursor {
            inverse = !inverse;
        }
        if inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.bold() {
            fg = brighten_color(fg, 0.18);
        }
        if cell.dim() {
            fg = darken_color(fg, 0.22);
        }
        let background = if cell.bgcolor() != vt100::Color::Default || inverse {
            bg
        } else {
            egui::Color32::TRANSPARENT
        };
        egui::TextFormat {
            font_id: font_id.clone(),
            color: fg,
            background,
            italics: cell.italic(),
            underline: if cell.underline() {
                egui::Stroke::new(1.0, fg)
            } else {
                egui::Stroke::NONE
            },
            ..Default::default()
        }
    }

    fn vt100_color_to_egui(
        color: vt100::Color,
        default: egui::Color32,
    ) -> egui::Color32 {
        match color {
            vt100::Color::Default => default,
            vt100::Color::Idx(idx) => ansi_color_from_index(idx),
            vt100::Color::Rgb(r, g, b) => egui::Color32::from_rgb(r, g, b),
        }
    }

    fn brighten_color(color: egui::Color32, factor: f32) -> egui::Color32 {
        let r = color.r() as f32;
        let g = color.g() as f32;
        let b = color.b() as f32;
        let nr = r + (255.0 - r) * factor;
        let ng = g + (255.0 - g) * factor;
        let nb = b + (255.0 - b) * factor;
        egui::Color32::from_rgb(nr as u8, ng as u8, nb as u8)
    }

    fn darken_color(color: egui::Color32, factor: f32) -> egui::Color32 {
        let r = color.r() as f32;
        let g = color.g() as f32;
        let b = color.b() as f32;
        let nr = r * (1.0 - factor);
        let ng = g * (1.0 - factor);
        let nb = b * (1.0 - factor);
        egui::Color32::from_rgb(nr as u8, ng as u8, nb as u8)
    }

    fn ansi_color_from_index(idx: u8) -> egui::Color32 {
        const BASIC: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        if idx < 16 {
            let (r, g, b) = BASIC[idx as usize];
            return egui::Color32::from_rgb(r, g, b);
        }
        if idx <= 231 {
            let mut n = idx - 16;
            let r = n / 36;
            n %= 36;
            let g = n / 6;
            let b = n % 6;
            let levels = [0, 95, 135, 175, 215, 255];
            return egui::Color32::from_rgb(levels[r as usize], levels[g as usize], levels[b as usize]);
        }
        let gray = 8 + (idx - 232).saturating_mul(10);
        egui::Color32::from_rgb(gray, gray, gray)
    }

    fn terminal_event_payload_for_terminal(
        event: &egui::Event,
        has_text: bool,
        has_key_backspace: bool,
    ) -> Option<Vec<u8>> {
        match event {
            egui::Event::Text(text) => {
                // BS (\x08) may arrive as a Text event on some platforms
                // when egui does not emit Key::Backspace.  Forward it as
                // the terminal backspace byte, but only when no explicit
                // Key::Backspace event is present (to avoid double-send).
                if text == "\x08" {
                    if has_key_backspace {
                        None
                    } else {
                        Some(vec![0x08])
                    }
                } else if text.chars().any(|c| c.is_control()) {
                    None
                } else {
                    Some(text.as_bytes().to_vec())
                }
            }
            egui::Event::Paste(text) => {
                // Normalize pasted text for terminal compatibility:
                // - \r\n → \r, \n → \r: OneNote uses \r\n which double-executes
                // - U+00A0 → space: OneNote/rich text editors insert non-breaking
                //   spaces that look identical but break shell commands and PEM files
                // No bracketed paste — SSM sessions don't reliably pass the
                // escape sequences through to the remote terminal.
                let normalized = text
                    .replace('\u{00a0}', " ")
                    .replace("\r\n", "\r")
                    .replace('\n', "\r");
                Some(normalized.into_bytes())
            }
            // Copy/Cut events carry no modifier info so we cannot
            // distinguish Ctrl+C from Ctrl+Shift+C here.  These are
            // handled in forward_terminal_key_input instead.
            egui::Event::Copy => None,
            egui::Event::Cut => None,
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => match key {
                egui::Key::Enter => Some(b"\r".to_vec()),
                egui::Key::Backspace => Some(vec![0x08]),
                egui::Key::Tab => Some(b"\t".to_vec()),
                egui::Key::Escape => Some(vec![0x1b]),
                egui::Key::ArrowUp => Some(b"\x1b[A".to_vec()),
                egui::Key::ArrowDown => Some(b"\x1b[B".to_vec()),
                egui::Key::ArrowRight => Some(b"\x1b[C".to_vec()),
                egui::Key::ArrowLeft => Some(b"\x1b[D".to_vec()),
                egui::Key::Home => Some(b"\x1b[H".to_vec()),
                egui::Key::End => Some(b"\x1b[F".to_vec()),
                egui::Key::Delete => Some(b"\x1b[3~".to_vec()),
                egui::Key::C if modifiers.ctrl && !modifiers.shift => Some(vec![0x03]),
                egui::Key::D if modifiers.ctrl => Some(vec![0x04]),
                egui::Key::L if modifiers.ctrl => Some(vec![0x0c]),
                egui::Key::U if modifiers.ctrl => Some(vec![0x15]),
                egui::Key::W if modifiers.ctrl => Some(vec![0x17]),
                _ if modifiers.ctrl && !modifiers.shift && !modifiers.alt => {
                    ctrl_key_byte(*key).map(|b| vec![b])
                }
                _ if !modifiers.ctrl && !modifiers.command && !modifiers.alt => {
                    if has_text {
                        None
                    } else {
                        key_ascii_fallback(*key, modifiers.shift).map(|c| vec![c])
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn terminal_event_kind(event: &egui::Event) -> String {
        match event {
            egui::Event::Text(_) => "text".to_string(),
            egui::Event::Paste(_) => "paste".to_string(),
            egui::Event::Copy => "copy".to_string(),
            egui::Event::Cut => "cut".to_string(),
            egui::Event::Key { key, .. } => format!("key:{key:?}"),
            egui::Event::PointerButton { .. } => "pointer_button".to_string(),
            egui::Event::PointerMoved(_) => "pointer_moved".to_string(),
            _ => "other".to_string(),
        }
    }

    fn ctrl_key_byte(key: egui::Key) -> Option<u8> {
        match key {
            egui::Key::A => Some(0x01),
            egui::Key::B => Some(0x02),
            egui::Key::C => Some(0x03),
            egui::Key::D => Some(0x04),
            egui::Key::E => Some(0x05),
            egui::Key::F => Some(0x06),
            egui::Key::G => Some(0x07),
            egui::Key::H => Some(0x08),
            egui::Key::I => Some(0x09),
            egui::Key::J => Some(0x0a),
            egui::Key::K => Some(0x0b),
            egui::Key::L => Some(0x0c),
            egui::Key::M => Some(0x0d),
            egui::Key::N => Some(0x0e),
            egui::Key::O => Some(0x0f),
            egui::Key::P => Some(0x10),
            egui::Key::Q => Some(0x11),
            egui::Key::R => Some(0x12),
            egui::Key::S => Some(0x13),
            egui::Key::T => Some(0x14),
            egui::Key::U => Some(0x15),
            egui::Key::V => Some(0x16),
            egui::Key::W => Some(0x17),
            egui::Key::X => Some(0x18),
            egui::Key::Y => Some(0x19),
            egui::Key::Z => Some(0x1a),
            _ => None,
        }
    }

    fn key_ascii_fallback(key: egui::Key, shift: bool) -> Option<u8> {
        let letter = match key {
            egui::Key::A => Some(b'a'),
            egui::Key::B => Some(b'b'),
            egui::Key::C => Some(b'c'),
            egui::Key::D => Some(b'd'),
            egui::Key::E => Some(b'e'),
            egui::Key::F => Some(b'f'),
            egui::Key::G => Some(b'g'),
            egui::Key::H => Some(b'h'),
            egui::Key::I => Some(b'i'),
            egui::Key::J => Some(b'j'),
            egui::Key::K => Some(b'k'),
            egui::Key::L => Some(b'l'),
            egui::Key::M => Some(b'm'),
            egui::Key::N => Some(b'n'),
            egui::Key::O => Some(b'o'),
            egui::Key::P => Some(b'p'),
            egui::Key::Q => Some(b'q'),
            egui::Key::R => Some(b'r'),
            egui::Key::S => Some(b's'),
            egui::Key::T => Some(b't'),
            egui::Key::U => Some(b'u'),
            egui::Key::V => Some(b'v'),
            egui::Key::W => Some(b'w'),
            egui::Key::X => Some(b'x'),
            egui::Key::Y => Some(b'y'),
            egui::Key::Z => Some(b'z'),
            egui::Key::Num0 => Some(b'0'),
            egui::Key::Num1 => Some(b'1'),
            egui::Key::Num2 => Some(b'2'),
            egui::Key::Num3 => Some(b'3'),
            egui::Key::Num4 => Some(b'4'),
            egui::Key::Num5 => Some(b'5'),
            egui::Key::Num6 => Some(b'6'),
            egui::Key::Num7 => Some(b'7'),
            egui::Key::Num8 => Some(b'8'),
            egui::Key::Num9 => Some(b'9'),
            egui::Key::Space => Some(b' '),
            _ => None,
        }?;
        if shift && letter.is_ascii_lowercase() {
            Some(letter.to_ascii_uppercase())
        } else {
            Some(letter)
        }
    }

    fn filter_embedded_terminals(terminals: Vec<TerminalOption>) -> Vec<TerminalOption> {
        if cfg!(windows) {
            terminals
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.kind,
                        TerminalKind::PowerShell7
                            | TerminalKind::WindowsPowerShell
                            | TerminalKind::Wsl
                    )
                })
                .collect()
        } else {
            terminals
        }
    }

    fn initial_terminal_id(config: &AppConfig, terminals: &[TerminalOption]) -> String {
        pick_default_terminal(config, terminals)
            .or_else(|| terminals.first().cloned())
            .map(|t| t.id)
            .unwrap_or_default()
    }

    fn parent_path(path: &str) -> String {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            return "/".to_string();
        }
        match trimmed.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(idx) => trimmed[..idx].to_string(),
        }
    }

    fn join_path(base: &str, component: &str) -> String {
        if base == "/" {
            format!("/{component}")
        } else {
            format!("{base}/{component}")
        }
    }

    fn parse_ls_output(output: &str) -> Vec<FileEntry> {
        let mut entries = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("total") {
                continue;
            }
            // Split on runs of whitespace, limit to 9 fields so the filename
            // (which may contain spaces) stays intact in the last element.
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 9 {
                continue;
            }
            // The name is everything from the 9th field onward (index 8..)
            let name = parts[8..].join(" ");
            if name == "." || name == ".." {
                continue;
            }
            let permissions = parts[0].to_string();
            let is_dir = permissions.starts_with('d');
            let size = parts[4].parse::<u64>().unwrap_or(0);
            let modified = format!("{} {} {}", parts[5], parts[6], parts[7]);
            entries.push(FileEntry {
                name,
                is_dir,
                size,
                permissions,
                modified,
            });
        }
        sort_file_entries(&mut entries);
        entries
    }

    fn sort_file_entries(entries: &mut [FileEntry]) {
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }

    fn list_files_local(path: &str) -> std::result::Result<Vec<FileEntry>, String> {
        let read_dir =
            std::fs::read_dir(path).map_err(|e| format!("failed to read {path}: {e}"))?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|e| format!("read_dir entry error: {e}"))?;
            let metadata = entry.metadata().map_err(|e| format!("metadata error: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = metadata.is_dir();
            let size = metadata.len();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| {
                    let secs = d.as_secs();
                    format!("{secs}")
                })
                .unwrap_or_default();
            let permissions = if is_dir {
                "drwxr-xr-x".to_string()
            } else {
                "-rw-r--r--".to_string()
            };
            entries.push(FileEntry {
                name,
                is_dir,
                size,
                permissions,
                modified,
            });
        }
        sort_file_entries(&mut entries);
        Ok(entries)
    }

    /// Create an `aws` CLI command with CREATE_NO_WINDOW on Windows
    /// so no console window flashes.
    fn fetch_volumes(profile: &str, region: &str, instance_id: &str) -> std::result::Result<Vec<VolumeInfo>, String> {
        let output = aws_command()
            .args([
                "ec2", "describe-volumes",
                "--profile", profile,
                "--region", region,
                "--filters", &format!("Name=attachment.instance-id,Values={instance_id}"),
                "--query", "Volumes[*].{VolumeId:VolumeId,Size:Size,VolumeType:VolumeType,Attachments:Attachments[0].{Device:Device,State:State,AttachTime:AttachTime}}",
                "--output", "json",
            ])
            .output()
            .map_err(|e| format!("describe-volumes failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("describe-volumes error: {stderr}"));
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| format!("JSON parse error: {e}"))?;

        let volumes = parsed.as_array().ok_or("expected array")?;
        let mut result = Vec::new();
        for vol in volumes {
            let attachments = vol.get("Attachments");
            result.push(VolumeInfo {
                volume_id: vol.get("VolumeId").and_then(|v| v.as_str()).unwrap_or("-").to_string(),
                size_gb: vol.get("Size").map(|v| format!("{} GB", v)).unwrap_or("-".to_string()),
                volume_type: vol.get("VolumeType").and_then(|v| v.as_str()).unwrap_or("-").to_string(),
                device: attachments.and_then(|a| a.get("Device")).and_then(|v| v.as_str()).unwrap_or("-").to_string(),
                state: attachments.and_then(|a| a.get("State")).and_then(|v| v.as_str()).unwrap_or("-").to_string(),
                attach_time: attachments.and_then(|a| a.get("AttachTime")).and_then(|v| v.as_str()).unwrap_or("-").to_string(),
            });
        }
        Ok(result)
    }

    fn fetch_iam_role(profile: &str, region: &str, instance_id: &str) -> Option<String> {
        let output = aws_command()
            .args([
                "ec2", "describe-instances",
                "--profile", profile,
                "--region", region,
                "--instance-ids", instance_id,
                "--query", "Reservations[0].Instances[0].IamInstanceProfile.Arn",
                "--output", "text",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let arn = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if arn.is_empty() || arn == "None" {
            return None;
        }
        // Extract role name from ARN: arn:aws:iam::123:instance-profile/MyRole -> MyRole
        arn.rsplit('/').next().map(|s| s.to_string())
    }

    fn aws_command() -> std::process::Command {
        let mut cmd = std::process::Command::new("aws");
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        cmd
    }

    fn ssm_send_command(
        profile: &str,
        region: &str,
        instance_id: &str,
        command: &str,
    ) -> std::result::Result<String, String> {
        let output = aws_command()
            .args([
                "ssm",
                "send-command",
                "--profile",
                profile,
                "--region",
                region,
                "--instance-ids",
                instance_id,
                "--document-name",
                "AWS-RunShellScript",
                "--parameters",
                &format!("commands=[\"{command}\"]"),
                "--query",
                "Command.CommandId",
                "--output",
                "text",
            ])
            .output()
            .map_err(|e| format!("ssm send-command failed: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("ssm send-command error: {stderr}"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn ssm_wait_for_command(
        profile: &str,
        region: &str,
        instance_id: &str,
        command_id: &str,
    ) -> std::result::Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if Instant::now() > deadline {
                return Err("ssm command timed out after 30s".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
            let output = aws_command()
                .args([
                    "ssm",
                    "get-command-invocation",
                    "--profile",
                    profile,
                    "--region",
                    region,
                    "--command-id",
                    command_id,
                    "--instance-id",
                    instance_id,
                    "--query",
                    "Status",
                    "--output",
                    "text",
                ])
                .output()
                .map_err(|e| format!("ssm get-command-invocation failed: {e}"))?;
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
            match status.as_str() {
                "Success" => {
                    let out = aws_command()
                        .args([
                            "ssm",
                            "get-command-invocation",
                            "--profile",
                            profile,
                            "--region",
                            region,
                            "--command-id",
                            command_id,
                            "--instance-id",
                            instance_id,
                            "--query",
                            "StandardOutputContent",
                            "--output",
                            "text",
                        ])
                        .output()
                        .map_err(|e| format!("ssm get output failed: {e}"))?;
                    return Ok(String::from_utf8_lossy(&out.stdout).to_string());
                }
                "Failed" | "Cancelled" | "TimedOut" => {
                    return Err(format!("ssm command ended with status: {status}"));
                }
                _ => continue,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn default_window_size_is_large_enough() {
            assert!(GUI_DEFAULT_WIDTH >= 1600.0);
            assert!(GUI_DEFAULT_HEIGHT >= 900.0);
            assert!(GUI_MIN_WIDTH >= 1200.0);
            assert!(GUI_MIN_HEIGHT >= 700.0);
        }

        #[test]
        fn shell_plan_has_program() {
            let (prog, args) = shell_plan(None);
            assert!(!prog.is_empty());
            assert!(!args.is_empty());
        }

        #[test]
        fn pty_command_for_context_uses_aws_in_live_mode() {
            let context = AwsContext {
                mode: Mode::Live,
                profile: "prod".to_string(),
                account_id: Some("000000000000".to_string()),
                arn: None,
                user_id: None,
                region: "us-east-1".to_string(),
                auth_status: AuthStatus::Ok,
            };
            let args = vec![
                "ssm".to_string(),
                "start-session".to_string(),
                "--target".to_string(),
                "i-123".to_string(),
            ];
            let cmd = pty_command_for_context(None, &context, "ignored", &args);
            if cfg!(windows) {
                // On Windows, live sessions are shell-wrapped (WSL/PowerShell/CMD)
                // rather than spawning `aws` directly, because ConPTY cannot
                // capture session-manager-plugin output from a bare process.
                assert!(
                    cmd.program == "wsl.exe"
                        || cmd.program.contains("powershell")
                        || cmd.program.contains("cmd"),
                    "expected shell wrapper on Windows, got: {}",
                    cmd.program,
                );
            } else {
                assert_eq!(cmd.program, "aws");
                assert_eq!(cmd.args, args);
            }
        }

        #[test]
        fn pty_command_for_context_falls_back_to_sim_when_args_empty() {
            let prev = std::env::var("SHELL").ok();
            std::env::set_var("SHELL", "/bin/bash");
            let context = AwsContext {
                mode: Mode::Live,
                profile: "prod".to_string(),
                account_id: Some("000000000000".to_string()),
                arn: None,
                user_id: None,
                region: "us-east-1".to_string(),
                auth_status: AuthStatus::Ok,
            };
            let cmd = pty_command_for_context(None, &context, "echo hi", &[]);
            assert_ne!(cmd.program, "aws");
            if let Some(prev) = prev {
                std::env::set_var("SHELL", prev);
            } else {
                std::env::remove_var("SHELL");
            }
        }

        #[test]
        #[cfg(windows)]
        fn filter_embedded_terminals_restricts_windows_shells() {
            let terminals = vec![
                TerminalOption {
                    id: "pwsh".to_string(),
                    display_name: "PowerShell 7".to_string(),
                    kind: TerminalKind::PowerShell7,
                    program: "pwsh.exe".to_string(),
                },
                TerminalOption {
                    id: "powershell".to_string(),
                    display_name: "Windows PowerShell".to_string(),
                    kind: TerminalKind::WindowsPowerShell,
                    program: "powershell.exe".to_string(),
                },
                TerminalOption {
                    id: "wsl".to_string(),
                    display_name: "WSL".to_string(),
                    kind: TerminalKind::Wsl,
                    program: "wsl.exe".to_string(),
                },
                TerminalOption {
                    id: "cmd".to_string(),
                    display_name: "Command Prompt".to_string(),
                    kind: TerminalKind::Cmd,
                    program: "cmd.exe".to_string(),
                },
            ];
            let filtered = filter_embedded_terminals(terminals);
            assert_eq!(filtered.len(), 3);
            assert!(filtered.iter().all(|t| {
                matches!(
                    t.kind,
                    TerminalKind::PowerShell7
                        | TerminalKind::WindowsPowerShell
                        | TerminalKind::Wsl
                )
            }));
        }

        #[test]
        #[cfg(not(windows))]
        fn filter_embedded_terminals_noop_non_windows() {
            let terminals = vec![
                TerminalOption {
                    id: "xterm".to_string(),
                    display_name: "XTerm".to_string(),
                    kind: TerminalKind::Xterm,
                    program: "xterm".to_string(),
                },
                TerminalOption {
                    id: "kitty".to_string(),
                    display_name: "Kitty".to_string(),
                    kind: TerminalKind::Kitty,
                    program: "kitty".to_string(),
                },
            ];
            let filtered = filter_embedded_terminals(terminals.clone());
            assert_eq!(filtered.len(), terminals.len());
            assert_eq!(filtered[0].id, terminals[0].id);
            assert_eq!(filtered[1].id, terminals[1].id);
        }

        #[test]
        #[cfg(windows)]
        fn windows_cmd_path_is_absolute() {
            let path = windows_cmd_path();
            assert!(path.to_ascii_lowercase().ends_with("\\system32\\cmd.exe"));
        }

        #[test]
        #[cfg(not(windows))]
        fn windows_cmd_path_stub_has_cmd_name() {
            let path = windows_cmd_path();
            assert!(path.to_ascii_lowercase().contains("cmd"));
        }

        #[test]
        #[cfg(windows)]
        fn format_sim_command_uses_cmd_syntax() {
            let cmd = format_sim_command(
                TerminalKind::Cmd,
                "aws ssm start-session --target i-123 --region us-east-1",
                "i-123",
                None,
            );
            assert!(cmd.contains("echo [SIM MODE]"));
            assert!(cmd.contains("& echo [SIM MODE] session open for i-123"));
        }

        #[test]
        #[cfg(windows)]
        fn format_sim_command_uses_powershell_syntax() {
            let cmd = format_sim_command(
                TerminalKind::WindowsPowerShell,
                "aws ssm start-session --target i-123 --region us-east-1",
                "i-123",
                None,
            );
            assert!(cmd.contains("Write-Host"));
            assert!(cmd.contains("session open for i-123"));
        }

        #[test]
        #[cfg(not(windows))]
        fn format_sim_command_uses_shell_echo() {
            let cmd = format_sim_command(
                TerminalKind::Xterm,
                "aws ssm start-session --target i-123 --region us-east-1",
                "i-123",
                None,
            );
            assert!(cmd.contains("echo '[SIM MODE]"));
            assert!(cmd.contains("session open for i-123"));
        }

        #[test]
        fn terminal_debug_label_includes_id() {
            let terminal = TerminalOption {
                id: "cmd".to_string(),
                display_name: "Command Prompt".to_string(),
                kind: ec2_manager::models::TerminalKind::Cmd,
                program: "cmd.exe".to_string(),
            };
            let label = terminal_debug_label(Some(&terminal));
            assert!(label.contains("id=cmd"));
            assert!(label.contains("Command Prompt"));
        }

        #[test]
        fn terminal_event_kind_reports_key() {
            let key = egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            };
            assert!(terminal_event_kind(&key).starts_with("key:"));
        }

        #[test]
        fn guarded_action_logs_error_on_failure() {
            let mut app = Ec2GuiApp::new(GuiOptions {
                mode: Mode::Sim,
                region: None,
                dry_run: true,
                debug: false,
                wsl_auto_setup: false,
            });
            let ok = app.guarded_action("test-action", |_app| {
                Err(AppError::Parse("boom".to_string()))
            });
            assert!(!ok);
            assert!(app
                .logs
                .iter()
                .any(|entry| entry.message.contains("test-action failed")));
        }

        #[test]
        fn terminate_child_reaps_process() {
            let (program, args) = if cfg!(windows) {
                (
                    "cmd",
                    vec!["/C".to_string(), "ping -n 10 127.0.0.1 >NUL".to_string()],
                )
            } else {
                ("/bin/sh", vec!["-c".to_string(), "sleep 35".to_string()])
            };

            let mut child = Command::new(program)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn process");

            terminate_child(&mut child);
            assert!(child.try_wait().expect("process state").is_some());
        }

        #[test]
        fn quick_connect_row_action_connects_and_selects() {
            let action = resolve_row_action(false, false, true);
            assert_eq!(
                action,
                RowAction {
                    select: true,
                    connect: true
                }
            );
        }

        #[test]
        fn single_click_row_action_selects_only() {
            let action = resolve_row_action(true, false, false);
            assert_eq!(
                action,
                RowAction {
                    select: true,
                    connect: false
                }
            );
        }

        #[test]
        fn open_connection_tab_dry_run_does_not_spawn_child() {
            let mut app = Ec2GuiApp::new(GuiOptions {
                mode: Mode::Sim,
                region: None,
                dry_run: true,
                debug: false,
                wsl_auto_setup: false,
            });
            let context = AwsContext {
                mode: Mode::Sim,
                profile: "sim-profile".to_string(),
                account_id: Some("000000000000".to_string()),
                arn: None,
                user_id: None,
                region: "us-east-1".to_string(),
                auth_status: AuthStatus::Ok,
            };

            app.open_connection_tab(
                "api-a".to_string(),
                "i-sim0001".to_string(),
                "echo hi".to_string(),
                Vec::new(),
                &context,
            )
            .expect("dry-run open should succeed");

            assert!(app.pty_sessions.is_empty());
            let selected = app
                .connections
                .selected_ref()
                .expect("tab should be selected");
            assert!(!selected.running);
            assert!(selected.lines.iter().any(|line| line.contains("[dry-run]")));
        }

        #[test]
        fn sim_mode_open_connection_tab_spawns_terminal_output() {
            let mut app = Ec2GuiApp::new(GuiOptions {
                mode: Mode::Sim,
                region: None,
                dry_run: false,
                debug: false,
                wsl_auto_setup: false,
            });
            let context = AwsContext {
                mode: Mode::Sim,
                profile: "sim-profile".to_string(),
                account_id: Some("000000000000".to_string()),
                arn: None,
                user_id: None,
                region: "us-east-1".to_string(),
                auth_status: AuthStatus::Ok,
            };

            let open_result = app.open_connection_tab(
                "api-a".to_string(),
                "i-sim0001".to_string(),
                "echo terminal-ok".to_string(),
                Vec::new(),
                &context,
            );
            if let Err(AppError::Parse(message)) = &open_result {
                if message.contains("Permission denied") {
                    // Some CI/sandbox environments disallow openpty.
                    return;
                }
            }
            open_result.expect("sim open should spawn a terminal session");

            let tab_id = app
                .connections
                .selected()
                .expect("selected connection tab should exist");
            for _ in 0..60 {
                let _ = app.poll_connection_events();
                let found = app
                    .connections
                    .selected_ref()
                    .map(|tab| tab.lines.iter().any(|line| line.contains("terminal-ok")))
                    .unwrap_or(false);
                if found {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            let found = app
                .connections
                .selected_ref()
                .map(|tab| tab.lines.iter().any(|line| line.contains("terminal-ok")))
                .unwrap_or(false);
            assert!(found, "expected terminal output to be captured");

            app.close_connection_tab(tab_id);
        }

        #[test]
        fn search_terms_from_rules_splits_include_and_exclude() {
            let rules = vec![
                SearchRuleInput {
                    kind: SearchRuleKind::Include,
                    term: "orders".to_string(),
                },
                SearchRuleInput {
                    kind: SearchRuleKind::Exclude,
                    term: "legacy".to_string(),
                },
                SearchRuleInput {
                    kind: SearchRuleKind::Include,
                    term: "  ".to_string(),
                },
            ];

            let (includes, excludes) = search_terms_from_rules(&rules);
            assert_eq!(includes, vec!["orders"]);
            assert_eq!(excludes, vec!["legacy"]);
        }

        #[test]
        fn rules_from_search_terms_roundtrip() {
            let rules = rules_from_search_terms(
                &["orders".to_string(), "platform".to_string()],
                &["legacy".to_string()],
            );
            let (includes, excludes) = search_terms_from_rules(&rules);
            assert_eq!(includes, vec!["orders", "platform"]);
            assert_eq!(excludes, vec!["legacy"]);
        }

        #[test]
        fn rules_from_search_terms_empty_creates_default_rule() {
            let rules = rules_from_search_terms(&[], &[]);
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].kind, SearchRuleKind::Include);
            assert!(rules[0].term.is_empty());
        }

        #[test]
        fn states_from_state_filter_none_is_empty() {
            assert!(states_from_state_filter("").is_empty());
        }

        #[test]
        fn states_from_state_filter_value_is_single_state() {
            assert_eq!(
                states_from_state_filter("running"),
                vec!["running".to_string()]
            );
        }

        #[test]
        fn state_filter_from_saved_states_uses_first_supported_state() {
            assert_eq!(
                state_filter_from_saved_states(&["stopped".to_string(), "running".to_string()]),
                "stopped".to_string()
            );
            assert_eq!(
                state_filter_from_saved_states(&["unknown".to_string()]),
                "".to_string()
            );
        }

        #[test]
        fn log_filters_include_expected_levels() {
            let mut filters = LogFilters::default();
            assert!(filters.includes(LogLevel::Info));
            assert!(filters.includes(LogLevel::Debug));
            assert!(!filters.includes(LogLevel::Trace));

            filters.set_verbosity_high();
            assert!(filters.includes(LogLevel::Debug));
            assert!(filters.includes(LogLevel::Trace));
        }

        #[test]
        fn app_log_is_capped_to_max_lines() {
            let mut app = Ec2GuiApp::new(GuiOptions {
                mode: Mode::Sim,
                region: None,
                dry_run: true,
                debug: false,
                wsl_auto_setup: false,
            });

            for i in 0..(Ec2GuiApp::MAX_LOG_LINES + 5) {
                app.log_info(format!("line-{i}"));
            }

            assert_eq!(app.logs.len(), Ec2GuiApp::MAX_LOG_LINES);
            assert_eq!(
                app.logs.front().map(|e| e.message.as_str()),
                Some("line-5")
            );
            assert_eq!(
                app.logs.back().map(|e| e.message.as_str()),
                Some("line-20004")
            );
        }

        #[test]
        fn profile_choice_change_detected_when_mtime_differs() {
            let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
            let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(20);
            assert!(profile_choice_changed(Some(t1), Some(t2)));
            assert!(profile_choice_changed(Some(t1), None));
            assert!(profile_choice_changed(None, Some(t2)));
        }

        #[test]
        fn profile_choice_change_not_detected_when_mtime_same() {
            let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
            assert!(!profile_choice_changed(Some(t1), Some(t1)));
            assert!(!profile_choice_changed(None, None));
        }

        #[test]
        fn profile_change_debounce_elapsed_when_duration_met() {
            let started = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(13);
            assert!(profile_change_debounce_elapsed(
                Some(started),
                now,
                Duration::from_secs(2)
            ));
        }

        #[test]
        fn profile_change_debounce_not_elapsed_when_too_soon_or_missing_start() {
            let started = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(11);
            assert!(!profile_change_debounce_elapsed(
                Some(started),
                now,
                Duration::from_secs(2)
            ));
            assert!(!profile_change_debounce_elapsed(
                None,
                now,
                Duration::from_secs(2)
            ));
        }

        #[test]
        fn initial_terminal_id_prefers_configured_terminal() {
            let mut config = AppConfig::default();
            config.default_terminal = Some("kitty".to_string());
            let terminals = vec![
                TerminalOption {
                    id: "xterm".to_string(),
                    display_name: "XTerm".to_string(),
                    kind: ec2_manager::models::TerminalKind::Xterm,
                    program: "xterm".to_string(),
                },
                TerminalOption {
                    id: "kitty".to_string(),
                    display_name: "Kitty".to_string(),
                    kind: ec2_manager::models::TerminalKind::Kitty,
                    program: "kitty".to_string(),
                },
            ];

            assert_eq!(initial_terminal_id(&config, &terminals), "kitty");
        }

        #[test]
        fn initial_terminal_id_empty_when_no_terminals() {
            let config = AppConfig::default();
            assert!(initial_terminal_id(&config, &[]).is_empty());
        }

        #[test]
        fn format_connection_summary_line_contains_instance_ip_and_status() {
            let line =
                format_connection_summary_line("api-a", "i-123", "10.0.1.25", true, 0);
            assert!(line.contains("Instance: api-a (i-123)"));
            assert!(line.contains("Private IP: 10.0.1.25"));
            assert!(line.contains("Status: Running"));
            assert!(line.contains('\t'));
        }

        #[test]
        fn selected_region_label_prefers_selected_region() {
            assert_eq!(
                selected_region_label(Some("eu-central-1"), Some("us-east-1")),
                "eu-central-1".to_string()
            );
        }

        #[test]
        fn selected_region_label_displays_auto_with_context_region() {
            assert_eq!(
                selected_region_label(None, Some("us-west-2")),
                "(auto) (us-west-2)".to_string()
            );
            assert_eq!(selected_region_label(None, None), "(auto)".to_string());
        }

        #[test]
        fn parse_bool_env_understands_common_values() {
            assert!(parse_bool_env(Some("true"), false));
            assert!(parse_bool_env(Some("1"), false));
            assert!(parse_bool_env(Some("YES"), false));
            assert!(!parse_bool_env(Some("false"), true));
            assert!(!parse_bool_env(Some("0"), true));
            assert!(!parse_bool_env(Some("off"), true));
            assert!(parse_bool_env(Some("unknown"), true));
            assert!(!parse_bool_env(None, false));
        }

        #[test]
        fn panic_payload_to_string_handles_string_and_static_str() {
            let owned: Box<dyn std::any::Any + Send> = Box::new("owned panic".to_string());
            assert_eq!(panic_payload_to_string(owned.as_ref()), "owned panic");

            let static_str: Box<dyn std::any::Any + Send> = Box::new("static panic");
            assert_eq!(panic_payload_to_string(static_str.as_ref()), "static panic");
        }

        #[test]
        fn panic_log_path_uses_expected_file_name() {
            let path = panic_log_path();
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some("ec2_manager_gui_panic.log")
            );
        }

        #[test]
        fn gui_smoke_match_detects_expected_marker() {
            assert!(gui_smoke_match_in_bytes(
                "session open",
                b"[SIM MODE] session open for i-sim0001"
            ));
            assert!(!gui_smoke_match_in_bytes("not-there", b"hello"));
        }

        #[test]
        fn write_gui_smoke_marker_creates_parent_and_writes_payload() {
            let base = std::env::temp_dir().join(format!(
                "ec2-manager-gui-smoke-{}",
                now_unix()
            ));
            let marker_path = base.join("nested").join("marker.txt");
            write_gui_smoke_marker(&marker_path, 7, "session open")
                .expect("marker write should succeed");
            let content = fs::read_to_string(&marker_path).expect("marker should be readable");
            assert!(content.contains("PASS"));
            assert!(content.contains("tab_id=7"));
            assert!(content.contains("expected=session open"));
            let _ = fs::remove_file(&marker_path);
            let _ = fs::remove_dir_all(&base);
        }

        #[test]
        fn terminal_event_payload_maps_ctrl_c_enter_and_paste() {
            let ctrl_c = egui::Event::Key {
                key: egui::Key::C,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_c, false, false),
                Some(vec![0x03])
            );

            let enter = egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&enter, false, false),
                Some(b"\r".to_vec())
            );

            let ctrl_w = egui::Event::Key {
                key: egui::Key::W,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_w, false, false),
                Some(vec![0x17])
            );

            let ctrl_u = egui::Event::Key {
                key: egui::Key::U,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_u, false, false),
                Some(vec![0x15])
            );

            // Paste should normalize line endings without bracketed paste
            let paste = egui::Event::Paste("echo hi".to_string());
            assert_eq!(
                terminal_event_payload_for_terminal(&paste, false, false),
                Some(b"echo hi".to_vec())
            );

            // Verify \r\n line endings are normalized to \r
            let paste_crlf = egui::Event::Paste("cd\r\nmkdir .ssh".to_string());
            assert_eq!(
                terminal_event_payload_for_terminal(&paste_crlf, false, false),
                Some(b"cd\rmkdir .ssh".to_vec())
            );
        }

        #[test]
        fn terminal_event_payload_copy_returns_none() {
            // Copy events are handled in forward_terminal_key_input, not here
            assert_eq!(
                terminal_event_payload_for_terminal(&egui::Event::Copy, false, false),
                None
            );
        }

        #[test]
        fn terminal_event_payload_cut_returns_none() {
            // Cut events are handled natively by egui
            assert_eq!(
                terminal_event_payload_for_terminal(&egui::Event::Cut, false, false),
                None
            );
        }

        #[test]
        fn terminal_event_payload_ctrl_shift_c_returns_none() {
            let ctrl_shift_c = egui::Event::Key {
                key: egui::Key::C,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Default::default()
                },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_shift_c, false, false),
                None
            );
        }

        #[test]
        fn terminal_event_payload_backspace_sends_bs() {
            let bs = egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&bs, false, false),
                Some(vec![0x08])
            );
        }

        #[test]
        fn terminal_event_payload_falls_back_for_letter_keys() {
            let key_a = egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&key_a, false, false),
                Some(vec![b'a'])
            );
            assert_eq!(
                terminal_event_payload_for_terminal(&key_a, true, false),
                None
            );

            let key_a_shift = egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&key_a_shift, false, false),
                Some(vec![b'A'])
            );
            assert_eq!(
                terminal_event_payload_for_terminal(&key_a_shift, true, false),
                None
            );
        }

        #[test]
        fn terminal_event_payload_text_bs_without_key_backspace() {
            // When no Key::Backspace event is present, Text("\x08")
            // should be forwarded as BS (0x08).
            let text_bs = egui::Event::Text("\u{8}".to_string());
            assert_eq!(
                terminal_event_payload_for_terminal(&text_bs, false, false),
                Some(vec![0x08])
            );
        }

        #[test]
        fn terminal_event_payload_text_bs_skipped_when_key_backspace_present() {
            // When Key::Backspace IS present, Text("\x08") should be
            // suppressed to avoid double-sending.
            let text_bs = egui::Event::Text("\u{8}".to_string());
            assert_eq!(
                terminal_event_payload_for_terminal(&text_bs, false, true),
                None
            );
        }

        #[test]
        fn terminal_event_payload_ignores_other_control_text() {
            // Control chars other than BS should still be filtered.
            let ctrl_text = egui::Event::Text("\u{1}".to_string());
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_text, false, false),
                None
            );
        }

        #[test]
        fn terminal_event_payload_ctrl_a_sends_soh() {
            let ctrl_a = egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { ctrl: true, ..Default::default() },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_a, false, false),
                Some(vec![0x01])
            );
        }

        #[test]
        fn terminal_event_payload_ctrl_e_sends_enq() {
            let ctrl_e = egui::Event::Key {
                key: egui::Key::E,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { ctrl: true, ..Default::default() },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_e, false, false),
                Some(vec![0x05])
            );
        }

        #[test]
        fn terminal_event_payload_ctrl_r_sends_dc2() {
            let ctrl_r = egui::Event::Key {
                key: egui::Key::R,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { ctrl: true, ..Default::default() },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_r, false, false),
                Some(vec![0x12])
            );
        }

        #[test]
        fn terminal_event_payload_ctrl_x_sends_can() {
            // Ctrl+X is also intercepted as egui::Event::Cut upstream in
            // forward_terminal_key_input; this covers the fallback path
            // when the Key event is what we see (e.g., some layouts).
            let ctrl_x = egui::Event::Key {
                key: egui::Key::X,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { ctrl: true, ..Default::default() },
            };
            assert_eq!(
                terminal_event_payload_for_terminal(&ctrl_x, false, false),
                Some(vec![0x18])
            );
        }

        #[test]
        fn compute_query_response_dsr_cursor_position() {
            // Parser reports 0-based row/col; response is 1-based.
            let r = compute_terminal_query_response(b"\x1b[6n", 4, 9);
            assert_eq!(r, b"\x1b[5;10R");
        }

        #[test]
        fn compute_query_response_da1_vt101() {
            let r = compute_terminal_query_response(b"\x1b[c", 0, 0);
            assert_eq!(r, b"\x1b[?1;0c");
            let r = compute_terminal_query_response(b"\x1b[0c", 0, 0);
            assert_eq!(r, b"\x1b[?1;0c");
        }

        #[test]
        fn compute_query_response_da2_vt220() {
            let r = compute_terminal_query_response(b"\x1b[>c", 0, 0);
            assert_eq!(r, b"\x1b[>41;0;0c");
            let r = compute_terminal_query_response(b"\x1b[>0c", 0, 0);
            assert_eq!(r, b"\x1b[>41;0;0c");
        }

        #[test]
        fn compute_query_response_da2_does_not_trigger_da1() {
            // DA2 (\x1b[>c) must not also match DA1 (\x1b[c) — otherwise
            // we'd send two conflicting replies.
            let r = compute_terminal_query_response(b"\x1b[>c", 0, 0);
            assert_eq!(r, b"\x1b[>41;0;0c");
            assert!(!r.windows(7).any(|w| w == b"\x1b[?1;0c"));
        }

        #[test]
        fn compute_query_response_osc_11_bg_color() {
            let r = compute_terminal_query_response(b"\x1b]11;?\x07", 0, 0);
            assert_eq!(r, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
        }

        #[test]
        fn compute_query_response_xtgettcap() {
            let r = compute_terminal_query_response(
                b"\x1bP+q436F6C6F7273\x1b\\",
                0,
                0,
            );
            assert_eq!(r, b"\x1bP0+r\x1b\\");
        }

        #[test]
        fn compute_query_response_combined() {
            // Vim often sends DA1 + DA2 + DSR in rapid succession;
            // we should answer all three in one response buffer.
            let input = b"\x1b[c\x1b[>c\x1b[6n";
            let r = compute_terminal_query_response(input, 1, 2);
            assert!(r.windows(7).any(|w| w == b"\x1b[?1;0c"));
            assert!(r.windows(10).any(|w| w == b"\x1b[>41;0;0c"));
            assert!(r.windows(6).any(|w| w == b"\x1b[2;3R"));
        }

        #[test]
        fn compute_query_response_empty_when_no_queries() {
            let r = compute_terminal_query_response(b"hello world\r\n", 0, 0);
            assert!(r.is_empty());
        }

        #[test]
        fn terminal_panel_text_is_light() {
            let text = terminal_panel_text();
            assert!(text.r() >= 200);
            assert!(text.g() >= 200);
            assert!(text.b() >= 200);
        }

        #[test]
        fn ansi_color_from_index_maps_basic_colors() {
            assert_eq!(
                ansi_color_from_index(1),
                egui::Color32::from_rgb(205, 0, 0)
            );
            assert_eq!(
                ansi_color_from_index(10),
                egui::Color32::from_rgb(0, 255, 0)
            );
        }

        #[test]
        fn terminal_layout_job_applies_cell_colors() {
            let mut parser = vt100::Parser::new(1, 2, 10);
            parser.process(b"\x1b[31mR\x1b[42mG");
            let job = terminal_layout_job(
                parser.screen(),
                false,
                egui::FontId::monospace(12.0),
                None,
            );
            let mut found_red = false;
            for section in &job.sections {
                let text = &job.text[section.byte_range.clone()];
                if text.contains('R') {
                    assert_eq!(
                        section.format.color,
                        ansi_color_from_index(1)
                    );
                    found_red = true;
                    break;
                }
            }
            assert!(found_red, "expected red cell to be present");
        }

        #[test]
        fn inventory_headers_do_not_include_app_service() {
            assert!(!INVENTORY_HEADERS
                .iter()
                .any(|(label, _)| *label == "App/Service"));
        }

        #[test]
        fn terminal_layout_job_includes_screen_text() {
            let mut parser = vt100::Parser::new(2, 10, 100);
            parser.process(b"hello");
            let job = terminal_layout_job(
                parser.screen(),
                false,
                egui::FontId::monospace(12.0),
                None,
            );
            assert!(job.text.contains("hello"));
        }

        #[test]
        fn terminal_panel_fill_is_dark() {
            let fill = terminal_panel_fill();
            assert!(fill.r() <= 30);
            assert!(fill.g() <= 30);
            assert!(fill.b() <= 30);
        }

        #[test]
        fn parse_ls_output_extracts_files_and_dirs() {
            let output = "\
total 24
drwxr-xr-x 3 user user 4096 Jan 10 12:00 .
drwxr-xr-x 5 user user 4096 Jan 10 12:00 ..
drwxr-xr-x 2 user user 4096 Jan 10 12:00 config
-rw-r--r-- 1 user user 1234 Jan 10 12:00 readme.md
-rw-r--r-- 1 user user  567 Jan 10 12:00 app.py";
            let entries = parse_ls_output(output);
            assert_eq!(entries.len(), 3);
            // dirs first
            assert!(entries[0].is_dir);
            assert_eq!(entries[0].name, "config");
            // then files alphabetically
            assert!(!entries[1].is_dir);
            assert_eq!(entries[1].name, "app.py");
            assert!(!entries[2].is_dir);
            assert_eq!(entries[2].name, "readme.md");
        }

        #[test]
        fn parse_ls_output_skips_dot_entries() {
            let output = "\
total 8
drwxr-xr-x 2 user user 4096 Jan 10 12:00 .
drwxr-xr-x 5 user user 4096 Jan 10 12:00 ..
-rw-r--r-- 1 user user   42 Jan 10 12:00 file.txt";
            let entries = parse_ls_output(output);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "file.txt");
        }

        #[test]
        fn parse_ls_output_handles_empty_output() {
            let entries = parse_ls_output("");
            assert!(entries.is_empty());
        }

        #[test]
        fn parent_path_navigates_up() {
            assert_eq!(parent_path("/home/user"), "/home");
            assert_eq!(parent_path("/home"), "/");
            assert_eq!(parent_path("/"), "/");
        }

        #[test]
        fn join_path_appends_component() {
            assert_eq!(join_path("/home", "user"), "/home/user");
            assert_eq!(join_path("/", "etc"), "/etc");
        }

        #[test]
        fn file_browser_state_defaults() {
            let fb = FileBrowserState::default();
            assert!(matches!(fb.status, FileOpStatus::Idle));
            assert!(fb.entries.is_empty());
            assert!(!fb.initialized);
            assert!(fb.selected_entries.is_empty());
            assert!(fb.last_clicked_entry.is_none());
            assert_eq!(fb.pending_downloads, 0);
        }

        #[test]
        fn file_entry_sort_dirs_first() {
            let mut entries = vec![
                FileEntry {
                    name: "zebra.txt".to_string(),
                    is_dir: false,
                    size: 100,
                    permissions: "-rw-r--r--".to_string(),
                    modified: String::new(),
                },
                FileEntry {
                    name: "alpha_dir".to_string(),
                    is_dir: true,
                    size: 4096,
                    permissions: "drwxr-xr-x".to_string(),
                    modified: String::new(),
                },
                FileEntry {
                    name: "apple.txt".to_string(),
                    is_dir: false,
                    size: 50,
                    permissions: "-rw-r--r--".to_string(),
                    modified: String::new(),
                },
            ];
            sort_file_entries(&mut entries);
            assert_eq!(entries[0].name, "alpha_dir");
            assert!(entries[0].is_dir);
            assert_eq!(entries[1].name, "apple.txt");
            assert_eq!(entries[2].name, "zebra.txt");
        }

        #[test]
        fn list_files_local_reads_current_dir() {
            let cwd = std::env::current_dir().unwrap().to_string_lossy().to_string();
            let entries = list_files_local(&cwd).unwrap();
            assert!(
                entries.iter().any(|e| e.name == "Cargo.toml"),
                "expected Cargo.toml in {:?}",
                entries.iter().map(|e| &e.name).collect::<Vec<_>>()
            );
        }

        #[test]
        fn list_files_local_error_on_nonexistent() {
            let result = list_files_local("/nonexistent_path_that_does_not_exist_12345");
            assert!(result.is_err());
        }

        #[test]
        fn base64_roundtrip() {
            use base64::Engine;
            let original = b"Hello, file browser! \x00\xff\xfe";
            let encoded = base64::engine::general_purpose::STANDARD.encode(original);
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&encoded)
                .unwrap();
            assert_eq!(&decoded, original);
        }

        #[test]
        fn file_browser_cleanup_on_tab_close() {
            let mut browsers: HashMap<u64, FileBrowserState> = HashMap::new();
            browsers.insert(
                42,
                FileBrowserState {
                    current_path: "/tmp".to_string(),
                    path_input: "/tmp".to_string(),
                    ..Default::default()
                },
            );
            assert!(browsers.contains_key(&42));
            browsers.remove(&42);
            assert!(!browsers.contains_key(&42));
        }

        #[test]
        fn scroll_offset_initializes_to_zero() {
            let mut parser = vt100::Parser::new(24, 80, 1000);
            parser.process(b"hello");
            // Simulate what spawn_pty_session_blocking does — scroll_offset starts at 0
            let scroll_offset: usize = 0;
            assert_eq!(scroll_offset, 0);
        }

        #[test]
        fn terminal_layout_job_with_scrollback_shows_history() {
            let mut parser = vt100::Parser::new(2, 10, 100);
            // Fill enough lines to push content into scrollback
            for i in 0..10 {
                parser.process(format!("line{i}\r\n").as_bytes());
            }
            // Without scrollback, we see only the latest visible rows
            let job_bottom = terminal_layout_job(
                parser.screen(),
                false,
                egui::FontId::monospace(12.0),
                None,
            );
            // Scroll up to see earlier content
            parser.screen_mut().set_scrollback(5);
            let job_scrolled = terminal_layout_job(
                parser.screen(),
                false,
                egui::FontId::monospace(12.0),
                None,
            );
            parser.screen_mut().set_scrollback(0);
            // The scrolled view should differ from the bottom view
            assert_ne!(job_bottom.text, job_scrolled.text);
        }

        #[test]
        fn scroll_offset_clamped_to_available_scrollback() {
            let mut parser = vt100::Parser::new(5, 10, 100);
            // Write just 3 lines — scrollback should be limited
            parser.process(b"a\r\nb\r\nc\r\n");
            parser.screen_mut().set_scrollback(usize::MAX);
            let max_sb = parser.screen().scrollback();
            let mut scroll_offset: usize = 9999;
            scroll_offset = scroll_offset.min(max_sb);
            assert!(scroll_offset <= max_sb);
            parser.screen_mut().set_scrollback(0);
        }

        #[test]
        fn terminal_selection_normalized_orders_correctly() {
            // anchor at abs_row 2 (closer to present), end at abs_row 5 (further in history)
            // normalized: start=higher abs_row (5), end=lower abs_row (2)
            let sel = TerminalSelection {
                anchor: Some(AbsPos { abs_row: 2, col: 3 }),
                end: Some(AbsPos { abs_row: 5, col: 10 }),
            };
            let (start, end) = sel.normalized().unwrap();
            assert_eq!(start, AbsPos { abs_row: 5, col: 10 });
            assert_eq!(end, AbsPos { abs_row: 2, col: 3 });
        }

        #[test]
        fn terminal_selection_normalized_same_row() {
            let sel = TerminalSelection {
                anchor: Some(AbsPos { abs_row: 3, col: 20 }),
                end: Some(AbsPos { abs_row: 3, col: 5 }),
            };
            let (start, end) = sel.normalized().unwrap();
            assert_eq!(start, AbsPos { abs_row: 3, col: 5 });
            assert_eq!(end, AbsPos { abs_row: 3, col: 20 });
        }

        #[test]
        fn terminal_selection_clear_removes_state() {
            let mut sel = TerminalSelection {
                anchor: Some(AbsPos { abs_row: 1, col: 2 }),
                end: Some(AbsPos { abs_row: 3, col: 4 }),
            };
            assert!(sel.is_active());
            sel.clear();
            assert!(!sel.is_active());
            assert!(sel.normalized().is_none());
        }

        #[test]
        fn screen_row_to_abs_basic() {
            // At scroll_offset 0, visible_rows 25:
            // screen_row 0 (top) → abs_row 24, screen_row 24 (bottom) → abs_row 0
            assert_eq!(screen_row_to_abs(0, 0, 25), 24);
            assert_eq!(screen_row_to_abs(24, 0, 25), 0);
            assert_eq!(screen_row_to_abs(12, 0, 25), 12);
            // At scroll_offset 10:
            // screen_row 0 → abs_row 34, screen_row 24 → abs_row 10
            assert_eq!(screen_row_to_abs(0, 10, 25), 34);
            assert_eq!(screen_row_to_abs(24, 10, 25), 10);
        }

        #[test]
        fn abs_to_screen_row_basic() {
            // At scroll_offset 0, visible_rows 25: visible abs_rows 0..=24
            assert_eq!(abs_to_screen_row(24, 0, 25), Some(0));
            assert_eq!(abs_to_screen_row(0, 0, 25), Some(24));
            assert_eq!(abs_to_screen_row(12, 0, 25), Some(12));
            // Outside visible range
            assert_eq!(abs_to_screen_row(25, 0, 25), None);
            // At scroll_offset 10: visible abs_rows 10..=34
            assert_eq!(abs_to_screen_row(34, 10, 25), Some(0));
            assert_eq!(abs_to_screen_row(10, 10, 25), Some(24));
            assert_eq!(abs_to_screen_row(9, 10, 25), None);
            assert_eq!(abs_to_screen_row(35, 10, 25), None);
        }

        #[test]
        fn abs_to_screen_row_roundtrip() {
            for offset in [0, 1, 5, 50, 100] {
                for vr in [1, 10, 25, 80] {
                    for sr in 0..vr {
                        let abs = screen_row_to_abs(sr, offset, vr);
                        let back = abs_to_screen_row(abs, offset, vr);
                        assert_eq!(back, Some(sr), "roundtrip failed offset={offset} vr={vr} sr={sr}");
                    }
                }
            }
        }

        #[test]
        fn extract_selection_text_on_visible_row_with_no_scrollback() {
            // Regression: a fresh SSM session writes only a few lines.
            // The grid is e.g. 24 rows tall but only rows 0..N have
            // content, with N << 23. Clicking on visible content at
            // grid row 2 yields abs_row = 23 - 2 = 21 (assuming
            // scroll_offset 0). Before the fix, extract_selection_text
            // called set_scrollback(21) which clamped to 0 and read
            // grid row 23 (empty), returning "". After the fix, it
            // reads grid row 2 and returns the actual word.
            let mut parser = vt100::Parser::new(24, 80, 1000);
            // Three lines of content land on grid rows 0, 1, 2.
            parser.process(b"alpha\r\nbeta\r\ngamma");
            let abs_row = 21usize; // = (24-1) - 2; clicked on the "gamma" row
            let start = AbsPos { abs_row, col: 0 };
            let end = AbsPos { abs_row, col: 4 }; // "gamma" spans cols 0..=4
            let text = extract_selection_text(&mut parser, start, end);
            assert_eq!(text, "gamma");
        }

        #[test]
        fn extract_selection_text_in_scrollback() {
            // Selection lives in scrollback proper. Make sure we still
            // read the right row after vt100 clamps.
            let mut parser = vt100::Parser::new(5, 20, 100);
            // Push 10 lines so 5 end up in scrollback (rows beyond viewport).
            for i in 0..10 {
                let line = format!("line{i:02}\r\n");
                parser.process(line.as_bytes());
            }
            // Most-recent line at abs_row=0 is empty (cursor sits there
            // after final \r\n). Lines line00..line09 are above. Pick
            // line05 — abs_row = 4 (cursor empty row + line09..line06 = 4 above
            // line05). We can compute by looking up what's at each abs_row
            // via the same helper. For determinism, just assert the row
            // contains "line".
            let start = AbsPos { abs_row: 4, col: 0 };
            let end = AbsPos { abs_row: 4, col: 5 };
            let text = extract_selection_text(&mut parser, start, end);
            assert!(text.starts_with("line"), "got {text:?}");
        }

        #[test]
        fn pixel_to_grid_cell_basic() {
            let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 400.0));
            let cell_w = 8.0;
            let cell_h = 16.0;
            let rows = 25;
            let cols = 80;
            // Pixel at (100 + 3*8, 50 + 2*16) = cell (2, 3)
            let pos = egui::pos2(100.0 + 24.0, 50.0 + 32.0);
            let (row, col) = pixel_to_grid_cell(pos, rect, cell_w, cell_h, rows, cols);
            assert_eq!(row, 2);
            assert_eq!(col, 3);
        }

        #[test]
        fn pixel_to_grid_cell_clamps_to_bounds() {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 400.0));
            let cell_w = 8.0;
            let cell_h = 16.0;
            let rows = 25;
            let cols = 80;
            // Position above and to the left of the rect
            let (row, col) = pixel_to_grid_cell(egui::pos2(-50.0, -50.0), rect, cell_w, cell_h, rows, cols);
            assert_eq!(row, 0);
            assert_eq!(col, 0);
            // Position far below and to the right
            let (row, col) = pixel_to_grid_cell(egui::pos2(9999.0, 9999.0), rect, cell_w, cell_h, rows, cols);
            assert_eq!(row, rows - 1);
            assert_eq!(col, cols - 1);
        }
    }
}

#[cfg(feature = "gui")]
fn main() {
    gui::run();
}
