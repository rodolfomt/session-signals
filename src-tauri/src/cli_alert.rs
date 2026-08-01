//! External CLI alert channel: invoke a user-authored executable from the
//! fixed `alerts/` folder in the app-data dir (see [`alerts_dir`]) when a
//! session enters — or stays in — an alerting state.
//!
//! **Deliberately convention-based, not configurable.** There is no path
//! field and no argv configuration. Only a fixed filename in that fixed
//! folder with a recognized extension is eligible to run, and it always
//! receives the same four positional arguments in the same order. That closes
//! the argument-injection and arbitrary-path surface a free-text "command to
//! run" setting would open — see the PRD's Decisions Log.
//!
//! Arguments never pass through a shell we invoke. The one platform exception
//! is outside our control: Windows can only launch `.bat`/`.cmd` via
//! `cmd.exe`, so std routes those through it. Since Rust 1.77 std escapes the
//! arguments for that case, and refuses the launch outright when an argument
//! can't be represented safely (CVE-2024-24576) — a failed spawn, never a
//! mis-parsed one. `.exe` stubs are unaffected.
//!
//! Privacy: this is a local process spawn. Session Signals makes no network
//! call here and never will (CLAUDE.md guardrail). What the *user's own stub*
//! does is the user's business.

use crate::engine::State;
use crate::notify::state_slug;
use crate::LockExt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Folder name, resolved inside the app-data dir.
const ALERTS_DIR: &str = "alerts";
/// A stub still running after this long is killed. Alert stubs are meant to be
/// brief (open a notification app, play a sound, ping a webhook) — this guards
/// against a hung stub silently blocking future alerts for its (session, state)
/// key forever.
const MAX_STUB_RUNTIME: Duration = Duration::from_secs(30);
/// Poll interval while waiting for a spawned stub to exit.
const REAP_POLL: Duration = Duration::from_millis(100);

/// Recognized stub extensions, in precedence order (first match wins), per
/// platform — matches what the OS can execute directly without a shell.
fn stub_extensions() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["exe", "bat", "cmd"]
    } else if cfg!(target_os = "macos") {
        &["sh", "command"]
    } else {
        &["sh"]
    }
}

/// Resolved once at startup by [`init`]. `None` before that (and in unit
/// tests, which drive `resolve_in` against a scratch dir directly).
static ALERTS_DIR_PATH: OnceLock<PathBuf> = OnceLock::new();

/// The `alerts/` folder — inside the app-data dir, alongside `beacon.json`
/// and the capture script.
///
/// **Not next to the executable.** An installed app's own directory is
/// `C:\Program Files\...` on Windows, `/usr/bin` on Linux, and *inside the
/// signed `.app` bundle* on macOS: unwritable without admin, invisible to a
/// user who just wants to drop a script somewhere, and on macOS both
/// signature-breaking and erased by every update. The app-data dir is
/// user-writable and survives upgrades, which is the whole point of a folder
/// the user is expected to put files into.
///
/// `None` only if [`init`] hasn't run or the platform dir couldn't be
/// resolved — in which case no stub ever resolves, which degrades to "no CLI
/// alerts configured" rather than to an error.
pub fn alerts_dir() -> Option<PathBuf> {
    ALERTS_DIR_PATH.get().cloned()
}

/// Candidate filenames for `state`, in extension-precedence order.
pub fn stub_filenames(state: State) -> Vec<String> {
    let slug = state_slug(state);
    stub_extensions()
        .iter()
        .map(|ext| format!("on_{slug}.{ext}"))
        .collect()
}

/// Resolve the first existing stub file for `state` inside `dir`, honoring
/// extension precedence. A directory that happens to share a stub's name is
/// not a match (`is_file` excludes it).
pub fn resolve_in(dir: &Path, state: State) -> Option<PathBuf> {
    stub_filenames(state)
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

/// Resolve the stub for `state` in the real `alerts_dir()`.
pub fn stub_for(state: State) -> Option<PathBuf> {
    resolve_in(&alerts_dir()?, state)
}

/// Resolve the `alerts/` folder inside the app-data dir, create it, and seed
/// its README. Call once from `setup`, before any alert could fire.
///
/// Failures are logged, not fatal: without the folder no stub resolves, which
/// is indistinguishable from "no alerts configured" — a degraded feature, not
/// a broken app. But unlike the previous silent version, a user wondering why
/// nothing runs has a line to find.
pub fn init(app: &AppHandle) {
    let base = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("beacon: no app data dir; CLI alerts disabled: {e}");
            return;
        }
    };
    let dir = base.join(ALERTS_DIR);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "beacon: could not create {}; CLI alerts disabled: {e}",
            dir.display()
        );
        return;
    }
    let readme = dir.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(&readme, README_TEXT);
    }
    seed_example_stubs(&dir);
    let _ = ALERTS_DIR_PATH.set(dir);
}

/// States that ship a working example stub out of the box. Chosen to cover
/// the two states that notify by default (`NeedsYou`, `WaitingReview`) plus
/// `Ready` — `Working` is left unseeded since it's off by default and, per
/// its own Settings hint, the state a user least wants pinged for.
const EXAMPLE_STUB_STATES: [State; 3] = [State::NeedsYou, State::Ready, State::WaitingReview];

/// The one stub extension seeded as a working example, chosen per platform
/// from [`stub_extensions`]: always a plain-text script a user can open and
/// read, never the binary `.exe` Windows also accepts as a stub.
fn example_stub_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "bat"
    } else {
        "sh"
    }
}

/// Seed a working example stub for each of [`EXAMPLE_STUB_STATES`] into
/// `dir`, one per call, skipping any filename that already exists — this
/// never overwrites a stub the user has since customized or replaced. Each
/// example does exactly one thing: append a line recording the four
/// arguments it received to a log file in its own `logs/<state>/`
/// subfolder, seeded with a heading explaining what wrote it and why. Purely
/// illustrative — `cli_enabled` still defaults to `false`, so nothing fires
/// until the state's "Run script" toggle is turned on in Settings.
fn seed_example_stubs(dir: &Path) {
    for state in EXAMPLE_STUB_STATES {
        let slug = state_slug(state);
        let ext = example_stub_extension();
        let path = dir.join(format!("on_{slug}.{ext}"));
        if path.exists() {
            continue;
        }
        if let Err(e) = std::fs::write(&path, example_stub_source(slug, ext)) {
            eprintln!(
                "beacon: could not seed example stub {}: {e}",
                path.display()
            );
            continue;
        }
        // Unix has no "executable" file attribute equivalent to Windows'
        // extension-based association — a freshly-written .sh file resolves
        // as a stub (`is_file()`) but the kernel refuses to `execve` it
        // without +x, exactly the asymmetry documented in this module's own
        // top-level comment. Without this, our own shipped example would
        // silently fail to launch on first use.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            {
                eprintln!(
                    "beacon: could not mark example stub {} executable: {e}",
                    path.display()
                );
            }
        }
    }
}

/// Source text for a logging-only example stub, native to the current
/// platform's scripting convention (`.bat` on Windows, `.sh` elsewhere).
/// Both variants are functionally identical: read the four positional
/// arguments, create `logs/<slug>/log.txt` with an explanatory heading on
/// first run, then append one line per invocation.
fn example_stub_source(slug: &str, ext: &str) -> String {
    if ext == "bat" {
        format!(
            r##"@echo off
setlocal
set "STATE=%~1"
set "PROJECT=%~2"
set "BRANCH=%~3"
set "DESCRIPTOR=%~4"
set "LOGDIR=%~dp0logs\{slug}"
if not exist "%LOGDIR%" mkdir "%LOGDIR%" >nul 2>&1
set "LOGFILE=%LOGDIR%\log.txt"
if not exist "%LOGFILE%" (
  >"%LOGFILE%" echo # Session Signals -- {slug} stub log
  >>"%LOGFILE%" echo # Written by on_{slug}.bat, a working example stub shipped
  >>"%LOGFILE%" echo # with Session Signals. Every time this state's "Run script"
  >>"%LOGFILE%" echo # toggle is on in Settings and the alert fires -- or you press
  >>"%LOGFILE%" echo # Test -- this script appends one line below: the time, then
  >>"%LOGFILE%" echo # the four positional arguments Session Signals always passes.
  >>"%LOGFILE%" echo(
)
>>"%LOGFILE%" echo %DATE% %TIME% state=%STATE% project=%PROJECT% branch=%BRANCH% descriptor=%DESCRIPTOR%
endlocal
"##,
            slug = slug
        )
    } else {
        format!(
            r##"#!/bin/sh
STATE="$1"
PROJECT="$2"
BRANCH="$3"
DESCRIPTOR="$4"
DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
LOGDIR="$DIR/logs/{slug}"
mkdir -p "$LOGDIR"
LOGFILE="$LOGDIR/log.txt"
if [ ! -f "$LOGFILE" ]; then
  {{
    echo "# Session Signals -- {slug} stub log"
    echo "# Written by on_{slug}.sh, a working example stub shipped with"
    echo "# Session Signals. Every time this state's \"Run script\" toggle"
    echo "# is on in Settings and the alert fires -- or you press Test --"
    echo "# this script appends one line below: the time, then the four"
    echo "# positional arguments Session Signals always passes."
    echo ""
  }} > "$LOGFILE"
fi
echo "$(date) state=$STATE project=$PROJECT branch=$BRANCH descriptor=$DESCRIPTOR" >> "$LOGFILE"
"##,
            slug = slug
        )
    }
}

const README_TEXT: &str = "\
Session Signals — external alert stubs
======================================

Drop an executable here named for the state it should react to:

    on_needs_you.<ext>        a session is blocked on you
    on_waiting_review.<ext>   a flagged session finished
    on_working.<ext>          a session started working
    on_ready.<ext>            a session finished its turn

Recognized extensions, in precedence order (first match wins):
    Windows:  .exe  .bat  .cmd
    macOS:    .sh   .command
    Linux:    .sh

Your stub is invoked with exactly four positional arguments, always in
this order. Absent values are passed as an empty string, never skipped:

    $1  state       e.g. needs_you
    $2  project     the session's folder name
    $3  branch      git branch, or \"\" if not resolvable
    $4  descriptor  the session's title / first prompt, or \"\"

Arguments are passed directly, never through a shell, so values with
spaces arrive intact as a single argument and nothing is re-split or
expanded. (One platform caveat: Windows runs .bat and .cmd files via
cmd.exe by necessity — arguments are still escaped for you, but an
argument cmd.exe cannot represent safely will make the launch fail
rather than run something unintended. A .exe has no such caveat.)

No environment variables are set. Output is discarded. A stub still
running after 30 seconds is killed.

The \"Run script\" toggle for a state is OFF by default in Settings, even
when a stub is present — running a local executable is a bigger step than
a sound or notification, so it's always an explicit opt-in.

Three working examples ship pre-populated in this folder: on_needs_you,
on_ready, and on_waiting_review (on_working is left for you to write, since
that state is off by default and the noisiest one to alert on). Each one
only logs — it appends a line recording the four arguments above to its
own logs/<state>/log.txt, with a heading on first run explaining what wrote
it. Turn on that state's \"Run script\" toggle and press Test to see it
work, or open the log after a real trigger. Delete or overwrite any of
them to replace it with your own stub of the same name.
";

/// Concurrency guard: which `session:state` keys currently have a stub in
/// flight. Prevents a slow stub from overlapping with itself on repeated
/// recurrence fires.
static IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn in_flight_key(session_id: &str, state: State) -> String {
    format!("{}:{}", session_id, state_slug(state))
}

/// Claim the key for this invocation. Returns `false` if already in flight.
fn claim(key: &str) -> bool {
    let mut guard = IN_FLIGHT.lock_safe();
    guard
        .get_or_insert_with(HashSet::new)
        .insert(key.to_string())
}

fn release(key: &str) {
    let mut guard = IN_FLIGHT.lock_safe();
    if let Some(set) = guard.as_mut() {
        set.remove(key);
    }
}

/// Everything `fire` needs to build the four positional arguments and resolve
/// the concurrency-guard key. Caller (the engine/sweep-thread dispatch) is
/// responsible for gating this on the state's `cli_enabled` preference first.
#[derive(Clone, Debug)]
pub struct StubInvocation {
    pub session_id: String,
    pub state: State,
    pub project: String,
    pub branch: Option<String>,
    pub descriptor: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FireOutcome {
    /// A stub was resolved and a detached thread was spawned to run it.
    Fired,
    /// No stub file exists for this state — a silent no-op, not an error.
    NoStub,
    /// A stub for this exact (session, state) is already running.
    Busy,
}

/// Resolve and spawn the stub for `inv.state`, if any, on a detached thread.
/// Never blocks the caller: the spawn + reap + timeout-kill all happen off
/// the calling thread, so this must be called with no locks held.
pub fn fire(inv: StubInvocation) -> FireOutcome {
    let Some(path) = stub_for(inv.state) else {
        return FireOutcome::NoStub;
    };
    let key = in_flight_key(&inv.session_id, inv.state);
    if !claim(&key) {
        return FireOutcome::Busy;
    }

    let args = [
        state_slug(inv.state).to_string(),
        inv.project,
        inv.branch.unwrap_or_default(),
        inv.descriptor.unwrap_or_default(),
    ];

    let key_for_thread = key.clone();
    let spawned_thread = std::thread::Builder::new()
        .name("beacon-alert-stub".into())
        .spawn(move || {
            let key = key_for_thread;
            let spawned = Command::new(&path)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();

            match spawned {
                Ok(mut child) => {
                    let started = Instant::now();
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) => {
                                if started.elapsed() >= MAX_STUB_RUNTIME {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    eprintln!(
                                        "beacon: alert stub {} exceeded {}s; killed",
                                        path.display(),
                                        MAX_STUB_RUNTIME.as_secs()
                                    );
                                    break;
                                }
                                std::thread::sleep(REAP_POLL);
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(e) => {
                    // On Windows this is also how an argument cmd.exe can't
                    // safely represent surfaces for a .bat/.cmd stub — the launch
                    // is refused rather than mis-escaped. Worth naming, since the
                    // descriptor argument is free-form text.
                    eprintln!(
                        "beacon: failed to launch alert stub {}: {e}",
                        path.display()
                    );
                }
            }
            release(&key);
        });

    // If the thread itself couldn't start, nothing will ever release the
    // claim — that would wedge this (session, state) on `Busy` for the rest
    // of the process's life.
    if let Err(e) = spawned_thread {
        release(&key);
        eprintln!("beacon: could not start alert-stub thread: {e}");
        return FireOutcome::NoStub;
    }

    FireOutcome::Fired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "beacon-cli-alert-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn example_stub_extension_is_platform_appropriate() {
        let expected = if cfg!(target_os = "windows") {
            "bat"
        } else {
            "sh"
        };
        assert_eq!(example_stub_extension(), expected);
    }

    #[test]
    fn example_stub_source_names_itself_and_its_own_log_folder() {
        for (ext, shebang) in [("bat", "@echo off"), ("sh", "#!/bin/sh")] {
            let src = example_stub_source("needs_you", ext);
            assert!(
                src.starts_with(shebang),
                "{ext} source must start with {shebang}"
            );
            assert!(
                src.contains("on_needs_you"),
                "{ext} source must name its own file"
            );
            assert!(
                src.contains("needs_you"),
                "{ext} source must reference its own log subfolder"
            );
        }
    }

    #[test]
    fn seed_example_stubs_creates_exactly_the_three_named_states() {
        let dir = scratch_dir("seed-new");
        seed_example_stubs(&dir);
        let ext = example_stub_extension();
        for state in EXAMPLE_STUB_STATES {
            let path = dir.join(format!("on_{}.{ext}", state_slug(state)));
            assert!(path.is_file(), "{} must be seeded", path.display());
        }
        // `Working` is deliberately not in EXAMPLE_STUB_STATES.
        assert!(!dir.join(format!("on_working.{ext}")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seed_example_stubs_never_overwrites_an_existing_file() {
        let dir = scratch_dir("seed-preserve");
        let ext = example_stub_extension();
        let path = dir.join(format!("on_needs_you.{ext}"));
        std::fs::write(&path, "user's own stub, not ours").unwrap();

        seed_example_stubs(&dir);

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "user's own stub, not ours"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stub_filenames_follow_the_on_state_convention() {
        let names = stub_filenames(State::NeedsYou);
        for name in &names {
            assert!(name.starts_with("on_needs_you."));
        }
    }

    #[test]
    fn extension_precedence_is_platform_correct() {
        let names = stub_filenames(State::Ready);
        let expected: Vec<String> = stub_extensions()
            .iter()
            .map(|ext| format!("on_ready.{ext}"))
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn missing_folder_and_missing_stub_resolve_to_none() {
        let dir = scratch_dir("missing");
        assert_eq!(resolve_in(&dir, State::Working), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_in_finds_a_stub_and_honors_precedence() {
        let dir = scratch_dir("precedence");
        // On Windows the precedence order is exe, bat, cmd — write the
        // lower-precedence one first, then the higher-precedence one, and
        // confirm the higher-precedence file wins.
        let exts = stub_extensions();
        let low = dir.join(format!("on_waiting_review.{}", exts[exts.len() - 1]));
        std::fs::write(&low, "").unwrap();
        let high = dir.join(format!("on_waiting_review.{}", exts[0]));
        std::fs::write(&high, "").unwrap();

        let resolved = resolve_in(&dir, State::WaitingReview);
        assert_eq!(resolved, Some(high));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_named_like_a_stub_is_not_a_stub() {
        let dir = scratch_dir("dir-not-file");
        let exts = stub_extensions();
        let fake = dir.join(format!("on_needs_you.{}", exts[0]));
        std::fs::create_dir_all(&fake).unwrap();

        assert_eq!(resolve_in(&dir, State::NeedsYou), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The extension table must be non-empty on every platform. An empty branch
    /// would make `resolve_in` return `None` unconditionally — the CLI channel
    /// would silently not exist, with no error anywhere.
    #[test]
    fn every_platform_has_at_least_one_stub_extension() {
        assert!(!stub_extensions().is_empty());
    }

    /// Extensions are bare, lowercase, dotless tokens. `stub_filenames` joins
    /// them with a literal `.`, so a leading dot here would yield `on_ready..sh`.
    #[test]
    fn stub_extensions_are_bare_lowercase_tokens() {
        for ext in stub_extensions() {
            assert!(!ext.starts_with('.'), "{ext} must not include the dot");
            assert!(!ext.is_empty());
            assert_eq!(*ext, ext.to_ascii_lowercase(), "{ext} must be lowercase");
            assert!(
                ext.chars().all(|c| c.is_ascii_alphanumeric()),
                "{ext} must be a plain alphanumeric extension"
            );
        }
    }

    /// No duplicates — a repeated extension would make precedence ambiguous.
    #[test]
    fn stub_extensions_are_unique() {
        let mut seen = HashSet::new();
        for ext in stub_extensions() {
            assert!(seen.insert(*ext), "duplicate extension {ext}");
        }
    }

    /// Every state must produce a candidate filename for every platform
    /// extension — a state missing from the naming convention would be
    /// permanently unalertable via CLI, silently.
    #[test]
    fn all_four_states_produce_candidates_on_every_platform() {
        let states = [
            State::NeedsYou,
            State::Working,
            State::Ready,
            State::WaitingReview,
        ];
        for state in states {
            let names = stub_filenames(state);
            assert_eq!(
                names.len(),
                stub_extensions().len(),
                "{state:?} must have one candidate per extension"
            );
            for name in &names {
                assert!(name.starts_with(&format!("on_{}", state_slug(state))));
            }
        }
    }

    /// Two different states must never resolve to the same filename, or one
    /// would shadow the other in the folder.
    #[test]
    fn state_stub_filenames_do_not_collide() {
        let states = [
            State::NeedsYou,
            State::Working,
            State::Ready,
            State::WaitingReview,
        ];
        let mut seen = HashSet::new();
        for state in states {
            for name in stub_filenames(state) {
                assert!(seen.insert(name.clone()), "collision on {name}");
            }
        }
    }

    /// `resolve_in` must build candidates with `Path::join`, never string
    /// concatenation with a hardcoded separator. Driving it through a nested
    /// scratch dir proves the joined path is correct on the host's separator.
    #[test]
    fn resolution_uses_path_join_not_a_hardcoded_separator() {
        let root = scratch_dir("sep");
        let nested = root.join("deeper").join("still");
        std::fs::create_dir_all(&nested).unwrap();

        let name = &stub_filenames(State::Ready)[0];
        let stub = nested.join(name);
        std::fs::write(&stub, "").unwrap();

        assert_eq!(resolve_in(&nested, State::Ready), Some(stub));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A stub with the right name but a *foreign* platform's extension must not
    /// resolve. Guards the table from being widened by accident into "any
    /// extension we've ever heard of," which is the security boundary the PRD's
    /// fixed-folder decision rests on.
    #[test]
    fn a_foreign_platform_extension_does_not_resolve() {
        let dir = scratch_dir("foreign");
        let foreign = if stub_extensions().contains(&"exe") {
            "sh"
        } else {
            "exe"
        };
        std::fs::write(dir.join(format!("on_needs_you.{foreign}")), "").unwrap();

        assert_eq!(resolve_in(&dir, State::NeedsYou), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The README shipped into `alerts/` lists every platform's extensions. If
    /// the table changes and the README doesn't, users get told to create a file
    /// that will never resolve — the most confusing possible failure, since
    /// nothing errors.
    #[test]
    fn readme_documents_every_extension_for_this_platform() {
        for ext in stub_extensions() {
            assert!(
                README_TEXT.contains(&format!(".{ext}")),
                "README does not document .{ext}"
            );
        }
    }

    #[test]
    fn in_flight_guard_blocks_only_the_same_session_and_state() {
        let key_a = in_flight_key("session-a", State::NeedsYou);
        let key_b = in_flight_key("session-b", State::NeedsYou);
        let key_c = in_flight_key("session-a", State::Working);

        assert!(claim(&key_a));
        assert!(!claim(&key_a));
        assert!(claim(&key_b));
        assert!(claim(&key_c));

        release(&key_a);
        assert!(claim(&key_a));

        release(&key_a);
        release(&key_b);
        release(&key_c);
    }
}
