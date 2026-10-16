// 把 store 的消息列表渲染为 DOM。仅负责视图，无业务逻辑。

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

// 从工具命令行/输出中尽力提取文件相对路径（用于"打开文件/查看 Diff"）。
function extractPath(cmd) {
  if (!cmd) return "";
  const m = String(cmd).match(/[\w./@-]+\.(rs|js|mjs|cjs|ts|tsx|jsx|json|md|css|html|toml|yaml|yml|py|go|java|c|h|cpp|sh|txt)/);
  return m ? m[0] : "";
}

// 工具卡片的 DOM 结构，返回卡片元素。默认收起，点击头部展开/收起。
function toolCard(msg, onToggle) {
  const card = el("div", "ag-tool");
  const head = el("div", "ag-tool-head");
  const arrow = el("span", "ag-tool-arrow", "▸");
  const name = el("span", "ag-tool-name", msg.name || "tool");
  const status = el("span", "ag-tool-status");
  const actions = el("span", "ag-tool-actions");
  head.append(arrow, name, status, actions);
  card.append(head);

  const rel = extractPath(msg.command) || extractPath(msg.output);
  if (rel) {
    const openBtn = el("button", "ag-tool-act", "打开");
    openBtn.title = "在编辑器中打开 " + rel;
    openBtn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      if (window.__ideOpenFile) window.__ideOpenFile(rel);
    });
    actions.append(openBtn);
    if (window.__hasUnreadAgentDiff && window.__hasUnreadAgentDiff(rel)) {
      const diffBtn = el("button", "ag-tool-act", "查看Diff");
      diffBtn.title = "查看 Agent 对 " + rel + " 的改动";
      diffBtn.addEventListener("click", (ev) => {
        ev.stopPropagation();
        if (window.__openAgentDiff) window.__openAgentDiff(rel);
      });
      actions.append(diffBtn);
    }
  }
  {
    const copyBtn = el("button", "ag-tool-act", "复制");
    copyBtn.title = "复制工具输出";
    copyBtn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      const text = (msg.output || msg.command || "");
      try { navigator.clipboard.writeText(text); } catch {}
    });
    actions.append(copyBtn);
  }

  const body = el("div", "ag-tool-body");
  if (msg.command) body.append(el("div", "ag-tool-cmd", msg.command));
  if (msg.output) body.append(el("pre", "ag-tool-out", msg.output));
  card.append(body);

  const setExpanded = (open) => {
    card.classList.toggle("expanded", open);
    arrow.textContent = open ? "▾" : "▸";
  };
  setExpanded(!!msg.expanded);
  head.addEventListener("click", () => {
    msg.expanded = !msg.expanded;
    setExpanded(msg.expanded);
    if (onToggle) onToggle();
  });

  applyToolState(card, msg);
  return card;
}

function applyToolState(card, msg) {
  const status = card.querySelector(".ag-tool-status");
  card.classList.toggle("done", !!msg.done);
  card.classList.toggle("error", !!msg.error);
  if (status) {
    if (!msg.done) status.textContent = "运行中…";
    else if (msg.error) status.textContent = "失败";
    else status.textContent = "完成" + (msg.ms ? ` · ${msg.ms}ms` : "");
  }
}

// 全量渲染。消息量通常不大；如需增量可后续优化。
export function render(root, store, handlers) {
  const nearBottom = root.scrollTop + root.clientHeight >= root.scrollHeight - 40;
  const keepScroll = root.scrollTop;
  root.innerHTML = "";
  for (const m of store.messages) {
    if (m.role === "user") {
      const wrap = el("div", "ag-msg ag-user");
      const b = el("div", "ag-bubble", m.text);
      wrap.append(b);
      if (m.files && m.files.length) {
        const fl = el("div", "ag-files");
        for (const f of m.files) fl.append(el("span", "ag-file", f));
        wrap.append(fl);
      }
      root.append(wrap);
    } else if (m.role === "assistant") {
      const wrap = el("div", "ag-msg ag-assistant");
      wrap.append(el("div", "ag-bubble", m.text));
      root.append(wrap);
    } else if (m.role === "notice") {
      root.append(el("div", "ag-notice", m.text));
    } else if (m.role === "tool") {
      root.append(toolCard(m, () => { root.scrollTop = keepScroll; }));
    }
  }
  // 同步更新最后一个工具卡片的进行中状态（若输出在追加）。
  const toolMsgs = store.messages.filter((m) => m.role === "tool");
  const cards = root.querySelectorAll(".ag-tool");
  for (let i = 0; i <toolMsgs.length && i < cards.length; i++) {
    applyToolState(cards[i], toolMsgs[i]);
  }
  if (nearBottom) root.scrollTop = root.scrollHeight;

  for (const s of store.subAgents.values()) {
    root.append(subAgentCard(s, handlers));
  }
}

// 仅更新运行/停止按钮状态。
export function updateControls(store, { sendBtn, stopBtn, input }) {
  if (sendBtn) sendBtn.classList.toggle("hidden", store.running);
  if (stopBtn) stopBtn.classList.toggle("hidden", !store.running);
  if (input) input.disabled = store.running;
}

function subAgentCard(s, handlers) {
  const card = el("div", "ag-sub");
  card.classList.toggle("running", s.status === "running");
  card.classList.toggle("failed", s.status === "failed");
  card.classList.toggle("closed", s.status === "closed");
  const head = el("div", "ag-sub-head");
  const arrow = el("span", "ag-sub-arrow", s.expanded ? "▾" : "▸");
  const name = el("span", "ag-sub-name", "子agent · " + s.name);
  const status = el("span", "ag-sub-status",
    s.status === "running" ? "运行中…" :
    s.status === "closed" ? "已关闭" :
    s.status === "cancelled" ? "已取消" :
    s.status === "failed" ? ("失败" + (s.reason ? " · " + s.reason : "")) : "完成");
  head.append(arrow, name, status);
  const subText = (s.lines.join("") + (s.text ? "\n" + s.text : "")).trim();
  const actions = el("span", "ag-tool-actions");
  if (subText) {
    const copyBtn = el("button", "ag-tool-act", "复制结果");
    copyBtn.title = "复制子 agent 结果";
    copyBtn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      try { navigator.clipboard.writeText(subText); } catch {}
    });
    actions.append(copyBtn);
  }
  if (handlers) {
    const act = (label, title, fn, needConfirm) => {
      const btn = el("button", "ag-tool-act", label);
      if (title) btn.title = title;
      btn.addEventListener("click", (ev) => {
        ev.stopPropagation();
        if (needConfirm && !window.confirm(needConfirm)) return;
        fn();
      });
      actions.append(btn);
    };
    if (s.status === "running") {
      act("停止", "停止该子 agent 当前运行", () => handlers.stop(s));
    } else {
      act("重试", "用原任务重新派发", () => handlers.retry(s));
      act("继续", "在现有会话上追加新任务", () => handlers.cont(s));
      act("新会话重试", "清空会话后用原任务重派", () => handlers.retryNew(s), "将在新会话中用原任务重试，确认？");
    }
  }
  if (actions.childNodes.length) head.append(actions);
  card.append(head);

  const body = el("div", "ag-sub-body");
  for (const t of s.tools) body.append(toolCard(t));
  const text = (s.lines.join("") + (s.text ? "\n" + s.text : "")).trim();
  if (text) body.append(el("pre", "ag-sub-out", text));
  card.append(body);

  const setExpanded = (open) => { card.classList.toggle("expanded", open); arrow.textContent = open ? "▾" : "▸"; };
  setExpanded(!!s.expanded);
  head.addEventListener("click", () => { s.expanded = !s.expanded; setExpanded(s.expanded); });
  return card;
}