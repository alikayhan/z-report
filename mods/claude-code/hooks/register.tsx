import type { Register } from "claude-code";
import { Model, PICKER_LIMIT, active } from "./model";

export const register: Register = (on) => {
  let model: Model | null = null;
  let interactive = false;
  on("session.start", async ($, e, next) => {
    interactive =
      e.isInteractive === true &&
      e.surface === "terminal" &&
      !(await $.env.get("Z_REPORT_EVALUATOR"));
    model ??= new Model({
      root: $.plugin.root,
      run: (argv, init) => $.process.run(argv, init),
      dataOverride: () => $.env.get("Z_REPORT_DATA_DIR"),
      redraw: () => $.ui.invalidate("ui.render"),
      log: (text) => $.ui.log(text),
      every: (ms, fn) => $.clock.every(ms, fn),
    });
    await $.command.register({
      name: "z-report",
      description: "Review your local work journal",
      argumentHint: "[x-read|cancel]",
      immediate: true,
    });
    const result = await next(e);
    if (interactive) void model.auto();
    return result;
  });
  on("classic.SessionStart", async ($, e, next) => {
    const result = await next(e);
    if (interactive && model && e.source === "resume") void model.auto();
    return result;
  });
  on("turn.complete", async ($, e, next) => {
    const result = await next(e);
    if (interactive && model) void model.auto();
    return result;
  });
  on("command.run", { command: "z-report" }, async ($, e) => {
    if (!interactive || !model)
      return { text: "Z Report requires an interactive terminal session." };
    const arg = e.args.trim();
    if (!["", "x-read", "cancel"].includes(arg))
      return { text: "Usage: /z-report [x-read|cancel]" };
    await $.ui.open({
      id: "z-report",
      title: "Z Report",
      focus: true,
      closeOnEscape: true,
      rows: 20,
    });
    const m = model;
    void m
      .act(async () => {
        await m.refresh();
        if (arg === "x-read") await m.start();
        else if (arg === "cancel") await m.cancel();
      })
      .then(() => {
        if (!arg) void m.auto();
      });
    return {};
  });
  on("ui.render", { component: "Pane" }, async ($, e, next) => {
    if (e.requestId !== "z-report" || e.surface !== "terminal" || !model)
      return next(e);
    const m = model;
    const { Box, Text, Button, Input, Select } = await $.ui.resolve(e);
    const action = (fn: () => Promise<void>) => {
      void m.act(fn);
    };
    const redraw = () => m.host.redraw();
    const c = m.candidate;
    const picker = m.picker;
    const draft = m.editing;
    const settings = m.settings;
    const range = (
      <Box flexDirection="column">
        <Box gap={1}>
          <Button
            key="today"
            label="Today"
            onPress={() =>
              action(async () => {
                m.period(1);
                await m.loadTab();
              })
            }
          />
          <Button
            key="week"
            label="Last 7 days"
            onPress={() =>
              action(async () => {
                m.period(7);
                await m.loadTab();
              })
            }
          />
        </Box>
        <Input
          key="from"
          label="From (YYYY-MM-DD)"
          value={m.from}
          onInput={(v) => {
            m.from = v;
          }}
          onSubmit={(v) =>
            action(async () => {
              m.from = v;
              await m.loadTab();
            })
          }
        />
        <Input
          key="to"
          label="To (YYYY-MM-DD)"
          value={m.to}
          onInput={(v) => {
            m.to = v;
          }}
          onSubmit={(v) =>
            action(async () => {
              m.to = v;
              await m.loadTab();
            })
          }
        />
        <Button
          key="range"
          label="Apply range"
          onPress={() => action(() => m.loadTab())}
        />
      </Box>
    );
    return (
      <Box flexDirection="column" gap={1}>
        <Box gap={1}>
          <Text bold>Z Report</Text>
          {!m.busy && (
            <Button
              key="run"
              label={active(m.read) ? "Cancel X-read" : "X-read"}
              onPress={() =>
                action(() => (active(m.read) ? m.cancel() : m.start()))
              }
            />
          )}
          <Button
            key="refresh"
            label="Refresh"
            onPress={() => action(() => m.refresh())}
          />
        </Box>
        <Text>
          {m.busy
            ? "Working…"
            : m.read
              ? `${m.read.status} · ${m.read.completed_days}/${m.read.total_days} days · ${m.read.message}`
              : "Ready"}
        </Text>
        {m.error && <Text color="red">{m.error}</Text>}
        {m.notice && <Text>{m.notice}</Text>}
        {m.overview && !m.overview.initialized && (
          <Text>
            Run X-read to review local Claude Code and Codex work from the last
            15 days. Evidence is evaluated in a separate CLI session using your
            account. After the first successful read, automatic catch-up runs at
            most once per 24 hours. Configure privacy below.
          </Text>
        )}
        {m.overview?.last_successful_read_at && (
          <Text
            dimColor
          >{`Last successful read: ${m.overview.last_successful_read_at}`}</Text>
        )}
        <Select
          key="tab"
          label="View"
          value={m.tab}
          options={["review", "journal", "export", "settings"].map((value) => ({
            value,
            label: value[0]!.toUpperCase() + value.slice(1),
          }))}
          onSelect={(v) =>
            action(async () => {
              m.tab = v;
              m.evidence = "";
              m.editing = null;
              await m.loadTab();
            })
          }
        />
        {m.tab === "review" && (
          <Box flexDirection="column" gap={1}>
            <Select
              key="status"
              label="Candidates"
              value={m.status}
              options={[
                { value: "pending", label: "Pending" },
                { value: "discarded", label: "Discarded" },
              ]}
              onSelect={(v) =>
                action(async () => {
                  m.status = v;
                  m.editing = null;
                  m.evidence = "";
                  await m.loadTab();
                })
              }
            />
            {!c && (
              <Text>
                No {m.status} candidates. X-read checks for recent work.
              </Text>
            )}
            {picker.total > PICKER_LIMIT && (
              <Box gap={1}>
                {picker.start > 0 && (
                  <Button
                    key="earlier"
                    label="Earlier"
                    onPress={() => m.pick(m.candidates[picker.start - 1]?.id)}
                  />
                )}
                <Text
                  dimColor
                >{`${picker.start + 1}–${picker.start + picker.items.length} of ${picker.total}`}</Text>
                {picker.start + picker.items.length < picker.total && (
                  <Button
                    key="later"
                    label="Later"
                    onPress={() =>
                      m.pick(
                        m.candidates[picker.start + picker.items.length]?.id,
                      )
                    }
                  />
                )}
              </Box>
            )}
            {!!picker.items.length && (
              <Select
                key="candidate"
                value={c?.id}
                options={picker.items.map((c) => ({
                  value: c.id,
                  label: `${c.day} · ${c.title}`,
                }))}
                onSelect={(v) => m.pick(v)}
              />
            )}
            {c && !draft && (
              <Box flexDirection="column" gap={1}>
                <Text bold>{c.title}</Text>
                <Text>{`${c.day}${c.day_end ? ` – ${c.day_end}` : ""} · ${c.agents.join(" + ")} · evidence ${c.evidence_level}${c.repo ? ` · ${c.repo}` : ""}`}</Text>
                <Text>{c.contribution}</Text>
                {c.outcomes.map((o, i) => (
                  <Text
                    key={`outcome-${i}`}
                  >{`${o.verified ? "✓" : "○"} ${o.claim} [${o.evidence_refs.join(", ")}]`}</Text>
                ))}
                {c.uncertainties.map((u, i) => (
                  <Text key={`uncertain-${i}`}>{`Uncertain: ${u}`}</Text>
                ))}
                {c.pr_links.map((p) => (
                  <Text key={p.url}>{p.url}</Text>
                ))}
                {c.status === "pending" ? (
                  <Box flexDirection="column" gap={1}>
                    <Box gap={1}>
                      <Button
                        key="approve"
                        label="Approve"
                        onPress={() =>
                          action(() => m.mutate("approve_candidate", c))
                        }
                      />
                      <Button
                        key="edit"
                        label="Edit"
                        onPress={() => {
                          m.editing = JSON.parse(JSON.stringify(c));
                          redraw();
                        }}
                      />
                      <Button
                        key="discard"
                        label="Discard"
                        onPress={() =>
                          action(() => m.mutate("discard_candidate", c))
                        }
                      />
                    </Box>
                    <Button
                      key="select-merge"
                      label={
                        m.mergeIds.includes(c.id)
                          ? "Remove from merge"
                          : "Select for merge"
                      }
                      onPress={() => {
                        m.mergeIds = m.mergeIds.includes(c.id)
                          ? m.mergeIds.filter((id) => id !== c.id)
                          : [...m.mergeIds, c.id];
                        redraw();
                      }}
                    />
                    {m.mergeIds.length >= 2 && (
                      <Button
                        key="merge"
                        label={`Merge ${m.mergeIds.length} selected`}
                        onPress={() => action(() => m.merge())}
                      />
                    )}
                  </Box>
                ) : (
                  <Button
                    key="restore"
                    label="Restore"
                    onPress={() =>
                      action(() => m.mutate("restore_candidate", c))
                    }
                  />
                )}
                {c.related && (
                  <Box flexDirection="column">
                    <Text>{`Related ${c.related.kind}: ${c.related.target_title} (${c.related.target_day})`}</Text>
                    <Button
                      key="dismiss-related"
                      label="Dismiss suggestion"
                      onPress={() =>
                        action(async () => {
                          await m.call("dismiss_related", { id: c.id });
                          await m.refresh();
                        })
                      }
                    />
                  </Box>
                )}
                <Button
                  key="evidence"
                  label={m.evidence ? "Hide evidence" : "Inspect evidence"}
                  onPress={() =>
                    action(async () => {
                      if (m.evidence) m.evidence = "";
                      else await m.showEvidence(c);
                    })
                  }
                />
                {m.evidence && <Text>{m.evidence}</Text>}
              </Box>
            )}
            {draft && (
              <Box flexDirection="column" gap={1}>
                <Input
                  key="edit-title"
                  label="Title"
                  value={draft.title}
                  onInput={(v) => {
                    draft.title = v;
                  }}
                  onSubmit={(v) => {
                    draft.title = v;
                  }}
                />
                <Input
                  key="edit-contribution"
                  label="Contribution"
                  value={draft.contribution}
                  onInput={(v) => {
                    draft.contribution = v;
                  }}
                  onSubmit={(v) => {
                    draft.contribution = v;
                  }}
                />
                {draft.outcomes.map((o, i) => (
                  <Input
                    key={`edit-outcome-${i}`}
                    label={`Outcome ${i + 1}`}
                    value={o.claim}
                    onInput={(v) => {
                      o.claim = v;
                    }}
                    onSubmit={(v) => {
                      o.claim = v;
                    }}
                  />
                ))}
                <Button
                  key="add-outcome"
                  label="Add outcome"
                  onPress={() => {
                    draft.outcomes.push({
                      claim: "",
                      evidence_level: 1,
                      evidence_refs: [],
                      verified: false,
                    });
                    redraw();
                  }}
                />
                <Text dimColor>
                  Changed claims require fresh evidence verification.
                </Text>
                <Box gap={1}>
                  <Button
                    key="save-edit"
                    label="Save changes"
                    onPress={() => action(() => m.saveEdit())}
                  />
                  <Button
                    key="cancel-edit"
                    label="Cancel edit"
                    onPress={() => {
                      m.editing = null;
                      redraw();
                    }}
                  />
                </Box>
              </Box>
            )}
          </Box>
        )}
        {m.tab === "journal" && (
          <Box flexDirection="column" gap={1}>
            {range}
            <Input
              key="search"
              label="Search"
              value={m.query}
              onInput={(v) => {
                m.query = v;
              }}
              onSubmit={(v) =>
                action(async () => {
                  m.query = v;
                  await m.loadTab();
                })
              }
            />
            {!m.journal.length && (
              <Text>No journal entries in this range.</Text>
            )}
            {m.journal.map((j) => (
              <Box key={j.id} flexDirection="column">
                <Text bold>{`${j.day} · ${j.title}`}</Text>
                <Text>{j.contribution}</Text>
                {j.outcomes.map((o, i) => (
                  <Text key={`${j.id}-${i}`}>{`• ${o.claim}`}</Text>
                ))}
              </Box>
            ))}
          </Box>
        )}
        {m.tab === "export" && (
          <Box flexDirection="column" gap={1}>
            {range}
            <Button
              key="copy"
              label="Copy Markdown"
              onPress={() =>
                action(async () => {
                  if (!m.export) return;
                  const r = await m.host.run(["/usr/bin/pbcopy"], {
                    stdin: m.export.markdown,
                    timeoutMs: 5000,
                  });
                  if (r.exitCode) throw new Error("Clipboard unavailable");
                  m.notice = "Copied Markdown";
                })
              }
            />
            <Input
              key="save-path"
              label="Save to absolute .md path"
              value={m.savePath}
              onInput={(v) => {
                m.savePath = v;
              }}
              onSubmit={(v) => {
                m.savePath = v;
              }}
            />
            <Button
              key="save-export"
              label="Save Markdown"
              onPress={() =>
                action(async () => {
                  await m.call("save_export", {
                    from: m.from,
                    to: m.to,
                    path: m.savePath,
                  });
                  m.notice = `Saved ${m.savePath}`;
                })
              }
            />
            <Text>{m.export?.markdown ?? "No export loaded."}</Text>
          </Box>
        )}
        {m.tab === "settings" && settings && (
          <Box flexDirection="column" gap={1}>
            <Text>
              Settings and journal are shared with Z Report desktop. Evaluations
              use your CLI account in an isolated session; evidence stays out of
              this conversation.
            </Text>
            {(
              ["auto_catchup", "retain_prompts", "cost_limit_enabled"] as const
            ).map((key) => (
              <Select
                key={key}
                label={
                  {
                    auto_catchup: "Automatic catch-up",
                    retain_prompts: "Retain prompts",
                    cost_limit_enabled: "Limit evaluation cost",
                  }[key]
                }
                value={String(settings[key])}
                options={[
                  { value: "true", label: "On" },
                  { value: "false", label: "Off" },
                ]}
                onSelect={(v) => {
                  settings[key] = v === "true";
                  redraw();
                }}
              />
            ))}
            <Input
              key="exclusions"
              label="Excluded repository paths (separate with ;)"
              value={settings.excluded_repos.join(";")}
              onInput={(v) => {
                settings.excluded_repos = v
                  .split(";")
                  .map((s) => s.trim())
                  .filter(Boolean);
              }}
              onSubmit={(v) => {
                settings.excluded_repos = v
                  .split(";")
                  .map((s) => s.trim())
                  .filter(Boolean);
              }}
            />
            <Input
              key="claude-path"
              label="Claude executable override"
              value={settings.claude_path ?? ""}
              onInput={(v) => {
                settings.claude_path = v || null;
              }}
              onSubmit={(v) => {
                settings.claude_path = v || null;
              }}
            />
            <Input
              key="retention"
              label="Pending retention days (0 keeps all)"
              value={String(settings.retention_days)}
              onInput={(v) => {
                settings.retention_days = Number(v);
              }}
              onSubmit={(v) => {
                settings.retention_days = Number(v);
              }}
            />
            <Button
              key="save-settings"
              label="Save settings"
              onPress={() =>
                action(async () => {
                  m.settings = await m.call("update_settings", {
                    patch: settings,
                  });
                  m.notice = "Settings saved";
                })
              }
            />
            {m.overview?.evaluators.map((v) => (
              <Text
                key={v.agent}
              >{`${v.agent}: ${v.found ? v.model : "CLI not found"}`}</Text>
            ))}
            <Text bold>Recent evaluations</Text>
            {m.runs.slice(0, 10).map((r) => (
              <Text
                key={r.id}
              >{`${r.day} · ${r.status} · ${r.model ?? ""} · ${r.candidate_count} candidates${r.error ? ` · ${r.error}` : ""}`}</Text>
            ))}
          </Box>
        )}
        <Text dimColor>
          Escape closes the pane. Reads continue while this session is alive.
        </Text>
      </Box>
    );
  });
};
