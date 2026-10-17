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

// 内容搜索（本地 rg/grep，远程 ssh）。
export function searchContent(projectId, query) {
  return invoke("ide_search_content", { projectId, query });
}

// —— 远程文件传输（仅远程项目）——
export function uploadPaths(projectId, localPaths, relDir) {
  return invoke("ide_upload_paths", { projectId, localPaths, relDir: relDir || "" });
}

export function downloadFile(projectId, relPath, localPath) {
  return invoke("ide_download_file", { projectId, relPath, localPath });
}

// —— 远程端口转发 ——
export function forwardOpen(opts) {
  return invoke("remote_forward_open", opts);
}

export function forwardClose(localPort) {
  return invoke("remote_forward_close", { localPort });
}

export function forwardList() {
  return invoke("remote_forward_list");
}

// 读取项目远程规格（None=本地）。
export function getProjectRemote(projectId) {
  return invoke("get_project_remote", { projectId });
}

// 更新项目远程规格（host/user/port/root 等）。
export function setProjectRemote(projectId, spec) {
  return invoke("set_project_remote", { projectId, spec });
}

// 远程交互终端的启动参数（prog/args/label/root）。
export function remoteTerminalArgv(projectId) {
  return invoke("remote_terminal_argv", { projectId });
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

export function gitBranches(projectId) {
  return invoke("ide_git_branches", { projectId });
}

export function gitBranchCreate(projectId, name, from) {
  return invoke("ide_git_branch_create", { projectId, name, from: from || null });
}

export function gitCheckout(projectId, name) {
  return invoke("ide_git_checkout", { projectId, name });
}

export function gitBranchDelete(projectId, name, force) {
  return invoke("ide_git_branch_delete", { projectId, name, force: !!force });
}

export function gitLog(projectId, limit) {
  return invoke("ide_git_log", { projectId, limit: limit || null });
}

export function gitShow(projectId, hash, relPath) {
  return invoke("ide_git_show", { projectId, hash, relPath: relPath || null });
}

export function gitReset(projectId, hash, mode) {
  return invoke("ide_git_reset", { projectId, hash, mode });
}

export function gitInit(projectId) {
  return invoke("ide_git_init", { projectId });
}

export function gitFetch(projectId) {
  return invoke("ide_git_fetch", { projectId });
}

export function gitMergeAbort(projectId) {
  return invoke("ide_git_merge_abort", { projectId });
}

export function gitResolve(projectId, relPath, side) {
  return invoke("ide_git_resolve", { projectId, relPath, side });
}

export function gitStashPush(projectId, message) {
  return invoke("ide_git_stash_push", { projectId, message: message || null });
}

export function gitStashList(projectId) {
  return invoke("ide_git_stash_list", { projectId });
}

export function gitStashOp(projectId, action, index) {
  return invoke("ide_git_stash_op", { projectId, action, index });
}

export function gitClone(projectId, url) {
  return invoke("ide_git_clone", { projectId, url });
}

export function gitMerge(projectId, name) {
  return invoke("ide_git_merge", { projectId, name });
}

export function gitFileLog(projectId, relPath, limit) {
  return invoke("ide_git_file_log", { projectId, relPath, limit: limit || null });
}

export function gitBlame(projectId, relPath) {
  return invoke("ide_git_blame", { projectId, relPath });
}

export function getRestrict(projectId) {
  return invoke("get_restrict", { projectId });
}

export function setRestrict(projectId, on) {
  return invoke("set_restrict", { projectId, on });
}

// —— 内置终端 ——
const ChannelCtor = window.__TAURI__.core.Channel;

export function termOpen(id, cwd, shell, cols, rows, onEvent, prog, args) {
  const channel = new ChannelCtor();
  channel.onmessage = onEvent;
  return invoke("term_open", {
    id, cwd: cwd || null, shell: shell || null,
    prog: prog || null, args: args || null,
    cols, rows, onEvent: channel,
  });
}

export function termWrite(id, data) {
  return invoke("term_write", { id, data });
}

export function termResize(id, cols, rows) {
  return invoke("term_resize", { id, cols, rows });
}

export function termClose(id) {
  return invoke("term_close", { id });
}

// —— 独立 SSH 文件浏览器（不依赖项目）——
export function sshListDir(spec, rel) {
  return invoke("ssh_list_dir", { spec, rel: rel || "" });
}
export function sshCreate(spec, rel, isDir) {
  return invoke("ssh_create", { spec, rel, isDir });
}
export function sshDelete(spec, rel) {
  return invoke("ssh_delete", { spec, rel });
}
export function sshRename(spec, fromRel, toRel) {
  return invoke("ssh_rename", { spec, fromRel, toRel });
}
export function sshMove(spec, fromAbs, toDirAbs) {
  return invoke("ssh_move", { spec, fromAbs, toDirAbs });
}
export function sshDownload(spec, rel, localPath) {
  return invoke("ssh_download", { spec, rel, localPath });
}
export function sshUpload(spec, localPath, relDest) {
  return invoke("ssh_upload", { spec, localPath, relDest });
}
export function sshHosts() {
  return invoke("remote_ssh_hosts");
}
export function sshTailscaleDevices() {
  return invoke("remote_tailscale_devices");
}
export function sshTerminalArgv(spec) {
  return invoke("ssh_terminal_argv", { spec });
}
export function sshReadText(spec, abs) {
  return invoke("ssh_read_text", { spec, abs });
}
export function sshReadBinaryB64(spec, abs) {
  return invoke("ssh_read_binary_b64", { spec, abs });
}
export function sshWriteText(spec, abs, content) {
  return invoke("ssh_write_text", { spec, abs, content });
}