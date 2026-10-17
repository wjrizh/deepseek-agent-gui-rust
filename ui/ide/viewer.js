// 文件预览：图片、PDF（pdf.js）、其它二进制（信息页 + 外部打开）。
// 与 Monaco 文本编辑互斥：同一时刻 editor-host 内只显示一种视图。

import { readBinary, openExternal } from "./api.js";

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "avif"];

function extOf(name) {
  const i = name.lastIndexOf(".");
  return i < 0 ? "" : name.slice(i + 1).toLowerCase();
}

// 预览类型：null 表示交给 Monaco 文本编辑器。
export function previewKind(name) {
  const ext = extOf(name);
  if (IMAGE_EXTS.includes(ext)) return "image";
  if (ext === "pdf") return "pdf";
  if (["doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp"].includes(ext))
    return "office";
  return null;
}

function fmtSize(n) {
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
  return (n / 1024 / 1024).toFixed(1) + " MB";
}

// 在 host 内渲染预览，返回 dispose。
export async function renderPreview(host, projectId, rel, name) {
  const kind = previewKind(name);
  host.innerHTML = "";
  host.classList.remove("hidden");

  const ext = extOf(name);
  const info = document.createElement("div");
  info.className = "preview-info";
  info.textContent = name + " · " + (ext ? ext.toUpperCase() : "文件");

  const openBtn = document.createElement("button");
  openBtn.className = "preview-open-btn";
  openBtn.textContent = "用系统程序打开";
  openBtn.addEventListener("click", async () => {
    try {
      await openExternal(projectId, rel);
    } catch (e) {
      info.textContent = "打开失败：" + e;
    }
  });

  const bar = document.createElement("div");
  bar.className = "preview-bar";
  bar.append(info, openBtn);
  host.append(bar);

  const body = document.createElement("div");
  body.className = "preview-body";
  host.append(body);

  if (kind === "office") {
    const tip = document.createElement("div");
    tip.className = "preview-placeholder";
    tip.textContent = "此格式不支持内嵌预览，请用系统程序打开编辑。";
    body.append(tip);
    return { dispose() {} };
  }

  let bin;
  try {
    bin = await readBinary(projectId, rel);
  } catch (e) {
    const err = document.createElement("div");
    err.className = "preview-placeholder";
    err.textContent = "读取失败：" + e;
    body.append(err);
    return { dispose() {} };
  }
  info.textContent = name + " · " + fmtSize(bin.size);

  if (kind === "image") {
    const img = document.createElement("img");
    img.className = "preview-image";
    img.src = "data:" + bin.mime + ";base64," + bin.base64;
    body.append(img);
    return { dispose() { img.src = ""; } };
  }

  if (kind === "pdf") {
    return renderPdf(body, bin.base64);
  }

  return { dispose() {} };
}

// 用浏览器原生 PDF 查看器（iframe + data URL），支持缩放与表单填写。
// 与 SSH 面板一致；现代 WebView 均支持原生 PDF。
async function renderPdf(body, base64) {
  const frame = document.createElement("iframe");
  frame.className = "pdf-frame";
  frame.src = "data:application/pdf;base64," + base64;
  body.appendChild(frame);
  return { dispose() { try { frame.remove(); } catch {} } };
}