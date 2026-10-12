// 与 Tauri 后端的命令封装。IDE 窗口通过 withGlobalTauri 暴露的 __TAURI__ 调用。

const invoke = window.__TAURI__.core.invoke;
const getCurrentWindow = window.__TAURI__.window.getCurrentWindow;

export const tauriWindow = getCurrentWindow();

export function listDir(projectId, relPath) {
  return invoke("ide_list_dir", { projectId, relPath: relPath || "" });
}

export function readFile(projectId, relPath) {
  return invoke("ide_read_file", { projectId, relPath });
}

export function writeFile(projectId, relPath, content) {
  return invoke("ide_write_file", { projectId, relPath, content });
}

export function deletePath(projectId, relPath) {
  return invoke("ide_delete", { projectId, relPath });
}

export function restorePath(projectId, trashRel, destRel) {
  return invoke("ide_restore", { projectId, trashRel, destRel: destRel || null });
}

export function purgeTrash(projectId) {
  return invoke("ide_purge_trash", { projectId });
}

export function copyPath(projectId, relPath, destDir) {
  return invoke("ide_copy", { projectId, relPath, destDir });
}

export function movePath(projectId, relPath, destDir, newName) {
  return invoke("ide_move", { projectId, relPath, destDir, newName: newName || null });
}

export function createPath(projectId, parentDir, name, isDir) {
  return invoke("ide_create", { projectId, parentDir, name, isDir });
}

// 读取/设置项目工作目录（供“设为项目文件夹”与“上一层”联动主 UI）。
export function getWorkdir(projectId) {
  return invoke("get_workdir", { projectId });
}

export function setWorkdir(projectId, path) {
  return invoke("set_workdir", { projectId, path });
}

// 读取文件为 base64（图片/PDF 预览）。
export function readBinary(projectId, relPath) {
  return invoke("ide_read_binary", { projectId, relPath });
}

// 用系统默认程序打开文件。
export function openExternal(projectId, relPath) {
  return invoke("ide_open_external", { projectId, relPath });
}

// —— Git ——
export function gitStatus(projectId) {
  return invoke("ide_git_status", { projectId });
}

export function gitDiff(projectId, relPath, staged) {
  return invoke("ide_git_diff", { projectId, relPath, staged: !!staged });
}

export function gitStage(projectId, relPath) {
  return invoke("ide_git_stage", { projectId, relPath: relPath || null });
}

export function gitUnstage(projectId, relPath) {
  return invoke("ide_git_unstage", { projectId, relPath: relPath || null });
}

export function gitDiscard(projectId, relPath) {
  return invoke("ide_git_discard", { projectId, relPath });
}

export function gitCommit(projectId, message) {
  return invoke("ide_git_commit", { projectId, message });
}

export function gitPush(projectId) {
  return invoke("ide_git_push", { projectId });
}

export function gitPull(projectId) {
  return invoke("ide_git_pull", { projectId });
}

export function getRestrict(projectId) {
  return invoke("get_restrict", { projectId });
}

export function setRestrict(projectId, on) {
  return invoke("set_restrict", { projectId, on });
}
