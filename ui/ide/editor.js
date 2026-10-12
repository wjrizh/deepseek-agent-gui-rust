// Monaco 编辑器封装：加载、打开文件、脏标记、保存。

let monacoReady = null;

// 按扩展名映射 Monaco 语言 id（缺省用 plaintext）。
const LANG_BY_EXT = {
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "javascript",
  ts: "typescript", tsx: "typescript",
  json: "json", html: "html", htm: "html", css: "css", scss: "scss", less: "less",
  md: "markdown", markdown: "markdown",
  rs: "rust", go: "go", py: "python", java: "java", c: "c", h: "c",
  cpp: "cpp", cc: "cpp", hpp: "cpp", cs: "csharp", rb: "ruby", php: "php",
  sh: "shell", bash: "shell", zsh: "shell", ps1: "powershell",
  yml: "yaml", yaml: "yaml", toml: "ini", ini: "ini", xml: "xml", sql: "sql",
  vue: "html", svelte: "html", lua: "lua", swift: "swift", kt: "kotlin",
};

export function langForPath(p) {
  const i = p.lastIndexOf(".");
  if (i <0) return "plaintext";
  const ext = p.slice(i + 1).toLowerCase();
  return LANG_BY_EXT[ext] || "plaintext";
}

function detectTheme() {
  return "vs-dark";
}

// 加载 Monaco（AMD loader）。
export function loadMonaco() {
  if (monacoReady) return monacoReady;
  monacoReady = new Promise((resolve) => {
    window.require.config({ paths: { vs: "../vendor/monaco/vs" } });
    window.require(["vs/editor/editor.main"], () => {
      resolve(window.monaco);
    });
  });
  return monacoReady;
}

// 创建一个绑定到 host 元素的编辑器实例。
export async function createEditor(host) {
  const monaco = await loadMonaco();
  const editor = monaco.editor.create(host, {
    value: "",
    language: "plaintext",
    theme: detectTheme(),
    automaticLayout: true,
    fontSize: 15,
    fontFamily: "'JetBrainsMono Nerd Font', 'JetBrains Mono', 'Cascadia Code', Menlo, Consolas, monospace",
    lineHeight: 22,
    letterSpacing: 0.2,
    minimap: { enabled: true, maxColumn: 80 },
    scrollBeyondLastLine: false,
    smoothScrolling: true,
    cursorSmoothCaretAnimation: "on",
    renderLineHighlight: "all",
    padding: { top: 12, bottom: 12 },
    fontLigatures: true,
    tabSize: 2,
  });
  // 禁用 Monaco 自带 undo/redo，交给应用级统一时间线（window.__ideUndo/__ideRedo）。
  editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyZ, () => window.__ideUndo && window.__ideUndo());
  editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyZ, () => window.__ideRedo && window.__ideRedo());
  editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyY, () => window.__ideRedo && window.__ideRedo());
  return { monaco, editor };
}