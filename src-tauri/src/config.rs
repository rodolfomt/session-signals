//! User configuration: notification preferences + listener/runtime settings.
//!
//! Persisted as a single `config` object inside the shared `beacon.json` store.
//! Every field carries `#[serde(default)]`, so a config written by an older
//! build (missing newer keys) still loads — the missing keys fall back to their
//! defaults. `version` lets us run an explicit migration later if the shape
//! changes in a way defaults can't cover; for now `sanitized()` normalizes it.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "beacon.json";
const CONFIG_KEY: &str = "config";

/// Bump when the schema changes in a way that needs active migration.
pub const CURRENT_VERSION: u32 = 1;

pub const DEFAULT_PORT: u16 = 4317;
pub const DEFAULT_STALE_MIN: u64 = 10;
/// Total silence before an idle session is removed from the list. It stays
/// visibly greyed from `DEFAULT_STALE_MIN` until this — long enough to persist
/// rather than blink out, short enough to eventually clear a dead session whose
/// terminal never fired `SessionEnd`.
pub const DEFAULT_IDLE_DROP_MIN: u64 = 60;
/// Days an observation record survives before `prune` drops it.
pub const DEFAULT_OBSERVE_RETAIN_DAYS: u64 = 30;
/// Default minimum cluster size before an observed opening is offered as a
/// filter proposal.
pub const DEFAULT_PROPOSE_THRESHOLD: u32 = 3;
/// Floor for `propose_threshold`, enforced in [`Config::sanitized`] and
/// re-enforced in `proposals::build` — measured leakage on the research
/// corpus was 26 human patterns at 1, 3 at 2, and 0 at 3.
pub const MIN_PROPOSE_THRESHOLD: u32 = 3;
/// Minimum sample length (chars) a cluster's `sample` must reach before it's
/// proposal-eligible — enforced in `proposals::build`. PRD decision 6,
/// measured, not invented: no automatic (machine-spawned) opening in the
/// measured corpus falls below 90 characters, so a 60-char floor clears every
/// machine opening by a 30-character margin — the recall cost of this floor
/// against machine traffic is measured zero, not assumed. 60 also matches
/// `observe::PREFIX_LENS`'s existing shortest tracked length, so it closes the
/// one real gap: a naturally-short prompt (< 60 chars) is otherwise sampled at
/// its own length with no floor at all (see `observe::sample`'s doc). The
/// original mixed-human/machine-cluster sweep (peak: 5 mixed clusters at 8
/// chars, zero from 57 chars onward) still holds as corroborating evidence,
/// but the machine-opening minimum is the measurement that actually justifies
/// gating machine-spawned traffic at 60. See the "Minimum sample length"
/// section of `docs/IGNORING_BOT_SPAWNED_SESSIONS.md` for the sweep table and,
/// importantly, what it does not establish: `mixed` cannot see two *unmarked*
/// openings colliding, which is the case this floor actually guards.
pub const MIN_PROPOSE_SAMPLE_LEN: usize = 60;

/// Default seconds between re-alerts while a session sits in an alerting state
/// (feat03 external alerting). Two minutes: long enough not to be a nuisance for
/// a state you've already noticed, short enough to catch you on a return to the
/// desk. One knob — cooldown and recurrence interval are the same number.
pub const DEFAULT_ALERT_COOLDOWN_SECS: u64 = 120;
/// Floor for a *non-zero* `cooldown_secs`. `0` is legal and means "no recurrence";
/// anything between 1 and this is clamped up. Guards against a hand-edited
/// `beacon.json` turning the recurrence sweep into a per-second subprocess spawner.
pub const MIN_ALERT_COOLDOWN_SECS: u64 = 10;
/// Default maximum alerts per state-episode, counting the initial transition fire.
/// Three: the original alert plus two nudges, then silence until the state changes.
pub const DEFAULT_MAX_TRIGGERS: u32 = 3;
/// Hard ceiling on `max_triggers`. An episode that has alerted this many times has
/// made its point; past here it's noise, and an unbounded value hand-edited into
/// the config would keep a stuck session alerting indefinitely.
pub const MAX_ALERT_TRIGGERS: u32 = 20;

/// Built-in notification sounds (macOS system sound names under
/// `/System/Library/Sounds`). The settings UI offers this set.
pub const SOUNDS: &[&str] = &["Ping", "Glass", "Submarine", "Funk", "Pop", "Hero"];

/// Per-state notification preference.
///
/// Beyond the OS notification + sound, this also carries the external-alerting
/// settings (feat03): whether the state's CLI stub is eligible to run, and the
/// shared cooldown/recurrence controls that apply uniformly to *both* the sound
/// and the CLI channel.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct StateNotify {
    pub enabled: bool,
    pub sound: bool,
    pub sound_name: String,
    /// Whether this state's CLI stub in the `alerts/` folder is eligible to run.
    /// Gated by `enabled` like every other channel — a state that doesn't notify
    /// at all never spawns a stub. Harmless when `true` with no stub present:
    /// an absent stub is a no-op, not an error (see Phase 2). **Defaults to
    /// `false`** for every state — running a local executable is a bigger
    /// step than a sound or OS notification, so it's an explicit opt-in even
    /// though example stubs ship pre-populated for three of the four states
    /// (see `cli_alert::seed_example_stubs`).
    pub cli_enabled: bool,
    /// Seconds between re-alerts while a session stays in this state. One knob:
    /// this is both the minimum cooldown between re-triggers *and* the recurrence
    /// interval — they were never two independent things. `0` disables recurrence
    /// entirely (fire once on the transition edge, today's behavior). Floored at
    /// [`MIN_ALERT_COOLDOWN_SECS`] when non-zero.
    pub cooldown_secs: u64,
    /// Maximum total alerts per state-episode, **including** the initial
    /// transition fire. `1` is exactly today's single-shot behavior; the counter
    /// resets when the session's state changes. Clamped to
    /// `1..=MAX_ALERT_TRIGGERS`.
    pub max_triggers: u32,
}

impl StateNotify {
    fn new(enabled: bool, sound_name: &str) -> Self {
        StateNotify {
            enabled,
            sound: false,
            sound_name: sound_name.to_string(),
            // Off by default, uniformly: running an arbitrary local executable
            // is a bigger step than a sound or OS notification, so it should
            // be an explicit opt-in per state rather than something a fresh
            // install (or an old config gaining the field via migration)
            // silently turns on.
            cli_enabled: false,
            cooldown_secs: DEFAULT_ALERT_COOLDOWN_SECS,
            max_triggers: DEFAULT_MAX_TRIGGERS,
        }
    }
}

impl Default for StateNotify {
    fn default() -> Self {
        StateNotify::new(false, "Ping")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub port: u16,
    pub stale_timeout_min: u64,
    /// Minutes of total silence before an idle session is removed from the list
    /// entirely. Until then it stays visible, greyed ("No response"). Always
    /// `>= stale_timeout_min` (normalized in `sanitized`).
    pub idle_drop_min: u64,
    pub launch_on_login: bool,
    /// Notify when a session goes idle/stale. Off by default (spec: never notify
    /// on stale-drop unless enabled).
    pub notify_idle: bool,
    /// Suppress a transition notification when that session's terminal window is
    /// already frontmost — you're looking right at it. On by default. Falls back
    /// to firing whenever the terminal can't be resolved, so a Needs-you alert is
    /// never silently dropped. App/window level only: it can't tell which *tab*
    /// of a multiplexed terminal or IDE is focused (see settings copy / docs).
    pub notify_unfocused_only: bool,
    /// Active theme id (mirrors src/themes). The palette itself lives in the
    /// frontend; the backend only stores the chosen id and reacts to the palette
    /// the webview pushes via `set_tray_palette`.
    pub theme: String,
    pub needs_you: StateNotify,
    pub working: StateNotify,
    pub ready: StateNotify,
    /// Notification preference for `State::WaitingReview` (a flagged session
    /// finished). Enabled, no sound by default — matches `needs_you`. Plain
    /// `#[serde(default)]` at the struct level is enough: a config file
    /// written before this field existed deserializes it from
    /// `Config::default()` below, so no `CURRENT_VERSION` bump is needed.
    pub waiting_review: StateNotify,
    /// Rules that hide non-interactive / machine-spawned sessions (e.g. headless
    /// `claude --print` agents launched by third-party tooling) from the widget
    /// and tray rollup.
    ///
    /// **Empty by default** — Session Signals hides nothing until you ask it to.
    /// See `docs/IGNORING_BOT_SPAWNED_SESSIONS.md` for ready-made patterns. Deserialized leniently
    /// so a rule kind from a newer/older build is dropped rather than aborting the
    /// whole config parse (which would reset every unrelated setting).
    #[serde(default, deserialize_with = "crate::ignore::deserialize_lenient")]
    pub ignore_rules: Vec<crate::ignore::Matcher>,
    /// Openings the user has declared their own — outranks `ignore_rules` and
    /// is never observed (see `observe.rs`). **Empty by default**, same
    /// rationale as `ignore_rules`: no shipped pattern names a specific tool.
    /// Same lenient deserializer: an unrecognized matcher kind is dropped
    /// rather than aborting the whole config parse.
    #[serde(default, deserialize_with = "crate::ignore::deserialize_lenient")]
    pub never_hide: Vec<crate::ignore::Matcher>,
    /// User-configured additions to the built-in marker registry
    /// (`markers::BUILTIN_HUMAN`). **Additive only** — an entry colliding
    /// with a built-in prefix is dropped (see `markers::Registry::new`).
    /// Ordinary `#[serde(default)]`, not the lenient deserializer: this is a
    /// plain struct shape, not a tagged enum, so an unparseable entry is a
    /// genuine config error.
    #[serde(default)]
    pub markers: Vec<crate::markers::MarkerRule>,
    /// Whether Session Signals reads session openings to look for repeating
    /// patterns (salted-hash counts only — see `observe.rs`). On by default:
    /// the eventual filter-proposal surface presumes observation runs.
    #[serde(default = "default_observe_enabled")]
    pub observe_enabled: bool,
    /// Days an observation record is kept before being pruned. `0` sanitizes
    /// to [`DEFAULT_OBSERVE_RETAIN_DAYS`] — there's no "never" here.
    #[serde(default)]
    pub observe_retain_days: u64,
    /// Minimum cluster size before an observed opening is offered as a
    /// filter proposal. **Floored at [`MIN_PROPOSE_THRESHOLD`] in
    /// `sanitized()`, not in the UI** — measured leakage on the research
    /// corpus was 26 human patterns at 1, 3 at 2, and 0 at 3, and a UI-only
    /// default is bypassable by hand-editing this file.
    #[serde(default = "default_propose_threshold")]
    pub propose_threshold: u32,
}

fn default_observe_enabled() -> bool {
    true
}

fn default_propose_threshold() -> u32 {
    DEFAULT_PROPOSE_THRESHOLD
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CURRENT_VERSION,
            port: DEFAULT_PORT,
            stale_timeout_min: DEFAULT_STALE_MIN,
            idle_drop_min: DEFAULT_IDLE_DROP_MIN,
            launch_on_login: false,
            notify_idle: false,
            notify_unfocused_only: true,
            theme: "classic".to_string(),
            // Spec defaults: Red on (sound off); Orange/Green off.
            needs_you: StateNotify::new(true, "Ping"),
            working: StateNotify::new(false, "Pop"),
            ready: StateNotify::new(false, "Glass"),
            waiting_review: StateNotify::new(true, "Ping"),
            ignore_rules: crate::ignore::IgnoreRules::defaults(),
            never_hide: Vec::new(),
            markers: Vec::new(),
            observe_enabled: true,
            observe_retain_days: DEFAULT_OBSERVE_RETAIN_DAYS,
            propose_threshold: DEFAULT_PROPOSE_THRESHOLD,
        }
    }
}

impl Config {
    /// Clamp/normalize values arriving from the UI or an older file, and stamp
    /// the current schema version.
    pub fn sanitized(mut self) -> Self {
        // Stay out of the privileged range; fall back to the default port.
        if self.port < 1024 {
            self.port = DEFAULT_PORT;
        }
        if self.stale_timeout_min == 0 {
            self.stale_timeout_min = DEFAULT_STALE_MIN;
        }
        // An idle session must be greyed before it's dropped, so the drop window
        // can never be shorter than the stale timeout.
        if self.idle_drop_min < self.stale_timeout_min {
            self.idle_drop_min = self.stale_timeout_min;
        }
        if self.theme.trim().is_empty() {
            self.theme = "classic".to_string();
        }
        if self.observe_retain_days == 0 {
            self.observe_retain_days = DEFAULT_OBSERVE_RETAIN_DAYS;
        }
        if self.propose_threshold < MIN_PROPOSE_THRESHOLD {
            self.propose_threshold = MIN_PROPOSE_THRESHOLD;
        }
        // Normalize the per-state alerting knobs. A `cooldown_secs` of 0 is legal and
        // means "no recurrence" — only non-zero values get floored, so an existing
        // config can't be silently opted into repeat alerts. `max_triggers` of 0 would
        // mean "never alert", which `enabled: false` already expresses, so it clamps to
        // 1 (today's single-shot behavior) rather than creating a second off-switch.
        for pref in [
            &mut self.needs_you,
            &mut self.working,
            &mut self.ready,
            &mut self.waiting_review,
        ] {
            if pref.cooldown_secs > 0 && pref.cooldown_secs < MIN_ALERT_COOLDOWN_SECS {
                pref.cooldown_secs = MIN_ALERT_COOLDOWN_SECS;
            }
            pref.max_triggers = pref.max_triggers.clamp(1, MAX_ALERT_TRIGGERS);
        }
        self.version = CURRENT_VERSION;
        self
    }
}

/// Load config from the store, or defaults if absent/unreadable.
pub fn load(app: &AppHandle) -> Config {
    if let Ok(store) = app.store(STORE_FILE) {
        if let Some(v) = store.get(CONFIG_KEY) {
            if let Ok(cfg) = serde_json::from_value::<Config>(v) {
                return cfg.sanitized();
            }
        }
    }
    Config::default()
}

/// Persist config to the store.
pub fn save(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let v = serde_json::to_value(cfg).map_err(|e| e.to_string())?;
    store.set(CONFIG_KEY, v);
    store.save().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config written by a build that predates this plan (no `never_hide`,
    /// `markers`, `observe_enabled`, `observe_retain_days` keys) must still
    /// load, with the new fields filling in from their defaults rather than
    /// aborting the whole parse.
    #[test]
    fn existing_config_json_loads_with_new_defaults() {
        let json = serde_json::json!({
            "version": 1,
            "port": 4317,
            "stale_timeout_min": 10,
            "idle_drop_min": 60,
            "launch_on_login": false,
            "notify_idle": false,
            "notify_unfocused_only": true,
            "theme": "classic",
            "needs_you": { "enabled": true, "sound": false, "sound_name": "Ping" },
            "working": { "enabled": false, "sound": false, "sound_name": "Pop" },
            "ready": { "enabled": false, "sound": false, "sound_name": "Glass" },
            "ignore_rules": []
        });
        let cfg: Config = serde_json::from_value(json).expect("old config must still parse");
        let cfg = cfg.sanitized();
        assert!(cfg.never_hide.is_empty());
        assert!(cfg.markers.is_empty());
        assert!(cfg.observe_enabled);
        assert_eq!(cfg.observe_retain_days, DEFAULT_OBSERVE_RETAIN_DAYS);
        assert_eq!(cfg.propose_threshold, DEFAULT_PROPOSE_THRESHOLD);
    }

    /// A config written before `waiting_review` existed must still load, with
    /// the field filling in from its default (enabled, no sound — matching
    /// `needs_you`) rather than aborting the whole parse.
    #[test]
    fn old_config_without_waiting_review_still_loads() {
        let json = serde_json::json!({
            "version": 1,
            "port": 4317,
            "stale_timeout_min": 10,
            "idle_drop_min": 60,
            "launch_on_login": false,
            "notify_idle": false,
            "notify_unfocused_only": true,
            "theme": "classic",
            "needs_you": { "enabled": true, "sound": false, "sound_name": "Ping" },
            "working": { "enabled": false, "sound": false, "sound_name": "Pop" },
            "ready": { "enabled": false, "sound": false, "sound_name": "Glass" },
            "ignore_rules": []
        });
        let cfg: Config = serde_json::from_value(json).expect("old config must still parse");
        let cfg = cfg.sanitized();
        assert!(cfg.waiting_review.enabled);
        assert!(!cfg.waiting_review.sound);
    }

    #[test]
    fn zero_observe_retain_days_sanitizes_to_default() {
        let mut cfg = Config {
            observe_retain_days: 0,
            ..Config::default()
        };
        cfg = cfg.sanitized();
        assert_eq!(cfg.observe_retain_days, DEFAULT_OBSERVE_RETAIN_DAYS);
    }

    #[test]
    fn propose_threshold_below_floor_is_clamped() {
        for (input, expected) in [(0, 3), (1, 3), (5, 5)] {
            let cfg = Config {
                propose_threshold: input,
                ..Config::default()
            }
            .sanitized();
            assert_eq!(cfg.propose_threshold, expected, "input {input}");
        }
    }

    /// A config written before external alerting existed (no `cli_enabled`,
    /// `cooldown_secs`, `max_triggers` on the per-state objects) must still
    /// load, with each state's new keys filling in from `StateNotify::default()`
    /// rather than aborting the parse or zeroing out the recurrence settings.
    #[test]
    fn old_config_without_alert_fields_still_loads() {
        let json = serde_json::json!({
            "version": 1,
            "port": 4317,
            "stale_timeout_min": 10,
            "idle_drop_min": 60,
            "launch_on_login": false,
            "notify_idle": false,
            "notify_unfocused_only": true,
            "theme": "classic",
            "needs_you": { "enabled": true, "sound": false, "sound_name": "Ping" },
            "working": { "enabled": false, "sound": false, "sound_name": "Pop" },
            "ready": { "enabled": false, "sound": false, "sound_name": "Glass" },
            "waiting_review": { "enabled": true, "sound": false, "sound_name": "Ping" },
            "ignore_rules": []
        });
        let cfg: Config = serde_json::from_value(json).expect("old config must still parse");
        let cfg = cfg.sanitized();
        for pref in [
            &cfg.needs_you,
            &cfg.working,
            &cfg.ready,
            &cfg.waiting_review,
        ] {
            assert!(!pref.cli_enabled);
            assert_eq!(pref.cooldown_secs, DEFAULT_ALERT_COOLDOWN_SECS);
            assert_eq!(pref.max_triggers, DEFAULT_MAX_TRIGGERS);
        }
        // The pre-existing fields must survive the migration untouched.
        assert!(cfg.needs_you.enabled);
        assert_eq!(cfg.ready.sound_name, "Glass");
    }

    /// `0` means "no recurrence" and must survive sanitization; any other
    /// sub-floor value is clamped up so a hand-edited config can't spawn alerts
    /// every second.
    #[test]
    fn cooldown_secs_floor_applies_only_to_nonzero() {
        for (input, expected) in [
            (0, 0),
            (1, MIN_ALERT_COOLDOWN_SECS),
            (9, MIN_ALERT_COOLDOWN_SECS),
            (10, 10),
            (300, 300),
        ] {
            let cfg = Config {
                needs_you: StateNotify {
                    cooldown_secs: input,
                    ..StateNotify::default()
                },
                ..Config::default()
            }
            .sanitized();
            assert_eq!(cfg.needs_you.cooldown_secs, expected, "input {input}");
        }
    }

    /// `max_triggers` clamps into `1..=MAX_ALERT_TRIGGERS`: 0 would duplicate
    /// `enabled: false`, and an unbounded value would alert forever.
    #[test]
    fn max_triggers_clamps_into_range() {
        for (input, expected) in [
            (0, 1),
            (1, 1),
            (3, 3),
            (MAX_ALERT_TRIGGERS, MAX_ALERT_TRIGGERS),
            (9_999, MAX_ALERT_TRIGGERS),
        ] {
            let cfg = Config {
                waiting_review: StateNotify {
                    max_triggers: input,
                    ..StateNotify::default()
                },
                ..Config::default()
            }
            .sanitized();
            assert_eq!(cfg.waiting_review.max_triggers, expected, "input {input}");
        }
    }

    /// Defaults are uniform across all four states by design: `serde(default)`
    /// fills a missing key from a single `StateNotify::default()` with no idea
    /// which state it belongs to, so differentiated defaults would migrate old
    /// configs to values a fresh install would never produce. Per-state behavior
    /// differentiation lives in `enabled`, not in the recurrence knobs.
    #[test]
    fn alert_defaults_are_uniform_across_states() {
        let cfg = Config::default();
        for pref in [
            &cfg.needs_you,
            &cfg.working,
            &cfg.ready,
            &cfg.waiting_review,
        ] {
            assert_eq!(pref.cooldown_secs, DEFAULT_ALERT_COOLDOWN_SECS);
            assert_eq!(pref.max_triggers, DEFAULT_MAX_TRIGGERS);
            assert!(!pref.cli_enabled);
        }
        // ...while the existing per-state gate stays differentiated.
        assert!(cfg.needs_you.enabled);
        assert!(!cfg.working.enabled);
    }
}
