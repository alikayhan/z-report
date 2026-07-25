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
  editedIds: new Set<string>(),
  editingId: null as string | null,
  selection: new Set<string>(),
  drawerOpen: false,
  exportPreset: "today" as "today" | "week" | "7days" | "custom",
  exportFrom: "",
  exportTo: "",
  exportPreview: "",
  settings: null as Settings | null,
  pinned: false,
  lastError: "",
};

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;
const content = () => $("#content");
const reduceMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function esc(s: string): string {
  const d = document.createElement("span");
  d.textContent = s;
  return d.innerHTML;
}

function escAttr(s: string): string {
  return esc(s).replace(/"/g, "&quot;");
}

function repoName(repo: string | null): string {
  if (!repo) return "no repo";
  return repo.split("/").filter(Boolean).pop() ?? repo;
}

function dayHeading(day: string): string {
  const d = new Date(day + "T12:00:00");
  if (Number.isNaN(d.getTime())) return day;
  const fmt = new Intl.DateTimeFormat("en-US", { weekday: "short", month: "short", day: "numeric" });
  return fmt.format(d).replace(",", " ·").toUpperCase();
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

/* ---------- header ---------- */

function renderHeader() {
  const o = state.overview;
  const parts: string[] = [dayHeading(todayStr())];
  if (o) {
    parts.push(o.pending === 0 ? "queue clear" : `${o.pending} to review`);
    parts.push(`z-read ${o.zread_time}`);
    if (!o.claude_found) parts.push("claude cli missing");
  }
  $("#status-line").textContent = parts.join(" · ").toUpperCase();
  $("#evaluating").hidden = !o?.evaluating;
  const badge = $("#badge-pending");
  if (o && o.pending > 0) {
    badge.textContent = String(o.pending);
    badge.hidden = false;
  } else {
    badge.hidden = true;
  }
  $("#footer-model").textContent = o ? o.model : "";
}

/* ---------- review ---------- */

function outcomeRow(o: { claim: string; evidence_level: number; verified: boolean }): string {
  const mark = o.verified
    ? `<span class="outcome-mark ok" title="Verified against local facts">✓</span>`
    : `<span class="outcome-mark warn" title="Could not be fully verified locally">△</span>`;
  return `<div class="outcome">${mark}
    <span class="outcome-claim">${esc(o.claim)}</span>
    <span class="level-chip l${o.evidence_level}">L${o.evidence_level} ${esc(levelLabel(o.evidence_level))}</span>
  </div>`;
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

function candidateCard(c: Candidate): string {
  if (state.editingId === c.id) return candidateEditCard(c);
  const selected = state.selection.has(c.id);
  const outcomes = c.outcomes.map(outcomeRow).join("");
  const uncertainties = c.uncertainties.length
    ? `<div class="uncertainties">${c.uncertainties.map((u) => esc(u)).join("<br>")}</div>`
    : "";
  return `<article class="receipt ${selected ? "selected" : ""}" data-id="${c.id}">
    <div class="receipt-meta">
      <span>${esc(repoName(c.repo))}</span>
      <span>conf ${c.confidence.toFixed(2)}</span>
    </div>
    <h2 class="receipt-title">${esc(c.title)}</h2>
    <p class="receipt-body">${esc(c.contribution)}</p>
    <hr class="receipt-rule">
    ${outcomes}
    ${uncertainties}
    ${prLinksRow(c.pr_links)}
    <hr class="receipt-rule">
    <div class="receipt-total">
      <span>${c.session_ids.length} session${c.session_ids.length === 1 ? "" : "s"}</span>
      <span>L${c.evidence_level} ${esc(levelLabel(c.evidence_level))}</span>
    </div>
    <div class="receipt-actions">
      <button class="key key-primary" data-act="approve">Approve</button>
      <button class="key" data-act="edit">Edit</button>
      <button class="key key-danger" data-act="discard">Discard</button>
      <input type="checkbox" class="select-box" data-act="select" title="Select for merge" ${selected ? "checked" : ""}>
    </div>
  </article>`;
}

function candidateEditCard(c: Candidate): string {
  const outcomes = c.outcomes
    .map(
      (o, i) => `<div class="outcome">
        <button class="outcome-remove" data-act="remove-outcome" data-idx="${i}" title="Remove this claim">✕</button>
        <span class="outcome-claim">${esc(o.claim)}</span>
        <span class="level-chip l${o.evidence_level}">L${o.evidence_level}</span>
      </div>`
    )
    .join("");
  return `<article class="receipt" data-id="${c.id}" data-editing="1">
    <div class="receipt-meta"><span>${esc(repoName(c.repo))}</span><span>editing</span></div>
    <input class="edit-field title" data-field="title" value="${esc(c.title)}" maxlength="120">
    <div style="height:8px"></div>
    <textarea class="edit-field" data-field="contribution">${esc(c.contribution)}</textarea>
    <hr class="receipt-rule">
    ${outcomes || `<div class="uncertainties">No outcome claims.</div>`}
    <div class="receipt-actions">
      <button class="key key-primary" data-act="save-edit">Save</button>
      <button class="key key-quiet" data-act="cancel-edit">Cancel</button>
    </div>
  </article>`;
}

function renderReview() {
  const c = content();
  let html = "";
  if (state.lastError) {
    html += `<div class="error-note">${esc(state.lastError)}</div>`;
  }
  if (state.pending.length === 0) {
    const o = state.overview;
    html += `<div class="empty">
      <span class="glyph">· · · Z · · ·</span>
      <p>The till is quiet. ${
        o && o.pending === 0 && o.session_count > 0
          ? `Your next Z-read closes the day at ${esc(o.zread_time)}.`
          : "Work with Claude Code as usual — achievements appear here after the daily Z-read."
      }</p>
      <button class="key-dark" data-act="xread">Run X-read now</button>
    </div>`;
  } else {
    const byDay = new Map<string, Candidate[]>();
    for (const cand of state.pending) {
      byDay.set(cand.day, [...(byDay.get(cand.day) ?? []), cand]);
    }
    for (const [day, cands] of [...byDay.entries()].sort((a, b) => b[0].localeCompare(a[0]))) {
      html += `<section class="day-group"><div class="day-head">${dayHeading(day)}</div>`;
      html += cands.map(candidateCard).join("");
      html += `</section>`;
    }
    html += `<div style="text-align:center;margin-top:4px">
      <button class="key-dark" data-act="xread">Run X-read now</button>
    </div>`;
  }
  if (state.discarded.length > 0) {
    html += `<div class="drawer">
      <button class="drawer-toggle" data-act="toggle-drawer">${state.drawerOpen ? "▾" : "▸"} Discarded (${state.discarded.length})</button>
      ${
        state.drawerOpen
          ? state.discarded
              .map(
                (d) => `<div class="discarded-row" data-id="${d.id}">
                  <span class="title">${esc(d.title)}</span>
                  <button class="key-dark" data-act="restore">Restore</button>
                </div>`
              )
              .join("")
          : ""
      }
    </div>`;
  }
  c.innerHTML = html;
  updateMergeBar();
}

/* ---------- journal ---------- */

function journalCard(e: JournalEntry): string {
  const outcomes = e.outcomes.map(outcomeRow).join("");
  return `<article class="receipt journal-entry" data-id="${e.id}">
    <div class="receipt-meta">
      <span>${esc(repoName(e.repo))}</span>
      <span>${esc(e.day)}${e.edited ? " · edited" : ""}</span>
    </div>
    <h2 class="receipt-title">${esc(e.title)}</h2>
    <p class="receipt-body">${esc(e.contribution)}</p>
    ${outcomes ? `<hr class="receipt-rule">${outcomes}` : ""}
    ${prLinksRow(e.pr_links)}
    <hr class="receipt-rule">
    <div class="receipt-total">
      <span>approved ${esc(e.approved_at.slice(0, 10))}</span>
      <span>L${e.evidence_level} ${esc(levelLabel(e.evidence_level))}</span>
    </div>
    <div class="journal-actions">
      ${
        e.evidence_level < 5
          ? `<button class="key" data-act="impact" title="Record a real-world outcome you observed">Confirm impact</button>`
          : ""
      }
      <button class="key key-danger" data-act="delete-entry">Delete</button>
    </div>
  </article>`;
}

function renderJournal() {
  const c = content();
  let html = `<div class="searchbar">
    <input type="search" id="journal-search" placeholder="Search your journal" value="${esc(state.journalQuery)}">
  </div>`;
  if (state.journal.length === 0) {
    html += `<div class="empty">
      <span class="glyph">· · · Z · · ·</span>
      <p>${state.journalQuery ? "Nothing matches that search." : "Approve your first achievement and it is recorded here — private, on this Mac."}</p>
    </div>`;
  } else {
    const byDay = new Map<string, JournalEntry[]>();
    for (const e of state.journal) {
      byDay.set(e.day, [...(byDay.get(e.day) ?? []), e]);
    }
    for (const [day, entries] of byDay) {
      html += `<section class="day-group"><div class="day-head">${dayHeading(day)}</div>`;
      html += entries.map(journalCard).join("");
      html += `</section>`;
    }
  }
  c.innerHTML = html;
  const search = $("#journal-search") as HTMLInputElement;
  let t: number | undefined;
  search.addEventListener("input", () => {
    window.clearTimeout(t);
    t = window.setTimeout(async () => {
      state.journalQuery = search.value;
      await loadJournal();
      renderJournal();
      ($("#journal-search") as HTMLInputElement).focus();
    }, 220);
  });
}

/* ---------- export ---------- */

function exportRange(): [string, string] {
  const today = todayStr();
  switch (state.exportPreset) {
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

function renderExport() {
  const [from, to] = exportRange();
  const presets: [string, string][] = [
    ["today", "Today"],
    ["week", "This week"],
    ["7days", "Last 7 days"],
    ["custom", "Custom"],
  ];
  content().innerHTML = `
    <div class="range-chips">
      ${presets
        .map(
          ([id, label]) =>
            `<button class="chip ${state.exportPreset === id ? "active" : ""}" data-preset="${id}">${label}</button>`
        )
        .join("")}
    </div>
    ${
      state.exportPreset === "custom"
        ? `<div class="range-inputs">
            <input type="date" id="export-from" value="${esc(from)}" max="${esc(todayStr())}">
            <span>to</span>
            <input type="date" id="export-to" value="${esc(to)}" max="${esc(todayStr())}">
          </div>`
        : ""
    }
    <div class="export-preview" id="export-preview">${esc(state.exportPreview || "…")}</div>
    <div class="export-tear"></div>
    <div class="export-actions">
      <button class="key-dark" data-act="copy-export">Copy Markdown</button>
      <button class="key-dark" data-act="save-export">Save to file…</button>
      <span id="export-flash" style="align-self:center;font-family:var(--mono);font-size:10px;letter-spacing:.1em;color:var(--verified-dark)"></span>
    </div>`;
  content()
    .querySelectorAll<HTMLButtonElement>("[data-preset]")
    .forEach((b) =>
      b.addEventListener("click", async () => {
        state.exportPreset = b.dataset.preset as typeof state.exportPreset;
        await loadExportPreview();
        renderExport();
      })
    );
  const fromEl = document.getElementById("export-from") as HTMLInputElement | null;
  const toEl = document.getElementById("export-to") as HTMLInputElement | null;
  for (const el of [fromEl, toEl]) {
    el?.addEventListener("change", async () => {
      state.exportFrom = fromEl?.value ?? "";
      state.exportTo = toEl?.value ?? "";
      await loadExportPreview();
      renderExport();
    });
  }
}

async function loadExportPreview() {
  const [from, to] = exportRange();
  try {
    state.exportPreview = await api.exportMarkdown(from, to);
  } catch (e) {
    state.exportPreview = String(e);
  }
}

/* ---------- settings ---------- */

function renderSettings() {
  const s = state.settings;
  if (!s) return;
  const o = state.overview;
  const metered = o?.metered ?? false;
  content().innerHTML = `
    <div class="settings-section">
      <h3>Schedule</h3>
      <div class="setting-row">
        <label for="set-zread">Daily Z-read at</label>
        <input type="time" id="set-zread" value="${esc(s.zread_time)}">
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
        command results including those from delegated sub-sessions, commit and pull
        request metadata) to Anthropic — the same boundary as using
        Claude Code itself. ${o?.claude_found ? "" : "<strong>Claude Code CLI was not found — install it or set its path below.</strong>"}
        Z Report has no backend, no analytics, and no telemetry of its own.
      </div>
    </div>

    <div class="settings-section">
      <h3>Recent evaluations</h3>
      ${renderRuns()}
    </div>

    <div class="settings-section">
      <h3>Danger</h3>
      <div class="setting-row">
        <label>Erase evidence, candidates, journal, and settings</label>
        <button class="key-dark" data-act="delete-all" style="color:var(--register-red-dark);border-color:rgba(178,58,50,.4)">Delete all data</button>
      </div>
    </div>`;

  const bind = (id: string, apply: (v: string) => void) => {
    const el = document.getElementById(id) as HTMLInputElement | HTMLTextAreaElement;
    el.addEventListener("change", async () => {
      apply((el as HTMLInputElement).type === "checkbox" ? String((el as HTMLInputElement).checked) : el.value);
      await api.setSettings(state.settings!);
      state.overview = await api.overview();
      renderHeader();
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

let runsCache = "";
function renderRuns(): string {
  return runsCache || `<p class="setting-hint">No evaluations yet.</p>`;
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
          <td class="${r.status === "ok" ? "ok" : "err"}" title="${esc(r.error ?? "")}">${esc(r.status)}</td>
        </tr>`
      )
      .join("")}
  </table>${metered ? "" : `<p class="setting-hint">Runs are included in your Claude subscription — no per-run charge.</p>`}`;
}

/* ---------- merge bar ---------- */

function updateMergeBar() {
  const bar = $("#merge-bar");
  const n = state.selection.size;
  bar.hidden = n < 2;
  if (n >= 2) $("#merge-count").textContent = `${n} selected`;
}

/* ---------- actions ---------- */

function stampAndRemove(card: HTMLElement, label: string, red: boolean, after: () => void) {
  if (reduceMotion()) {
    after();
    return;
  }
  const overlay = document.createElement("div");
  overlay.className = "stamp-overlay";
  overlay.innerHTML = `<span class="stamp-seal ${red ? "red" : ""}">${label}</span>`;
  card.appendChild(overlay);
  card.classList.add("leaving");
  window.setTimeout(after, 760);
}

async function handleAction(act: string, card: HTMLElement | null, target: HTMLElement) {
  const id = card?.dataset.id ?? "";
  switch (act) {
    case "xread": {
      state.lastError = "";
      await api.runXread();
      state.overview = await api.overview();
      if (state.overview) state.overview.evaluating = true;
      renderHeader();
      break;
    }
    case "approve": {
      if (!card) break;
      stampAndRemove(card, "APPROVED", false, async () => {
        await api.approve(id, state.editedIds.has(id));
        await refreshAll();
      });
      break;
    }
    case "discard": {
      if (!card) break;
      stampAndRemove(card, "DISCARDED", true, async () => {
        await api.discard(id);
        await refreshAll();
      });
      break;
    }
    case "restore": {
      await api.restore(id);
      await refreshAll();
      break;
    }
    case "edit": {
      state.editingId = id;
      renderReview();
      break;
    }
    case "cancel-edit": {
      state.editingId = null;
      renderReview();
      break;
    }
    case "save-edit": {
      if (!card) break;
      const cand = state.pending.find((c) => c.id === id);
      if (!cand) break;
      const title = (card.querySelector('[data-field="title"]') as HTMLInputElement).value.trim();
      const contribution = (card.querySelector('[data-field="contribution"]') as HTMLTextAreaElement).value.trim();
      if (title) {
        await api.updateCandidate(id, title, contribution, cand.outcomes);
        state.editedIds.add(id);
      }
      state.editingId = null;
      await refreshAll();
      break;
    }
    case "remove-outcome": {
      const cand = state.pending.find((c) => c.id === id);
      const idx = Number(target.dataset.idx);
      if (cand && !Number.isNaN(idx)) {
        cand.outcomes.splice(idx, 1);
        state.editedIds.add(id);
        renderReview();
      }
      break;
    }
    case "select": {
      if (state.selection.has(id)) state.selection.delete(id);
      else state.selection.add(id);
      renderReview();
      break;
    }
    case "toggle-drawer": {
      state.drawerOpen = !state.drawerOpen;
      renderReview();
      break;
    }
    case "impact": {
      const note = window.prompt("What real-world outcome did you observe?", "");
      if (note === null) break;
      await api.confirmImpact(id, note.trim());
      await loadJournal();
      renderJournal();
      break;
    }
    case "delete-entry": {
      if (target.dataset.confirm !== "1") {
        target.dataset.confirm = "1";
        target.textContent = "Click again to delete";
        window.setTimeout(() => {
          target.dataset.confirm = "";
          target.textContent = "Delete";
        }, 2500);
        break;
      }
      await api.deleteJournalEntry(id);
      await loadJournal();
      renderJournal();
      break;
    }
    case "copy-export": {
      await navigator.clipboard.writeText(state.exportPreview);
      flashExport("copied");
      break;
    }
    case "save-export": {
      const [from, to] = exportRange();
      await api.setPinned(true);
      try {
        const path = await save({
          defaultPath: from === to ? `z-report-${from}.md` : `z-report-${from}-to-${to}.md`,
          filters: [{ name: "Markdown", extensions: ["md"] }],
        });
        if (path) {
          await api.writeFile(path, state.exportPreview);
          flashExport("saved");
        }
      } finally {
        if (!state.pinned) await api.setPinned(false);
      }
      break;
    }
    case "delete-all": {
      if (target.dataset.confirm !== "1") {
        target.dataset.confirm = "1";
        target.textContent = "Click again to erase everything";
        window.setTimeout(() => {
          target.dataset.confirm = "";
          target.textContent = "Delete all data";
        }, 3000);
        break;
      }
      await api.deleteAllData();
      state.settings = await api.getSettings();
      await refreshAll();
      break;
    }
  }
}

function flashExport(msg: string) {
  const el = document.getElementById("export-flash");
  if (!el) return;
  el.textContent = msg.toUpperCase();
  window.setTimeout(() => (el.textContent = ""), 1800);
}

/* ---------- data loading ---------- */

async function loadReview() {
  [state.pending, state.discarded] = await Promise.all([
    api.candidates("pending"),
    api.candidates("discarded"),
  ]);
  state.selection = new Set([...state.selection].filter((id) => state.pending.some((c) => c.id === id)));
}

async function loadJournal() {
  state.journal = await api.journal("1970-01-01", todayStr(), state.journalQuery || undefined);
}

async function refreshAll() {
  state.overview = await api.overview();
  await loadReview();
  renderHeader();
  render();
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

async function switchView(view: View) {
  state.view = view;
  document.querySelectorAll<HTMLButtonElement>(".tab").forEach((t) => {
    t.classList.toggle("active", t.dataset.view === view);
  });
  if (view === "journal") await loadJournal();
  if (view === "export") await loadExportPreview();
  if (view === "settings") {
    state.settings = await api.getSettings();
    await loadRuns();
  }
  render();
}

/* ---------- boot ---------- */

async function boot() {
  document.querySelectorAll<HTMLButtonElement>(".tab").forEach((t) => {
    t.addEventListener("click", () => switchView(t.dataset.view as View));
  });
  $("#btn-settings").addEventListener("click", () => switchView("settings"));
  $("#btn-pin").addEventListener("click", async () => {
    state.pinned = !state.pinned;
    await api.setPinned(state.pinned);
    $("#btn-pin").setAttribute("aria-pressed", String(state.pinned));
  });

  content().addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    const actEl = target.closest<HTMLElement>("[data-act]");
    if (!actEl || actEl.dataset.act === "select") return;
    const card = actEl.closest<HTMLElement>("[data-id]");
    void handleAction(actEl.dataset.act!, card, actEl);
  });
  content().addEventListener("change", (e) => {
    const target = e.target as HTMLElement;
    if (target.matches('[data-act="select"]')) {
      const card = target.closest<HTMLElement>("[data-id]");
      if (card) void handleAction("select", card, target);
    }
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      if (state.editingId) {
        state.editingId = null;
        renderReview();
      } else {
        void api.hideWindow();
      }
    }
  });

  $("#btn-merge").addEventListener("click", async () => {
    const ids = [...state.selection];
    if (ids.length < 2) return;
    await api.merge(ids);
    state.selection.clear();
    await refreshAll();
  });
  $("#btn-merge-cancel").addEventListener("click", () => {
    state.selection.clear();
    renderReview();
  });

  if (inTauri) {
    await listen("zr:refresh", async () => {
      await refreshAll();
      if (state.view === "export") {
        await loadExportPreview();
        render();
      }
    });
    await listen<boolean>("zr:evaluating", (e) => {
      if (state.overview) state.overview.evaluating = e.payload;
      renderHeader();
    });
  }

  await refreshAll();
  window.setInterval(async () => {
    state.overview = await api.overview();
    renderHeader();
  }, 60_000);
}

void boot();
