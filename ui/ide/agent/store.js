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

    setRunning(on) {
      this.running = !!on;
      this.emitChange();
    },
  };
}