// Mirrors the Rust `AlertStubStatus` (src-tauri/src/lib.rs). One bool per
// state: whether a stub file currently resolves in the `alerts/` folder.
// Read fresh on every call — the backend does not cache it.
export interface AlertStubStatus {
  needs_you: boolean;
  working: boolean;
  ready: boolean;
  waiting_review: boolean;
}

/// States whose recurrence controls are meaningful. `working`/`ready` churn
/// via their own hook events and never recur — the backend hands back a
/// disabled policy for them (`engine::AlertPolicies::for_state`), so showing
/// the inputs would promise behavior that cannot happen.
export const RECURRING_STATES = ["needs_you", "waiting_review"] as const;
