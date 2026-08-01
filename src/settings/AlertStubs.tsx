import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { StateNotify } from "../state/config";
import { Toggle } from "./Settings";

type StateKey = "needs_you" | "working" | "ready" | "waiting_review";

// Mirrors the backend clamps in `config.rs::sanitized()`. The floor and cap
// are enforced server-side; these bounds only stop the spinner from
// suggesting values that would be silently rewritten on save.
const MIN_COOLDOWN_SECS = 10;
const MAX_TRIGGERS_CAP = 20;

export function StubDetail({
  stateKey,
  value,
  detected,
  recurring,
  onChange,
  flash,
}: {
  stateKey: StateKey;
  value: StateNotify;
  detected: boolean;
  recurring: boolean;
  onChange: (partial: Partial<StateNotify>) => void;
  flash: (msg: string, kind: "ok" | "err") => void;
}) {
  const test = useCallback(async () => {
    try {
      const outcome = await invoke<string>("test_alert_stub", { state: stateKey });
      if (outcome === "fired") flash("Stub fired", "ok");
      else if (outcome === "no_stub") flash("No stub found for this state", "err");
      else flash("A stub for this state is already running", "err");
    } catch (e) {
      flash(String(e), "err");
    }
  }, [stateKey, flash]);

  const remaining = Math.max(0, value.max_triggers - 1);

  return (
    <div className="sStubDetail">
      <div className="sStubRow">
        <span className="sRowTitle">Run script</span>
        <span className={`sStubStatus ${detected ? "on" : ""}`}>
          {detected ? "stub found" : "no stub"}
        </span>
        <button type="button" className="sBtn" onClick={test} disabled={!detected}>
          Test
        </button>
        <Toggle checked={value.cli_enabled} onChange={(v) => onChange({ cli_enabled: v })} />
      </div>
      {recurring && (
        <>
          <div className="sStubRow">
            <div className="sChip">
              <input
                className="sChipInput sChipInputWide"
                type="number"
                min={0}
                value={value.cooldown_secs}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  if (Number.isFinite(v) && v >= 0) onChange({ cooldown_secs: v });
                }}
              />
              <span className="sChipSuf">s</span>
            </div>
            <div className="sChip">
              <input
                className="sChipInput sChipInputWide"
                type="number"
                min={1}
                max={MAX_TRIGGERS_CAP}
                value={value.max_triggers}
                onChange={(e) => {
                  const v = parseInt(e.target.value, 10);
                  if (Number.isFinite(v) && v >= 1) onChange({ max_triggers: v });
                }}
              />
              <span className="sChipSuf">alerts</span>
            </div>
          </div>
          <p className="sStubHint">
            {value.cooldown_secs < MIN_COOLDOWN_SECS
              ? remaining > 0
                ? `First alert fires immediately; repeats are off below ${MIN_COOLDOWN_SECS}s.`
                : "Fires once, immediately."
              : remaining > 0
                ? `First alert fires immediately; ${remaining} more follow every ${value.cooldown_secs}s.`
                : "Fires once, immediately."}
          </p>
        </>
      )}
    </div>
  );
}

export function AlertsFolderCard({
  path,
  flash,
  onRefresh,
}: {
  path: string;
  flash: (msg: string, kind: "ok" | "err") => void;
  onRefresh: () => void;
}) {
  const openFolder = useCallback(async () => {
    try {
      await invoke("reveal_alerts_dir");
      onRefresh();
    } catch (e) {
      flash(String(e), "err");
    }
  }, [flash, onRefresh]);

  return (
    <div className="sCard sCardPad">
      <div className="sAlertsFolderRow">
        <div className="sRowText">
          <span className="sRowTitle">Alert scripts</span>
          <span className="sRowHint">
            Drop on_needs_you.exe / .bat / .cmd here. See README.txt.
          </span>
        </div>
        <button type="button" className="sBtn" onClick={openFolder} disabled={!path}>
          Open folder
        </button>
      </div>
      {path && <p className="sAlertsPath">{path}</p>}
    </div>
  );
}
