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
  for (const name of STAGES) {
    const s = stages?.find?.((x) => x.name === name);
    const state = s?.state || "waiting";
    const chip = el("span", `stage ${state}`, name);
    rail.appendChild(chip);
  }
}

function renderProfiles(profiles) {
  const list = $("plist");
  list.textContent = "";
  $("pcount").textContent = profiles?.length ? `${profiles.length}` : "";
  if (!profiles?.length) {
    list.appendChild(el("li", "", ""));
    const li = list.lastElementChild;
    li.appendChild(el("span", "dot"));
    li.appendChild(el("span", "pname", "no profiles yet"));
    li.appendChild(el("span", "pdetail", ""));
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

function renderBlockers(blockers) {
  const box = $("blockers");
  const ul = $("blist");
  ul.textContent = "";
  if (!blockers?.length) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  for (const b of blockers) ul.appendChild(el("li", "", b));
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
    pre.appendChild(el("span", "empty", "no log yet — the guard writes backups/guard-update.log"));
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
  const code = state?.exit_code;
  if (code === null || code === undefined) {
    v.className = "verdict running";
    v.textContent = "RUNNING";
  } else if (code === 0) {
    v.className = "verdict pass";
    v.textContent = "PASS";
  } else {
    v.className = "verdict fail";
    v.textContent = `FAIL · exit ${code}`;
  }
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
    if (snap.error) $("run").textContent = "no run state yet";
    return;
  }

  $("run").textContent = `${state.run_id || "run"} · v${state.version || "?"}`;
  $("elapsed").textContent = humanDuration(elapsedSeconds(state));
  renderRail(state.stages);
  renderProfiles(state.profiles);
  renderBlockers(state.blockers);
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
    $("run").textContent = `snapshot failed: ${e}`;
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

tick();
