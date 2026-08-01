//! Task 11 (plan: session-review-and-subagent-backlog) — the one surviving
//! open question this plan declines to resolve by prose or guesswork:
//!
//!  1. What do `Notification.notification_type` values `agent_completed` and
//!     `agent_needs_input` actually carry, and when do they fire? Undocumented
//!     here — the engine deliberately fails open on them (see
//!     `engine::tests::unknown_notification_types_fail_open` and friends),
//!     rather than guessing at semantics from the name alone.
//!  2. Does `PostToolUse.tool_response` for a `run_in_background` Bash call
//!     carry a PID or shell id we could use for background-process tracking?
//!     (The plan's "NOT Building" list rules that feature out for now
//!     precisely because it's unverified.)
//!
//! **Reads real, LOCAL, live hook traffic — never a fixture, never CI.**
//! Binds a raw, unauthenticated HTTP listener and prints every request body it
//! receives verbatim (as raw JSON, not decoded through `engine::HookEvent` —
//! decoding would silently drop exactly the undocumented fields this harness
//! exists to see). Asserts nothing about a specific value; the printed
//! payloads are the deliverable, same as `prefix_sweep.rs`'s printed table.
//! Never commits a captured payload anywhere — the fixtures rule is
//! *authored, never harvested* (see `docs/internal/` conventions).
//!
//! ```text
//! cargo test --test notification_capture -- --ignored --nocapture
//! ```
//!
//! Then, temporarily, point a Claude Code session's hooks at
//! `http://127.0.0.1:4319/hook` (copy the app's real hook block from Settings
//! and swap the port) and trigger the two scenarios above — e.g. run a
//! subagent to see if it ever emits `agent_completed`/`agent_needs_input`,
//! and start a `run_in_background` Bash tool call and inspect its
//! `PostToolUse` body for a pid/shell id. Ctrl-C or let the window elapse;
//! restore the real hook config afterward.

use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tiny_http::{Response, Server};

/// Deliberately distinct from the app's default listener port (4317) so this
/// harness can run alongside a live Session Signals instance without colliding.
const CAPTURE_PORT: u16 = 4319;

/// How long to keep listening before giving up and printing a summary. Long
/// enough to manually trigger a subagent run and a background Bash call.
const CAPTURE_WINDOW: Duration = Duration::from_secs(600);

#[test]
#[ignore = "reads live local hook traffic; run with `cargo test --test notification_capture -- --ignored --nocapture` \
            and point a Claude Code session's hooks at http://127.0.0.1:4319/hook"]
fn capture_notification_payloads() {
    let addr = SocketAddr::from(([127, 0, 0, 1], CAPTURE_PORT));
    let server = Server::http(addr).expect("bind 127.0.0.1:4319 — is another instance running?");

    println!(
        "notification_capture: listening on http://127.0.0.1:{CAPTURE_PORT}/hook for up to {}s",
        CAPTURE_WINDOW.as_secs()
    );
    println!(
        "notification_capture: point a Claude Code session's hooks here (copy the real hook \
         block from Settings, swap the port to {CAPTURE_PORT}), then trigger a subagent run and \
         a `run_in_background` Bash call."
    );

    let deadline = Instant::now() + CAPTURE_WINDOW;
    let mut captured = 0usize;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match server.recv_timeout(remaining) {
            Ok(Some(mut request)) => {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                // Always answer 200 + JSON so a real Claude Code hook never logs
                // an error while we're capturing (see listener.rs's own note).
                let _ = request.respond(Response::from_string("{}").with_status_code(200));

                // Parse as a loose `Value`, never the strict `HookEvent` — the
                // whole point is to see fields the typed struct would silently
                // drop.
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(v) => {
                        captured += 1;
                        let event = v
                            .get("hook_event_name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("?");
                        println!("--- capture #{captured}: {event} ---");
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&v).unwrap_or(body.clone())
                        );
                    }
                    Err(e) => {
                        println!("--- capture: unparseable body ({e}) ---\n{body}");
                    }
                }
            }
            Ok(None) => break, // timed out with no request
            Err(e) => {
                println!("notification_capture: recv error: {e}");
                break;
            }
        }
    }

    println!("notification_capture: window elapsed — {captured} request(s) captured.");
}
