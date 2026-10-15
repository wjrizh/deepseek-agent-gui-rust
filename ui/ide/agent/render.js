// 把 store 的消息列表渲染为 DOM。仅负责视图，无业务逻辑。

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

// 工具卡片的 DOM 结构，返回卡片元素。默认收起，点击头部展开/收起。
function toolCard(msg, onToggle) {
  const card = el("div", "ag-tool");
  const head = el("div", "ag-tool-head");
  const arrow = el("span", "ag-tool-arrow", "▸");
  const name = el("span", "ag-tool-name", msg.name || "tool");
  const status = el("span", "ag-tool-status");
  head.append(arrow, name, status);
  card.append(head);

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
export function render(root, store) {
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
    root.append(subAgentCard(s));
  }
}

// 仅更新运行/停止按钮状态。
export function updateControls(store, { sendBtn, stopBtn, input }) {
  if (sendBtn) sendBtn.classList.toggle("hidden", store.running);
  if (stopBtn) stopBtn.classList.toggle("hidden", !store.running);
  if (input) input.disabled = store.running;
}

function subAgentCard(s) {
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
    s.status === "failed" ? ("失败" + (s.reason ? " · " + s.reason : "")) : "完成");
  head.append(arrow, name, status);
  card.append(head);

  const body = el("div", "ag-sub-body");
  if (s.duty) body.append(el("div", "ag-sub-duty", s.duty));
  for (const t of s.tools) body.append(toolCard(t));
  const text = (s.lines.join("") + (s.text ? "\n" + s.text : "")).trim();
  if (text) body.append(el("pre", "ag-sub-out", text));
  card.append(body);

  const setExpanded = (open) => { card.classList.toggle("expanded", open); arrow.textContent = open ? "▾" : "▸"; };
  setExpanded(!!s.expanded);
  head.addEventListener("click", () => { s.expanded = !s.expanded; setExpanded(s.expanded); });
  return card;
}