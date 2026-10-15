// Agent 面板会话状态：消息列表、运行标记、工具卡片索引。
// 纯数据，不含 DOM，便于测试与扩展。

export function createStore(projectId) {
  return {
    projectId,
    messages: [],          // { role: "user"|"assistant"|"notice", text, files? }
    running: false,
    curAssistant: null,    // 当前流式助手消息对象（引用 messages 中一项）
    curTool: null,         // 当前工具卡片数据对象
    toolByCallId: new Map(),
    subAgents: new Map(),
    subToolById: new Map(),
    listeners: new Set(),

    subscribe(fn) { this.listeners.add(fn); return () => this.listeners.delete(fn); },
    emitChange() { for (const fn of this.listeners) { try { fn(this); } catch {} } },

    addUser(text, files) {
      const m = { role: "user", text: text || "", files: files || [] };
      this.messages.push(m);
      this.emitChange();
      return m;
    },

    appendAssistant(text) {
      const t = text || "";
      if (!this.curAssistant) {
        // 空文本不创建气泡，避免工具调用前出现空气泡。
        if (!t) return null;
        this.curAssistant = { role: "assistant", text: "" };
        this.messages.push(this.curAssistant);
      }
      this.curAssistant.text += t;
      this.emitChange();
      return this.curAssistant;
    },

    // 工具调用开始时：丢弃本轮已流式的助手文本（与主窗口一致）。
    beginTool(callId, name, command) {
      if (this.curAssistant) {
        // 若尚未产出可见文本，直接移除该条，避免留下空气泡。
        const idx = this.messages.indexOf(this.curAssistant);
        if (idx >= 0 && !this.curAssistant.text.trim()) this.messages.splice(idx, 1);
        else this.curAssistant.text = "";
        this.curAssistant = null;
      }
      const card = { role: "tool", name: name || "tool", command: command || "", output: "", done: false, error: false };
      if (callId != null) this.toolByCallId.set(callId, card);
      this.curTool = card;
      this.messages.push(card);
      this.emitChange();
      return card;
    },

    toolOutput(callId, name, output) {
      let card = callId != null ? this.toolByCallId.get(callId) : this.curTool;
      if (!card || card.done) {
        card = { role: "tool", name: name || "tool", command: "", output: "", done: false, error: false };
        if (callId != null) this.toolByCallId.set(callId, card);
        this.curTool = card;
        this.messages.push(card);
      }
      card.output += output || "";
      this.emitChange();
      return card;
    },

    endTool(callId, ms, error) {
      const card = callId != null ? this.toolByCallId.get(callId) : this.curTool;
      if (card) { card.done = true; card.error = !!error; card.ms = ms || 0; }
      if (callId != null) this.toolByCallId.delete(callId);
      if (card === this.curTool) this.curTool = null;
      this.emitChange();
    },

    addNotice(text) {
      this.messages.push({ role: "notice", text: text || "" });
      this.emitChange();
    },

    finish() {
      this.curAssistant = null;
      this.curTool = null;
      this.toolByCallId.clear();
      this.running = false;
      this.emitChange();
    },

    error(message) {
      this.curAssistant = null;
      this.curTool = null;
      this.toolByCallId.clear();
      this.running = false;
      this.messages.push({ role: "notice", text: "⚠ " + (message || "未知错误") });
      this.emitChange();
    },

    subStart(id, name, duty) {
      if (!id) return;
      let s = this.subAgents.get(id);
      if (!s) { s = { id, name: name || id, duty: duty || "", status: "running", ok: null, reason: null, text: "", lines: [], tools: [] }; this.subAgents.set(id, s); }
      s.name = name || s.name; s.duty = duty || s.duty; s.status = "running";
      this.emitChange();
    },
    subDelta(id, name, text) {
      if (!id) return;
      let s = this.subAgents.get(id);
      if (!s) { s = { id, name: name || id, duty: "", status: "running", ok: null, reason: null, text: "", lines: [], tools: [] }; this.subAgents.set(id, s); }
      s.lines.push(text || "");
      this.emitChange();
    },
    subTool(id, name, phase, callId, command, output, ms, error) {
      if (!id) return;
      let s = this.subAgents.get(id);
      if (!s) { s = { id, name: name || id, duty: "", status: "running", ok: null, reason: null, text: "", lines: [], tools: [] }; this.subAgents.set(id, s); }
      const key = id + ":" + callId;
      if (phase === "call") {
        const card = { role: "tool", name: name || "tool", command: command || "", output: "", done: false, error: false };
        this.subToolById.set(key, card); s.tools.push(card);
      } else if (phase === "output") {
        let card = this.subToolById.get(key);
        if (!card) { card = { role: "tool", name: name || "tool", command: "", output: "", done: false, error: false }; this.subToolById.set(key, card); s.tools.push(card); }
        card.output += output || "";
      } else if (phase === "done") {
        const card = this.subToolById.get(key);
        if (card) { card.done = true; card.error = !!error; card.ms = ms || 0; }
      }
      this.emitChange();
    },
    subDone(id, name, ok, reason, result) {
      if (!id) return;
      let s = this.subAgents.get(id);
      if (!s) { s = { id, name: name || id, duty: "", status: "done", ok: null, reason: null, text: "", lines: [], tools: [] }; this.subAgents.set(id, s); }
      s.status = ok ? "done" : "failed";
      s.ok = !!ok; s.reason = reason || null; s.text = result || "";
      this.emitChange();
    },
    subClose(id, name) {
      const s = id ? this.subAgents.get(id) : [...this.subAgents.values()].find((x) => x.name === name);
      if (s) { s.status = "closed"; this.emitChange(); }
    },
    subCountText() {
      let total = 0, running = 0;
      for (const s of this.subAgents.values()) { total++; if (s.status === "running") running++; }
      return { total, running };
    },

    setRunning(on) {
      this.running = !!on;
      this.emitChange();
    },
  };
}