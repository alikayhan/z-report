import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { api, inTauri, levelLabel, Candidate, JournalEntry, Overview, PrLink, Settings } from "./api";
import "./styles.css";

type View = "review" | "journal" | "export" | "settings";

const state = {
  view: "review" as View,
  overview: null as Overview | null,
  pending: [] as Candidate[],
  discarded: [] as Candidate[],
  journal: [] as JournalEntry[],
  journalQuery: "",
  selId: null as string | null,
  draftOutcomes: null as Candidate["outcomes"] | null,
  editedIds: new Set<string>(),
  selection: new Set<string>(),
  drawerOpen: false,
  impactForId: null as string | null,
  exportPreset: "today" as "today" | "week" | "7days" | "custom",
  exportFrom: "",
  exportTo: "",
  exportPreview: "",
  exportEntries: [] as JournalEntry[],
  settings: null as Settings | null,
  lastError: "",
};

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;
const reduceMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function esc(s: string): string {
  const d = document.createElement("span");
  d.textContent = s;
  return d.innerHTML;
}

function escAttr(s: string): string {
  return esc(s).replace(/"/g, "&quot;");
}

const plural = (n: number, word: string, words = word + "s") => `${n} ${n === 1 ? word : words}`;

// Two-step confirm for destructive buttons; arming decays after 2.5 s.
function armConfirm(btn: HTMLElement, armed: string, label: string): boolean {
  if (btn.dataset.confirm === "1") {
    btn.dataset.confirm = "";
    btn.textContent = label;
    return true;
  }
  btn.dataset.confirm = "1";
  btn.textContent = armed;
  window.setTimeout(() => {
    btn.dataset.confirm = "";
    btn.textContent = label;
  }, 2500);
  return false;
}

let toastTimer: number | undefined;
function toast(msg: string) {
  const t = $("#toast");
  t.textContent = msg;
  t.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (t.hidden = true), 3000);
}

function repoName(repo: string | null): string {
  if (!repo) return "no repo";
  return repo.split("/").filter(Boolean).pop() ?? repo;
}

const dayFmt = new Intl.DateTimeFormat("en-US", { weekday: "short", month: "short", day: "numeric" });
const shortDayFmt = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });

function shortDay(day: string): string {
  const d = new Date(day + "T12:00:00");
  if (Number.isNaN(d.getTime())) return day;
  return shortDayFmt.format(d);
}

function dayHeading(day: string): string {
  const d = new Date(day + "T12:00:00");
  if (Number.isNaN(d.getTime())) return day;
  return dayFmt.format(d).replace(",", "").toUpperCase();
}

function daySpan(day: string, dayEnd: string | null): string {
  return dayEnd && dayEnd !== day ? `${dayHeading(day)} – ${dayHeading(dayEnd)}` : dayHeading(day);
}

function todayStr(): string {
  return state.overview?.today ?? new Date().toISOString().slice(0, 10);
}

function shiftDay(day: string, delta: number): string {
  const d = new Date(day + "T12:00:00");
  d.setDate(d.getDate() + delta);
  return d.toISOString().slice(0, 10);
}

function weekStart(day: string): string {
  const d = new Date(day + "T12:00:00");
  const dow = (d.getDay() + 6) % 7;
  d.setDate(d.getDate() - dow);
  return d.toISOString().slice(0, 10);
}

function relTime(iso: string | null): string {
  if (!iso) return "never";
  const mins = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 60000));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  return hours < 24 ? `${hours} h ago` : `${Math.round(hours / 24)} d ago`;
}

/* ---------- sidebar ---------- */

function renderSidebar() {
  const o = state.overview;
  const badge = $("#badge");
  if (o && o.pending > 0) {
    badge.textContent = String(o.pending);
    badge.hidden = false;
  } else {
    badge.hidden = true;
  }
  const rows: string[] = [];
  if (o) {
    rows.push(`<div class="srow"><span>Next Z-read</span><b>${esc(o.zread_time)}</b></div>`);
    rows.push(`<div class="srow"><span>Last scan</span><b>${esc(relTime(o.last_scan_at))}</b></div>`);
    rows.push(`<div class="srow"><span>Sessions</span><b>${o.session_count}</b></div>`);
    if (!o.claude_found) rows.push(`<div class="srow warn"><span>Claude CLI</span><b>missing</b></div>`);
  }
  $("#side-status").innerHTML = rows.join("");
  $("#evaluating").hidden = !o?.evaluating;
}

/* ---------- review: queue list ---------- */

function levelChip(l: number): string {
  return `<span class="level-chip l${l}">L${l} ${esc(levelLabel(l))}</span>`;
}

function renderQueue() {
  const rowsEl = $("#rows");
  let html = "";
  if (state.lastError) {
    html += `<div class="error-note">${esc(state.lastError)}</div>`;
  }
  const byDay = new Map<string, Candidate[]>();
  for (const c of state.pending) {
    byDay.set(c.day, [...(byDay.get(c.day) ?? []), c]);
  }
  for (const [day, cands] of [...byDay.entries()].sort((a, b) => b[0].localeCompare(a[0]))) {
    html += `<div class="day-head">${esc(dayHeading(day))}</div>`;
    for (const c of cands) {
      const meta = [repoName(c.repo), plural(c.session_ids.length, "session")].join(" · ");
      html += `<div class="row ${c.id === state.selId ? "active" : ""}" data-id="${escAttr(c.id)}" tabindex="0">
        <input type="checkbox" data-sel="${escAttr(c.id)}" title="Select for merge" ${state.selection.has(c.id) ? "checked" : ""}>
        <div><div class="row-title">${esc(c.title)}</div><div class="row-meta">${esc(meta)}</div></div>
        <span class="lvl-mini l${c.evidence_level}">L${c.evidence_level}</span>
      </div>`;
    }
  }
  rowsEl.innerHTML = html;
  rowsEl.classList.toggle("selecting", state.selection.size > 0);
  $("#queue-count").textContent = `Queue · ${state.pending.length} open`;

  const drawer = $("#drawer");
  drawer.hidden = state.discarded.length === 0;
  drawer.innerHTML = state.discarded.length
    ? `<button class="drawer-toggle" data-act="toggle-drawer">${state.drawerOpen ? "▾" : "▸"} Discarded (${state.discarded.length})</button>` +
      (state.drawerOpen
        ? state.discarded
            .map(
              (d) => `<div class="discarded-row"><span>${esc(d.title)}</span><button class="key-dark" data-restore="${escAttr(d.id)}">Restore</button></div>`
            )
            .join("")
        : "")
    : "";

  updateMergeBar();
}

function updateMergeBar() {
  const bar = $("#merge-bar");
  bar.hidden = state.selection.size < 2;
  if (!bar.hidden) $("#merge-count").textContent = `${state.selection.size} selected`;
}

/* ---------- review: receipt ---------- */

function barcode(): string {
  const widths = [2, 1, 3, 1, 2, 4, 1, 2, 1, 3, 2, 1, 4, 2, 1, 3, 1, 2, 4, 1, 2, 3, 1, 2, 4, 1, 2, 3, 1];
  let x = 0;
  let rects = "";
  for (const w of widths) {
    rects += `<rect x="${x}" y="0" width="${w}" height="22" fill="currentColor"/>`;
    x += w + 2;
  }
  return `<svg viewBox="0 0 ${x} 22" preserveAspectRatio="none" aria-hidden="true">${rects}</svg>`;
}

const BARCODE = barcode();

function sessionCodes(ids: string[]): string {
  const shown = ids.slice(0, 3).map((s) => s.replace(/-/g, "").slice(0, 8).toUpperCase());
  const extra = ids.length > 3 ? ` +${ids.length - 3}` : "";
  return shown.join(" · ") + extra;
}

function outcomeRow(o: { claim: string; evidence_level: number; verified: boolean }, lead?: string): string {
  const mark =
    lead ??
    (o.verified
      ? `<span class="o-mark ok" title="Verified against local facts">✓</span>`
      : `<span class="o-mark warn" title="Could not be fully verified locally">△</span>`);
  return `<div class="outcome">${mark}<span class="o-claim">${esc(o.claim)}</span><i class="o-leader"></i>${levelChip(o.evidence_level)}</div>`;
}

// Canonical form is enforced once at ingest; this guards only the href sink.
function prLinksRow(links: PrLink[]): string {
  const html = links
    .filter((pr) => pr.url.startsWith("https://"))
    .map(
      (pr) =>
        `<a class="pr-link" href="${escAttr(pr.url)}" target="_blank" rel="noopener noreferrer">PR ${esc(pr.repository)}#${esc(String(pr.number))}</a>`,
    )
    .join("");
  return html ? `<div class="pr-links">${html}</div>` : "";
}

function relatedRow(c: Candidate): string {
  const r = c.related;
  if (!r) return "";
  const merge =
    r.kind === "continuation" ? `<button class="key-ink violet" data-act="merge-related">Merge</button>` : "";
  const lead =
    r.kind === "continuation"
      ? `Looks like a continuation of <b>${esc(r.target_title)}</b> from ${esc(shortDay(r.target_day))}.`
      : `Already in your journal as <b>${esc(r.target_title)}</b> on ${esc(shortDay(r.target_day))}.`;
  return `<div class="related">
    <span class="related-text">${lead}</span>
    <span class="related-actions">${merge}<button class="key-ink" data-act="dismiss-related">Dismiss</button></span>
  </div>`;
}

function selected(): Candidate | undefined {
  return state.pending.find((c) => c.id === state.selId);
}

const editing = () => state.draftOutcomes !== null;

function stopEditing() {
  state.draftOutcomes = null;
}

// Light path for selection moves: swap the active row class and reprint the
// receipt without rebuilding the queue DOM (keyboard navigation repeats fast).
function selectCard(id: string) {
  if (state.selId === id && !editing()) return;
  state.selId = id;
  stopEditing();
  const rows = $("#rows");
  rows.querySelector(".row.active")?.classList.remove("active");
  rows.querySelector(`.row[data-id="${CSS.escape(id)}"]`)?.classList.add("active");
  renderReceipt();
}

function startEdit() {
  const c = selected();
  if (!c) return;
  state.draftOutcomes = c.outcomes.map((o) => ({ ...o }));
  renderReceipt();
  ($("#edit-title") as HTMLInputElement).focus();
}

function renderReceipt() {
  const scroll = $("#receipt-scroll");
  const bar = $("#actionbar");
  const c = selected();

  if (!c) {
    const o = state.overview;
    $("#crumb").textContent = "Queue clear";
    scroll.innerHTML = `<div class="counter-empty">
      <span class="glyph">· · · Z · · ·</span>
      <p>The till is quiet. ${
        o && o.session_count > 0
          ? `Your next Z-read closes the day at ${esc(o.zread_time)}.`
          : "Work with Claude Code as usual — achievements appear here after the daily Z-read."
      }</p>
    </div>`;
    bar.innerHTML = "";
    return;
  }

  const sub = [daySpan(c.day, c.day_end), plural(c.session_ids.length, "session"), `conf ${c.confidence.toFixed(2)}`].join(" · ");

  if (editing()) {
    const outcomes = state.draftOutcomes!;
    scroll.innerHTML = `<article class="receipt"><div class="tear top"></div><div class="paper">
      <div class="r-store">${esc(repoName(c.repo))}</div>
      <div class="r-sub">${esc(sub)} · editing</div>
      <hr class="dash">
      <input class="edit-field title" id="edit-title" value="${escAttr(c.title)}" maxlength="120">
      <div style="height:9px"></div>
      <textarea class="edit-field" id="edit-body">${esc(c.contribution)}</textarea>
      <hr class="dash">
      ${
        outcomes.length
          ? outcomes
              .map((o, i) => outcomeRow(o, `<button class="o-remove" data-rm="${i}" title="Remove this claim">✕</button>`))
              .join("")
          : `<div class="uncertainties">No outcome claims.</div>`
      }
    </div><div class="tear"></div></article>`;
    bar.innerHTML = `
      <button class="keycap violet" data-act="save-edit">Save</button>
      <button class="keycap" data-act="cancel-edit">Cancel <span class="hint">esc</span></button>`;
    return;
  }

  const idx = state.pending.findIndex((x) => x.id === c.id);
  $("#crumb").textContent = `Card ${idx + 1} of ${state.pending.length} · ${daySpan(c.day, c.day_end)}`;

  const uncertainties = c.uncertainties.length
    ? `<div class="uncertainties">${c.uncertainties.map(esc).join("<br>")}</div>`
    : "";

  scroll.innerHTML = `<article class="receipt" id="receipt">
    <div class="tear top"></div>
    <div class="paper">
      <div class="r-store">${esc(repoName(c.repo))}</div>
      <div class="r-sub">${esc(sub)}</div>
      <hr class="dash">
      <h2 class="r-title">${esc(c.title)}</h2>
      <p class="r-body">${esc(c.contribution)}</p>
      ${relatedRow(c)}
      <hr class="dash">
      ${c.outcomes.map((o) => outcomeRow(o)).join("")}
      ${uncertainties}
      ${prLinksRow(c.pr_links)}
      <hr class="dash">
      <div class="r-total">
        <span>${plural(c.outcomes.length, "outcome")}</span>
        <span class="sum">L${c.evidence_level} · ${esc(levelLabel(c.evidence_level))}</span>
      </div>
      <div class="r-code">${BARCODE}<span>${esc(sessionCodes(c.session_ids))}</span></div>
    </div>
    <div class="tear"></div>
  </article>`;

  bar.innerHTML = `
    <button class="keycap violet" data-act="approve">Approve <span class="hint">A</span></button>
    <button class="keycap" data-act="edit">Edit <span class="hint">E</span></button>
    <button class="keycap dark" data-act="discard">Discard <span class="hint">X</span></button>`;
}

function renderReview() {
  renderQueue();
  renderReceipt();
}

function stampReceipt(label: string, red: boolean, after: () => void) {
  const receipt = document.getElementById("receipt");
  if (!receipt || reduceMotion()) {
    after();
    return;
  }
  const overlay = document.createElement("div");
  overlay.className = "stamp-overlay";
  overlay.innerHTML = `<span class="seal ${red ? "red" : ""}">${label}</span>`;
  receipt.appendChild(overlay);
  window.setTimeout(after, 780);
}

// The card after the acted-on one, chosen before the queue reloads.
function nextAfter(id: string): string | null {
  const i = state.pending.findIndex((c) => c.id === id);
  return state.pending[i + 1]?.id ?? state.pending[i - 1]?.id ?? null;
}

/* ---------- journal ---------- */

function entryMarkdown(e: JournalEntry): string {
  let md = `### ${e.title}\n_${levelLabel(e.evidence_level)}_\n\n${e.contribution}\n\n`;
  md += e.outcomes.map((o) => `- ${o.verified ? "✓" : "△"} ${o.claim} _(${levelLabel(o.evidence_level)})_`).join("\n");
  for (const pr of e.pr_links) {
    md += `\n- PR ${pr.repository}#${pr.number}: ${pr.url}`;
  }
  return md;
}

function renderJournal() {
  const roll = $("#roll");
  const entries = state.journal;
  $("#journal-count").textContent = plural(entries.length, "entry", "entries");

  let inner = `<div class="roll-head">
    <div class="r-store">Z Report — approved journal</div>
    <div class="r-sub">Private · this Mac only</div>
  </div>`;

  if (!entries.length) {
    inner += `<hr class="dash"><p class="roll-empty">${
      state.journalQuery
        ? "Nothing matches that search."
        : "Approve your first achievement and it is recorded here — private, on this Mac."
    }</p>`;
  }

  let lastDay = "";
  for (const e of entries) {
    const dayLabel = daySpan(e.day, e.day_end);
    if (dayLabel !== lastDay) {
      inner += `<div class="perf"></div><div class="roll-day">· ${esc(dayLabel)} ·</div>`;
      lastDay = dayLabel;
    } else {
      inner += `<div class="perf"></div><div style="height:16px"></div>`;
    }
    const metaLeft = [repoName(e.repo), `approved ${e.approved_at.slice(0, 10)}`, e.edited ? "edited" : ""]
      .filter(Boolean)
      .join(" · ");
    const impactUi =
      state.impactForId === e.id
        ? `<div class="impact-form">
            <input class="edit-field" id="impact-note" placeholder="What real-world outcome did you observe?">
            <button class="key-ink violet" data-record="${escAttr(e.id)}">Record</button>
            <button class="key-ink" data-impact-cancel="1">Cancel</button>
          </div>`
        : "";
    inner += `<div class="entry" data-entry="${escAttr(e.id)}">
      <h2 class="r-title">${esc(e.title)}</h2>
      <p class="r-body">${esc(e.contribution)}</p>
      ${e.outcomes.map((o) => outcomeRow(o)).join("")}
      ${prLinksRow(e.pr_links)}
      <div class="entry-meta"><span>${esc(metaLeft)}</span><span>L${e.evidence_level} · ${esc(levelLabel(e.evidence_level))}</span></div>
      ${impactUi}
      <div class="entry-actions">
        ${e.evidence_level < 5 ? `<button class="key-ink" data-impact="${escAttr(e.id)}" title="Record a real-world outcome you observed">Confirm impact</button>` : ""}
        <button class="key-ink" data-copy-entry="${escAttr(e.id)}">Copy as Markdown</button>
        <button class="key-ink danger" data-delete-entry="${escAttr(e.id)}">Delete</button>
      </div>
    </div>`;
  }

  if (entries.length) inner += `<div class="roll-end">· · · end of tape · · ·</div>`;
  roll.innerHTML = `<div class="tear top"></div><div class="paper">${inner}</div><div class="tear"></div>`;
}

async function loadJournal() {
  state.journal = await api.journal("1970-01-01", todayStr(), state.journalQuery || undefined);
}

/* ---------- export ---------- */

const PRESETS: [typeof state.exportPreset, string][] = [
  ["today", "Today"],
  ["week", "This week"],
  ["7days", "Last 7 days"],
  ["custom", "Custom"],
];

function exportRange(): [string, string] {
  return presetRange(state.exportPreset);
}

function rangeLabel(from: string, to: string): string {
  return from === to ? shortDay(from) : `${shortDay(from)} – ${shortDay(to)}`;
}

function renderExport() {
  const [from, to] = exportRange();
  $("#chips").innerHTML = PRESETS.map(
    ([id, label]) =>
      `<button class="chip ${state.exportPreset === id ? "active" : ""}" data-preset="${id}"><span>${label}</span><span>${
        id === "custom" ? "" : esc(rangeLabel(...presetRange(id)))
      }</span></button>`
  ).join("");
  const custom = $("#custom-range");
  custom.hidden = state.exportPreset !== "custom";
  const fromEl = $("#export-from") as HTMLInputElement;
  const toEl = $("#export-to") as HTMLInputElement;
  fromEl.value = from;
  toEl.value = to;
  fromEl.max = todayStr();
  toEl.max = todayStr();

  $("#export-count").textContent = plural(state.exportEntries.length, "entry", "entries");
  $("#included").innerHTML = state.exportEntries.length
    ? `Included:<br>${state.exportEntries.map((e) => `— ${esc(e.title)}`).join("<br>")}`
    : "No approved achievements in this range yet.";
  $("#export-preview").textContent = state.exportPreview || "…";
}

function presetRange(preset: typeof state.exportPreset): [string, string] {
  const today = todayStr();
  switch (preset) {
    case "today":
      return [today, today];
    case "week":
      return [weekStart(today), today];
    case "7days":
      return [shiftDay(today, -6), today];
    case "custom":
      return [state.exportFrom || today, state.exportTo || today];
  }
}

async function loadExport() {
  const [from, to] = exportRange();
  try {
    [state.exportPreview, state.exportEntries] = await Promise.all([
      api.exportMarkdown(from, to),
      api.journal(from, to),
    ]);
  } catch (e) {
    state.exportPreview = String(e);
    state.exportEntries = [];
  }
}

/* ---------- settings ---------- */

let runsCache = "";

function renderSettings() {
  const s = state.settings;
  if (!s) return;
  const o = state.overview;
  const metered = o?.metered ?? false;
  $("#settings-col").innerHTML = `
    <div class="settings-section">
      <h3>Schedule</h3>
      <div class="setting-row">
        <label for="set-zread">Daily Z-read at</label>
        <input type="time" id="set-zread" value="${escAttr(s.zread_time)}">
      </div>
      <div class="setting-row">
        <label for="set-scan">Collect evidence every</label>
        <input type="number" id="set-scan" min="5" max="240" value="${s.scan_interval_min}">
      </div>
      <p class="setting-hint">Minutes between local transcript scans. Scanning is local only — nothing is sent anywhere.</p>
    </div>

    <div class="settings-section">
      <h3>Privacy</h3>
      <div class="setting-row">
        <label for="set-prompts">Keep prompt excerpts in evidence</label>
        <input type="checkbox" id="set-prompts" ${s.retain_prompts ? "checked" : ""}>
      </div>
      <p class="setting-hint">Off = only metadata (files, commands, commits) is stored and shown to the evaluator. Full transcripts are never copied; Z Report only references the session files Claude Code already keeps.</p>
      <div class="setting-row">
        <label for="set-retention">Keep unreviewed candidates for</label>
        <input type="number" id="set-retention" min="0" max="3650" value="${s.retention_days}">
      </div>
      <p class="setting-hint">Days. 0 keeps them forever. Approved journal entries are always kept. Session evidence is never deleted — anything outside the 15-day evaluation window is simply ignored.</p>
      <label class="setting-hint" for="set-repos" style="display:block">Excluded repositories (one path per line)</label>
      <textarea class="repos" id="set-repos">${esc(s.excluded_repos.join("\n"))}</textarea>
    </div>

    <div class="settings-section">
      <h3>Evaluator</h3>
      <div class="setting-row">
        <label>Model</label>
        <span style="font-family:var(--mono);font-size:11px">${esc(o?.model ?? "claude-opus-5")} · xhigh</span>
      </div>
      <div class="setting-row">
        <label for="set-cost-limit">${metered ? "Stop a run past a $5 cost limit" : "Stop a run past a high usage limit"}</label>
        <input type="checkbox" id="set-cost-limit" ${s.cost_limit_enabled ? "checked" : ""}>
      </div>
      <p class="setting-hint">A safety valve on any single evaluation. Turn off to let a run finish no matter how large (uncapped).</p>
      <div class="boundary-note">
        <strong>What leaves this Mac:</strong> evaluation runs on your own Claude Code
        account and sends the prepared evidence package (session excerpts, file paths,
        command results including those from delegated sub-sessions, the names of
        external tools you used to change something, commit and pull request
        metadata) to Anthropic — the same boundary as using Claude Code itself. ${o?.claude_found ? "" : "<strong>Claude Code CLI was not found — install it or set its path below.</strong>"}
        Z Report has no backend, no analytics, and no telemetry of its own.
      </div>
    </div>

    <div class="settings-section">
      <h3>Recent evaluations</h3>
      ${runsCache || `<p class="setting-hint">No evaluations yet.</p>`}
    </div>

    <div class="settings-section">
      <h3>Danger</h3>
      <div class="setting-row">
        <label>Erase evidence, candidates, journal, and settings</label>
        <button class="key-dark danger" data-act="delete-all">Delete all data</button>
      </div>
    </div>`;

  const bind = (id: string, apply: (v: string) => void) => {
    const el = document.getElementById(id) as HTMLInputElement | HTMLTextAreaElement;
    el.addEventListener("change", async () => {
      apply((el as HTMLInputElement).type === "checkbox" ? String((el as HTMLInputElement).checked) : el.value);
      await api.setSettings(state.settings!);
      state.overview = await api.overview();
      renderSidebar();
    });
  };
  bind("set-zread", (v) => (state.settings!.zread_time = v || "18:00"));
  bind("set-scan", (v) => (state.settings!.scan_interval_min = Math.max(5, Number(v) || 30)));
  bind("set-prompts", (v) => (state.settings!.retain_prompts = v === "true"));
  bind("set-retention", (v) => (state.settings!.retention_days = Math.max(0, Number(v) || 0)));
  bind("set-repos", (v) =>
    (state.settings!.excluded_repos = v.split("\n").map((l) => l.trim()).filter(Boolean))
  );
  bind("set-cost-limit", (v) => (state.settings!.cost_limit_enabled = v === "true"));
}

async function loadRuns() {
  const runs = await api.evalRuns();
  if (runs.length === 0) {
    runsCache = "";
    return;
  }
  const metered = state.overview?.metered ?? false;
  runsCache = `<table class="runs-table">
    <tr><th>Day</th><th>Kind</th><th>Model</th>${metered ? "<th>Cost</th>" : ""}<th>Found</th><th></th></tr>
    ${runs
      .slice(0, 8)
      .map(
        (r) => `<tr>
          <td>${esc(r.day.slice(5))}</td>
          <td>${esc(r.kind)}</td>
          <td>${esc(r.model ?? "—")}</td>
          ${metered ? `<td>${r.cost_usd != null ? "$" + r.cost_usd.toFixed(2) : "—"}</td>` : ""}
          <td>${r.candidate_count}</td>
          <td class="${r.status === "ok" ? "ok" : "err"}" title="${escAttr(r.error ?? "")}">${esc(r.status)}</td>
        </tr>`
      )
      .join("")}
  </table>${metered ? "" : `<p class="setting-hint">Runs are included in your Claude subscription — no per-run charge.</p>`}`;
}

/* ---------- actions ---------- */

function resolveSelected(label: string, red: boolean, act: (id: string) => Promise<void>, msg: string) {
  const c = selected();
  if (!c || editing()) return;
  const next = nextAfter(c.id);
  stampReceipt(label, red, async () => {
    await act(c.id);
    state.selId = next;
    await refreshAll();
    renderReview();
    toast(msg);
  });
}

const approveSelected = () =>
  resolveSelected(
    "APPROVED",
    false,
    async (id) => {
      await api.approve(id, state.editedIds.has(id));
      state.editedIds.delete(id);
    },
    "Approved — filed to your journal",
  );

const discardSelected = () =>
  resolveSelected("DISCARDED", true, (id) => api.discard(id), "Discarded — restore it from the drawer below the queue");

async function mergeCards(ids: string[]) {
  const newId = await api.merge(ids);
  state.selection.clear();
  stopEditing();
  await refreshAll();
  state.selId = newId;
  renderReview();
  toast("Merged into one card — the summary is being rewritten in the background");
}

async function handleAct(act: string, target: HTMLElement) {
  const c = selected();
  switch (act) {
    case "approve":
      await approveSelected();
      break;
    case "discard":
      await discardSelected();
      break;
    case "edit":
      startEdit();
      break;
    case "cancel-edit":
      stopEditing();
      renderReceipt();
      break;
    case "save-edit": {
      if (!c) break;
      const title = ($("#edit-title") as HTMLInputElement).value.trim();
      const contribution = ($("#edit-body") as HTMLTextAreaElement).value.trim();
      if (title) {
        await api.updateCandidate(c.id, title, contribution, state.draftOutcomes ?? c.outcomes);
        state.editedIds.add(c.id);
      }
      stopEditing();
      await refreshAll();
      renderReview();
      break;
    }
    case "merge-related":
      if (c?.related) await mergeCards([c.related.target_id, c.id]);
      break;
    case "dismiss-related":
      if (!c) break;
      await api.dismissRelated(c.id);
      await refreshAll();
      renderReview();
      toast("Suggestion dismissed — it stays dismissed for these sessions");
      break;
    case "delete-all": {
      if (!armConfirm(target, "Click again to erase everything", "Delete all data")) break;
      await api.deleteAllData();
      state.settings = await api.getSettings();
      await loadRuns();
      await refreshAll();
      render();
      break;
    }
  }
}

/* ---------- data loading ---------- */

async function loadReview() {
  [state.pending, state.discarded] = await Promise.all([
    api.candidates("pending"),
    api.candidates("discarded"),
  ]);
  state.selection = new Set([...state.selection].filter((id) => state.pending.some((c) => c.id === id)));
  if (!state.pending.some((c) => c.id === state.selId)) {
    state.selId = state.pending[0]?.id ?? null;
    stopEditing();
  }
}

async function refreshAll() {
  state.overview = await api.overview();
  await loadReview();
  renderSidebar();
}

function render() {
  switch (state.view) {
    case "review":
      renderReview();
      break;
    case "journal":
      renderJournal();
      break;
    case "export":
      renderExport();
      break;
    case "settings":
      renderSettings();
      break;
  }
}

async function loadView(view: View) {
  if (view === "journal") await loadJournal();
  if (view === "export") await loadExport();
  if (view === "settings") {
    state.settings = await api.getSettings();
    await loadRuns();
  }
}

async function switchView(view: View) {
  state.view = view;
  document.querySelectorAll<HTMLButtonElement>(".nav-item").forEach((b) => {
    b.classList.toggle("active", b.dataset.view === view);
  });
  document.querySelectorAll<HTMLElement>(".view").forEach((v) => {
    v.classList.toggle("active", v.id === "view-" + view);
  });
  await loadView(view);
  render();
}

/* ---------- boot ---------- */

async function boot() {
  document.querySelectorAll<HTMLButtonElement>(".nav-item").forEach((b) => {
    b.addEventListener("click", () => void switchView(b.dataset.view as View));
  });

  $("#rows").addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    if (target.closest("[data-sel]")) return;
    const row = target.closest<HTMLElement>("[data-id]");
    if (row) selectCard(row.dataset.id!);
  });
  $("#rows").addEventListener("change", (e) => {
    const box = (e.target as HTMLElement).closest<HTMLInputElement>("[data-sel]");
    if (!box) return;
    const id = box.dataset.sel!;
    if (state.selection.has(id)) state.selection.delete(id);
    else state.selection.add(id);
    $("#rows").classList.toggle("selecting", state.selection.size > 0);
    updateMergeBar();
  });

  $("#drawer").addEventListener("click", async (e) => {
    const target = e.target as HTMLElement;
    if (target.closest("[data-act='toggle-drawer']")) {
      state.drawerOpen = !state.drawerOpen;
      renderQueue();
      return;
    }
    const restore = target.closest<HTMLElement>("[data-restore]");
    if (restore) {
      const id = restore.dataset.restore!;
      await api.restore(id);
      stopEditing();
      await refreshAll();
      state.selId = id;
      renderReview();
      toast("Restored to the queue");
    }
  });

  const actHandler = (e: Event) => {
    const actEl = (e.target as HTMLElement).closest<HTMLElement>("[data-act]");
    if (actEl) void handleAct(actEl.dataset.act!, actEl);
  };
  $("#actionbar").addEventListener("click", actHandler);
  $("#receipt-scroll").addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    const rm = target.closest<HTMLElement>("[data-rm]");
    if (rm && state.draftOutcomes) {
      state.draftOutcomes.splice(Number(rm.dataset.rm), 1);
      renderReceipt();
      return;
    }
    actHandler(e);
  });

  $("#btn-merge").addEventListener("click", async () => {
    const ids = [...state.selection];
    if (ids.length >= 2) await mergeCards(ids);
  });
  $("#btn-merge-cancel").addEventListener("click", () => {
    state.selection.clear();
    renderQueue();
  });

  $("#btn-xread").addEventListener("click", async () => {
    state.lastError = "";
    try {
      await api.runXread();
      if (state.overview) state.overview.evaluating = true;
      renderSidebar();
    } catch (e) {
      state.lastError = String(e);
      renderQueue();
    }
  });

  let searchTimer: number | undefined;
  $("#journal-search").addEventListener("input", (e) => {
    window.clearTimeout(searchTimer);
    searchTimer = window.setTimeout(async () => {
      state.journalQuery = (e.target as HTMLInputElement).value;
      await loadJournal();
      renderJournal();
    }, 220);
  });

  $("#roll").addEventListener("click", async (e) => {
    const target = e.target as HTMLElement;
    const impact = target.closest<HTMLElement>("[data-impact]");
    if (impact) {
      state.impactForId = impact.dataset.impact!;
      renderJournal();
      ($("#impact-note") as HTMLInputElement | null)?.focus();
      return;
    }
    if (target.closest("[data-impact-cancel]")) {
      state.impactForId = null;
      renderJournal();
      return;
    }
    const record = target.closest<HTMLElement>("[data-record]");
    if (record) {
      const note = ($("#impact-note") as HTMLInputElement).value.trim();
      await api.confirmImpact(record.dataset.record!, note);
      state.impactForId = null;
      await loadJournal();
      renderJournal();
      toast("Impact recorded — this entry is now at level 5");
      return;
    }
    const copy = target.closest<HTMLElement>("[data-copy-entry]");
    if (copy) {
      const entry = state.journal.find((x) => x.id === copy.dataset.copyEntry);
      if (entry) {
        await navigator.clipboard.writeText(entryMarkdown(entry));
        toast("Copied");
      }
      return;
    }
    const del = target.closest<HTMLElement>("[data-delete-entry]");
    if (del) {
      if (!armConfirm(del, "Click again to delete", "Delete")) return;
      await api.deleteJournalEntry(del.dataset.deleteEntry!);
      await loadJournal();
      renderJournal();
    }
  });
  $("#roll").addEventListener("keydown", (e) => {
    if ((e.target as HTMLElement).id === "impact-note" && e.key === "Enter") {
      ($("#roll").querySelector("[data-record]") as HTMLElement | null)?.click();
    }
  });

  $("#chips").addEventListener("click", async (e) => {
    const chip = (e.target as HTMLElement).closest<HTMLElement>("[data-preset]");
    if (!chip) return;
    state.exportPreset = chip.dataset.preset as typeof state.exportPreset;
    await loadExport();
    renderExport();
  });
  for (const id of ["#export-from", "#export-to"]) {
    $(id).addEventListener("change", async () => {
      state.exportFrom = ($("#export-from") as HTMLInputElement).value;
      state.exportTo = ($("#export-to") as HTMLInputElement).value;
      await loadExport();
      renderExport();
    });
  }
  $("#btn-copy").addEventListener("click", async () => {
    await navigator.clipboard.writeText(state.exportPreview);
    toast("Copied");
  });
  $("#btn-save").addEventListener("click", async () => {
    if (!inTauri) {
      toast("Saving to a file is available in the app");
      return;
    }
    const [from, to] = exportRange();
    const path = await save({
      defaultPath: from === to ? `z-report-${from}.md` : `z-report-${from}-to-${to}.md`,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (path) {
      await api.writeFile(path, state.exportPreview);
      toast(`Saved ${path.split("/").pop()}`);
    }
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && editing()) {
      stopEditing();
      renderReceipt();
      return;
    }
    if ((e.target as HTMLElement).matches("input, textarea")) return;
    if (state.view !== "review") return;
    const delta = e.key === "j" || e.key === "ArrowDown" ? 1 : e.key === "k" || e.key === "ArrowUp" ? -1 : 0;
    if (delta !== 0) {
      const i = state.pending.findIndex((c) => c.id === state.selId);
      const next = state.pending[Math.min(Math.max(i + delta, 0), state.pending.length - 1)];
      if (next) selectCard(next.id);
      e.preventDefault();
      return;
    }
    if (e.key === "a") approveSelected();
    else if (e.key === "x") discardSelected();
    else if (e.key === "e" && !editing()) startEdit();
  });

  $("#settings-col").addEventListener("click", actHandler);

  if (inTauri) {
    await listen("zr:refresh", async () => {
      await refreshAll();
      await loadView(state.view);
      render();
    });
    await listen<boolean>("zr:evaluating", (e) => {
      if (state.overview) state.overview.evaluating = e.payload;
      renderSidebar();
    });
  }

  await refreshAll();
  const hash = location.hash.slice(1);
  if (hash === "journal" || hash === "export" || hash === "settings") {
    await switchView(hash);
  } else {
    render();
  }
  window.setInterval(async () => {
    state.overview = await api.overview();
    renderSidebar();
  }, 60_000);
}

void boot();
