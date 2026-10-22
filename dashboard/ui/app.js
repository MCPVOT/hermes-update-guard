// Read-only viewer for a guarded update.
//
// Polls the Rust command `snapshot()` every 600 ms and paints the run state. It
// never writes: the guard owns backups/guard-run.json, this only renders it.

const invoke = window.__TAURI__?.core?.invoke;
const STAGES = ["preflight", "export", "backup", "update", "verify"];
const POLL_MS = 600;

const $ = (id) => document.getElementById(id);

let alwaysOnTop = false;
let lastLogKey = "";
let lastProfilesKey = "";
let lastTitle = "";
let mounted = false;

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

/** Seconds between an ISO-ish local stamp and now (or between two stamps). */
function elapsedSeconds(state) {
  const parse = (s) => {
    if (!s) return null;
    const d = new Date(s);
    return Number.isNaN(d.getTime()) ? null : d;
  };
  const start = parse(state.started_at);
  if (!start) return 0;
  const end = parse(state.finished_at) || new Date();
  return Math.max(0, Math.round((end - start) / 1000));
}

function humanDuration(sec) {
  if (sec < 60) return `${sec}s`;
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

function logClass(line) {
  const lower = line.toLowerCase();
  if (lower.includes("fail") || lower.includes("error") || lower.includes("***")) return "l-fail";
  if (lower.includes("[warn]") || lower.includes("warn")) return "l-warn";
  if (lower.includes("[ok]") || lower.includes(" ok ")) return "l-ok";
  if (line.startsWith("--") || line.startsWith("==")) return "l-dim";
  return "";
}

function renderRail(stages) {
  const rail = $("rail");
  rail.textContent = "";
  // A glyph as well as a colour: status conveyed by colour alone is unreadable
  // for a colour-blind operator and fails WCAG 1.4.1.
  const MARK = { done: "✓", failed: "✕", active: "▸", waiting: "·" };
  for (const name of STAGES) {
    const s = stages?.find?.((x) => x.name === name);
    const state = s?.state || "waiting";
    const chip = el("span", `stage ${state}`, `${MARK[state] || "·"} ${name}`);
    chip.setAttribute("aria-label", `${name}: ${state}`);
    chip.title = `${name}: ${state}`;
    rail.appendChild(chip);
  }
}

function renderProfiles(profiles) {
  const list = $("plist");
  // Rebuilding this list every 600 ms throws away scroll position and forces a
  // reflow even when nothing changed. Key it on content and skip the rebuild.
  const key = (profiles || []).map((p) => `${p.name}:${p.state}:${p.detail || ""}`).join("|");
  if (key === lastProfilesKey) return;
  lastProfilesKey = key;

  list.textContent = "";
  $("pcount").textContent = profiles?.length ? `${profiles.length}` : "";
  if (!profiles?.length) {
    const li = el("li");
    li.appendChild(el("span", "dot"));
    li.appendChild(el("span", "pname", "no profiles yet"));
    li.appendChild(el("span", "pdetail", ""));
    list.appendChild(li);
    return;
  }
  for (const p of profiles) {
    const li = el("li");
    li.appendChild(el("span", `dot ${p.state || "waiting"}`));
    li.appendChild(el("span", "pname", p.name));
    li.appendChild(el("span", "pdetail", p.detail || p.state || ""));
    list.appendChild(li);
  }
}

// Blockers stop a run; warnings are things it noted and continued past. They
// were the same list once, which is how "PASS" ended up displayed next to
// "3 unfinished update receipt(s)".
function renderBlockers(blockers, warnings) {
  const box = $("blockers");
  const ul = $("blist");
  ul.textContent = "";
  const items = [
    ...(blockers || []).map((b) => ["blocker", b]),
    ...(warnings || []).map((w) => ["warning", w]),
  ];
  if (!items.length) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  $("bhead").textContent =
    blockers?.length && warnings?.length
      ? "Blockers · warnings"
      : blockers?.length
        ? "Blockers"
        : "Warnings";
  for (const [kind, text] of items) ul.appendChild(el("li", kind, text));
}

function renderLog(lines) {
  const pre = $("log");
  const key = `${lines.length}:${lines[lines.length - 1] || ""}`;
  if (key === lastLogKey) return;
  lastLogKey = key;

  const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 40;
  pre.textContent = "";
  $("lcount").textContent = lines.length ? `${lines.length} lines` : "";

  if (!lines.length) {
    pre.appendChild(
      el(
        "span",
        "empty",
        "no log file — an attached run prints to its own terminal. Use `update --detached` to get one here."
      )
    );
    return;
  }
  const frag = document.createDocumentFragment();
  for (const line of lines) {
    const cls = logClass(line);
    const span = el("span", cls, line + "\n");
    frag.appendChild(span);
  }
  pre.appendChild(frag);
  if (atBottom) pre.scrollTop = pre.scrollHeight;
}

function renderVerdict(state) {
  const v = $("verdict");
  const raw = state?.exit_code;
  // Coerce: a JSON "0" is not === 0, and the old code rendered that as
  // "FAIL · exit 0".
  const code = raw === null || raw === undefined ? null : Number(raw);

  if (Number.isFinite(code)) {
    if (code === 0) {
      v.className = "verdict pass";
      v.textContent = "PASS";
    } else {
      v.className = "verdict fail";
      v.textContent = `FAIL · exit ${code}`;
    }
  } else if (code !== null) {
    // present but not a number: it is not a success
    v.className = "verdict fail";
    v.textContent = `FAIL · exit ${raw}`;
  } else if (state?.finished_at) {
    // The run ended without recording an exit code. Claiming "RUNNING" forever
    // would be a lie; claiming PASS would be worse.
    v.className = "verdict warn";
    v.textContent = "FINISHED · no exit code";
  } else {
    v.className = "verdict running";
    v.textContent = "RUNNING";
  }

  const run = state?.run_id ? `${state.run_id}` : "hermes-update-guard";
  setWindowTitle(`${run} · ${v.textContent}`);
}

function setWindowTitle(text) {
  if (text === lastTitle) return;
  lastTitle = text;
  document.title = text;
  // invoke() returns a Promise, so a try/catch around it catches nothing — an
  // ACL rejection would surface as an unhandled rejection on every title change.
  Promise.resolve(invoke?.("set_window_title", { text })).catch(() => {});
}

/** The poll broke: say so instead of leaving a stale PASS/FAIL on screen. */
function renderStale(reason) {
  const v = $("verdict");
  v.className = "verdict warn";
  v.textContent = "STALE · cannot read state";
  $("run").textContent = String(reason).slice(0, 90);
  setWindowTitle("hermes-update-guard · STALE");
}

function render(snap) {
  $("clock").textContent = snap.now || "--:--:--";
  $("homenote").textContent = snap.home || "";

  const state = snap.state;
  if (!snap.ok || !state) {
    $("run").textContent = "waiting for a run";
    $("elapsed").textContent = "";
    renderRail(null);
    renderProfiles(null);
    renderBlockers(null);
    renderLog([]);
    const v = $("verdict");
    v.className = "verdict running";
    v.textContent = "IDLE";
    lastLogKey = "";
    lastProfilesKey = "";
    setWindowTitle("hermes-update-guard · IDLE");
    // Show what actually failed (bad path, unreadable JSON) instead of hiding it
    // behind a friendly "no run state yet".
    $("run").textContent = snap.error
      ? `no run state: ${String(snap.error).slice(0, 90)}`
      : "no run state yet";
    return;
  }

  $("run").textContent = `${state.run_id || "run"} · v${state.version || "?"}`;
  $("elapsed").textContent = humanDuration(elapsedSeconds(state));
  renderRail(state.stages);
  renderProfiles(state.profiles);
  renderBlockers(state.blockers, state.warnings);
  renderLog(snap.log || []);
  renderVerdict(state);
  mounted = true;
}

async function tick() {
  if (!invoke) {
    $("run").textContent = "Tauri API unavailable";
    return;
  }
  try {
    render(await invoke("snapshot"));
  } catch (e) {
    renderStale(e);
  }
  setTimeout(tick, POLL_MS);
}

$("pin").addEventListener("click", async () => {
  alwaysOnTop = !alwaysOnTop;
  $("pin").setAttribute("aria-pressed", String(alwaysOnTop));
  try {
    await invoke("set_always_on_top", { on: alwaysOnTop });
  } catch (e) {
    $("pin").setAttribute("aria-pressed", String(!alwaysOnTop));
    alwaysOnTop = !alwaysOnTop;
  }
});

// Escape closes the window, like every other pop-up. It is the only key that
// does anything: nothing here can touch a run.
//
// This goes through an app command rather than `plugin:window|close`: the crate
// ships no capabilities file, so a plugin call would be ACL-denied and Escape
// would silently do nothing.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    Promise.resolve(invoke?.("close_window")).catch(() => {});
  }
});

tick();
