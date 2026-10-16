# IDE 远程插件 · 实施清单（逐项完成）

> 依据 `docs/remote-plugin-plan.md` + 外部 AI 评审结论定稿。
> 原则：增量、一步一验、零回归。每步完成后：`cargo build` + `cargo test`（z 用户）+ `node --check`（JS）。
> 现有本地功能不得受影响；`invoke("ide_*")` 接口保持稳定，后端按项目类型分派。

---

## 一、评审意见取舍

### 采纳（好）
1. **传输层 A**：系统 OpenSSH + ControlMaster/ControlPersist，不自研认证/SFTP。贴合项目 shell-out 风格，复用 `~/.ssh/config`、agent、known_hosts。
2. **改 `fs.rs` 内部分派**，不复制整套命令。切入点即现有 `project_root()` / `project_restrict()`。
3. **`remote_by_project` 独立字段**（不改 `Project`），避免持久化迁移。
4. **拆 FileBackend / ExecBackend / GitBackend**，避免上帝 trait。
5. **host key 默认校验**、首次显示指纹；**不 shell 拼接**用户输入；**restrict 远端 realpath 校验**。
6. **远程 Git 用异步子进程**（现有 git.rs 同步阻塞，远程更甚）。
7. **`ssh -G` 解析 config**，不自写完整 parser。
8. **远程删除 P0 先永久删+二次确认**，回收站后置。
9. **能力探测 + 结构化错误码**（缺 ssh/sftp/git/rg 明确提示、降级）。

### 不采纳 / 后置（过度或收益低）
- ❌ 引入 `russh`/`ssh2`（除非目标平台确无 OpenSSH）。
- ⏸ `keyring` crate：MVP 先支持 agent/无口令私钥，密码认证后置。
- ⏸ 自定义「JSON header + 二进制 payload」协议：MVP 用固定脚本 + 文本，先不做二进制帧。
- ⏸ 自研连接池：只管子进程与 ControlMaster socket 生命周期。
- ⏸ 实时远程 watch / inotify-over-SSH / 断点续传 / 远程进程附加（P2，可能永不做）。

---

## 二、分阶段清单

### 阶段 0 · 模型与能力探测（无行为变更）
- [ ] 0.1 `projects.rs`：新增 `RemoteSpec`（host/user/port/root/ssh_alias/proxy_jump/restrict）+ `remote_by_project` 字段（`#[serde(default)]`）+ getter/setter；STORE_VERSION 视需要递增。
- [ ] 0.2 `projects.rs`：项目区分本地/远程的判定方法（如 `is_remote(project_id)`）。
- [ ] 0.3 `state.rs`：`Inner` 新增 SSH 连接状态占位（进程/socket 表）。
- [ ] 0.4 新增 `src/ide/remote.rs`：`remote_probe()`（探测 ssh/sftp/git/rg/tailscale 是否存在与版本）、`remote_test_connection(spec)`（`ssh -G` + 连通性）。
- [ ] 0.5 `lib.rs` 注册新命令。
- ✅ 验收：编译+测试全绿；现有本地项目创建/打开/文件/Git 行为不变。

### 阶段 1 · 最小 SSH 执行层
- [ ] 1.1 `src/ide/ssh/mod.rs`：连接参数组装（由 RemoteSpec → ssh 命令行，含 `-o ControlMaster=auto -o ControlPath=... -o ControlPersist`）。
- [ ] 1.2 host key 校验：首次连接显示指纹并确认；写 known_hosts；禁止无条件 `StrictHostKeyChecking=no`。
- [ ] 1.3 `exec`：结构化远程命令执行（超时、取消、stdout/stderr 分离、退出码）。
- [ ] 1.4 连接生命周期：建立/复用/关闭；`ControlPersist` 交给 OpenSSH，不自研池。
- ✅ 验收：可对一台主机跑通 `exec` 并正确返回；断连有明确错误。

### 阶段 2 · 远程文件核心
- [ ] 2.1 `fs.rs`：`project_root` → 引入 `backend_for(project_id)` 分派（本地分支逻辑原样）。
- [ ] 2.2 `FileBackend` trait：list_dir / read_file / write_file / create / delete / rename / move / copy / stat。
- [ ] 2.3 本地实现迁移到 `backend/local.rs`（逻辑不变）。
- [ ] 2.4 远程实现 `backend/ssh.rs`：list（条目数+字节上限）、read（沿用 8 MiB 上限 + 二进制检测）、write（临时文件 + 原子 rename；保存前 mtime 检查）。
- [ ] 2.5 远程路径校验：POSIX 相对路径 + 远端 realpath，拒绝 NUL/绝对越界/`..`/软链逃逸。
- [ ] 2.6 create/delete/rename/move：二次确认；delete 先永久删（回收站后置）。
- ✅ 验收：远程项目可浏览/打开/编辑/保存/新建/删除；本地项目零回归。

### 阶段 3 · 远程终端
- [ ] 3.1 `term.rs`：`term_open` 增加可选 `prog`/`args`（缺省=旧行为，零回归）。
- [ ] 3.2 前端 `term.js`：支持以 `ssh -tt alias`（或 `tailscale ssh`）开新终端；cwd 用远程 root。
- [ ] 3.3 关闭窗口清理终端进程，但**不销毁共享 ControlMaster 连接**。
- ✅ 验收：IDE 内开远程交互终端可用；关窗无残留进程。

### 阶段 4 · 远程窗口与 UX
- [ ] 4.1 创建/编辑项目时填远程信息（`ui/app.js` 项目弹窗）。
- [ ] 4.2 状态栏显示 `SSH: alias — /root`；文件树/Git/终端带远程标识。
- [ ] 4.3 连接失败/权限失败/命令缺失：结构化错误码 + 中文提示 + 降级。
- ✅ 验收：本地/远程项目视觉可区分，不混淆。

### 阶段 5 · 远程 Git
- [ ] 5.1 `git.rs`：抽出 `run_git`，远程走异步子进程（`tokio::process`），本地保持。
- [ ] 5.2 status/diff/stage/commit/branch/log（diff/log/blame 流式 + 上限 + 取消）。
- [ ] 5.3 push/pull/fetch（网络长任务、误操作确认；不做自动重试）。
- ✅ 验收：远程仓库 Git 面板可用；Git 缺失仅禁用该面板，不影响文件/终端。

### 阶段 6 · P1 扩展
- [ ] 6.1 远程 `rg` 搜索（缺 rg 回退 grep；流式 + 取消；参数走 stdin 不拼接）。
- [ ] 6.2 单文件上传/下载（sftp/协议流）。
- [ ] 6.3 本地端口转发 `-L`（独立 `ssh -N -L` 子进程，纳入连接状态管理）。
- [ ] 6.4 Tailscale 设备发现（`tailscale status --json`；operator 权限不足时提示 `sudo tailscale set --operator=$USER`，不自动提权）。
- [ ] 6.5 多远程窗口（连接引用计数）、断线重连（读可重试一次，写不自动重放）、ProxyJump。

### 阶段 7 · P2（视需要）
- [ ] 7.1 远程文件轮询刷新（2~5s，仅已打开文件与展开目录）。
- [ ] 7.2 远程回收站 `.ligong-ide-trash` + 清理策略。
- [ ] 7.3 远程 `-R`/SOCKS `-D`、exit node、断点续传。

---

## 三、关键约束（全程遵守）
- 新增命令先校验：`project_id` 存在、项目类型、连接状态；不得让远程项目落入本地 `PathBuf` 分支。
- 所有远程写操作必须有超时与取消语义；写操作**不自动重试**。
- 日志脱敏：不记录密码、私钥、含秘密的完整命令。
- 凭据：项目 JSON 只存 alias/host/user/port/root/key_path；不存密码/私钥/token。
- 每阶段独立可验收，未过验收不进下一阶段。

---

## 四、设计决策（两轮子 agent 评审定稿）

### 4.1 寻址键（对齐 VSCode Uri scheme）
- 所有文件/Git 操作内部寻址从 `PathBuf` 改为 **`(BackendId, rel_path)`**：`BackendId` = `Local` 或 `Ssh(project_id)`。
- 对外 `ide_*` 命令签名不变，内部由 `backend_for(project_id) -> Arc<dyn FileBackend>` 分派。
- **共享层**（不复制进后端）：`MAX_READ_BYTES`、`natord_cmp`、二进制/编码探测、restrict 策略。

### 4.2 trait 拆分（避免上帝 trait）
- `FileBackend`：**必需仅 4~5 个**（`stat/read/write/list` + `create/delete/rename`），`copy/move/watch` 为**可选能力**（默认 `Unsupported`，学 Go `io/fs.FS`）。
- `ExecBackend`：`exec(cmd) -> Output`（超时/取消/退出码）。
- `GitBackend`：内部基于 `ExecBackend` 组合，不独立造轮子。
- `SearchBackend`：独立（rg→grep 降级）。
- 能力集用 `Capabilities { has_git, has_rg, has_sftp, ... }` 结构表达，不给核心 trait 加 `supports_x()`。

### 4.3 传输与连接（子 agent 结论）
- `ControlMaster=auto` + `ControlPath` + `ControlPersist=60s`；**不自研连接池**，只管子进程与 socket 生命周期。
- 显式 `ServerAliveInterval=15` + `ServerAliveCountMax`。
- 文件传输走 `sftp -b -`（命令从 stdin），不解析 `ls`/人类文本。
- **Windows-as-client：OpenSSH 不支持 ControlMaster** → 能力探测显式检测并降级（连接不复用，仍可用）。
- 命令注入防护：`ssh host '<cmd>'` 是双层 shell，用户输入路径/搜索词/参数**走 stdin 或严格 quoting**，不拼字符串。

### 4.4 错误与可调试性（**优先级前移**）
- 引入 `thiserror` 分层错误域：`TransportError`（连不上/超时/host key）→ `BackendError`（越界/不存在/权限）→ `CapabilityError`（远端缺 git/rg）。
- 错误**可机器判定**（枚举/码），前端才能重试/提示/降级，不匹配中文字符串。
- 日志：现仅 Windows 落盘（`lib.rs` 的 `#[cfg(windows)]`），**补非 Windows 也落文件**，供远程调试。
- 前端加集中错误处理（`unhandledrejection` + toast），替代散落的 `statusPath.textContent = "...失败"`。
- 提供 debug 命令 dump 当前 backend/连接状态。

### 4.5 代码结构审核补充（已纳入阶段清单）
- `project_root`/`project_restrict` **三处重复**：`fs.rs:77,90`、`git.rs:45`、`preview.rs:27,43` → 阶段 2 统一收敛（原方案漏了 `preview.rs`）。
- `.ligong-ide-trash` 内联在 `fs.rs:74,211-291`；`suppress_paths`（`fs.rs:61`）与本地 watcher 耦合 → Backend 分派后**仅 Local 调用**。
- 前端 `switchView`（`ide.js:982`）硬编码 if/else，非插件化 → 阶段 4 需改造为可注册视图。

### 4.6 关键路线结论
- **传输层选 A（系统 OpenSSH）**；`russh` 仅作"目标平台无 OpenSSH"兜底（如部分 Windows）；`ssh2`（C 依赖）不推荐。
- **寻址用 Uri 而非裸路径**是 VSCode Remote 的核心经验：机器边界下沉为 Backend 边界，命令层不感知本地/远程。

---

## 五、实际进度（实时更新）

### 已完成
- **阶段0**：`RemoteSpec` + `remote_by_project` + `remote_probe`/`remote_test_connection`。
- **阶段1**：`src/ide/ssh/mod.rs`（exec 异步 + ControlMaster + host key 指纹 + close_master）。真机 gongwei 验证通过。
- **阶段2**：`src/ide/backend.rs`（list/read/write/create/delete/rename/read_binary，POSIX）。`fs.rs` 6 命令 + `preview.rs` 按 project_id 分派。本机 localhost 验证通过。
- **阶段3**：`term_open` 加 `prog/args`；`api.js::termOpen` 扩展；`term.js` `newRemoteTerminal`。
- **阶段4**：`window.rs` URL/标题带远程标识、远程跳过本地 watch；`ide.js` 项目名显示远程；项目 modal 自动列出 **SSH config 主机**（`remote_ssh_hosts`）+ **Tailscale 设备**（`remote_tailscale_devices`）+ **远程目录浏览**（`remote_browse`）。
- 命令：`set/get/clear_project_remote`、`remote_ssh_hosts`、`remote_tailscale_devices`、`remote_browse`、`remote_host_key`、`remote_exec`。

### 已完成（续）
- **Windows 远端适配（阶段2 扩展）**：新增 `src/ide/backend_win.rs`（pwsh 7 + `-EncodedCommand` UTF-16LE base64，stdin 传大内容）；`backend.rs` 加 `normalize_win`/`win_resolve`（大小写不敏感越界）/`ps_lit`，7 个文件操作按 `is_windows_remote` 分派；`is_windows_remote` 加进程内缓存；`remote_browse` Windows 分支改 `exec_ps`。**真机 gongwei 验证通过**（列目录/写含中文+尖括号文件/读回/新建/改名/删除）。gongwei 已升级 PowerShell 7.4.6。
- **阶段5 远程 Git**：`git.rs` 引入 `GitTarget`（本地 PathBuf / 远程 RemoteSpec+root）；`run_git!` 宏 38 处调用统一改异步；远程 POSIX 走 `git -C`（shell_quote），Windows 走 pwsh（`& git -C`，单引号字面量 + `exit $LASTEXITCODE`）；`clone`/`discard` 按本地/远程分支处理。**真机 localhost 验证通过**（init/add/commit/log/show 往返）。
- **阶段6.1 内容搜索**：新增 `src/ide/search.rs`（本地 rg→grep 回退；远程 POSIX rg/grep、Windows `Select-String -SimpleMatch`；`-F` 固定串；500 条上限）。命令 `ide_search_content`。前端搜索框以 `#` 前缀切换内容模式，`tree.js::searchContent` 分组渲染。**真机 localhost + gongwei 验证通过**。
- **阶段6.2 上传/下载**：新增 `src/ide/transfer.rs`（`write_remote_binary` 走 base64+stdin，POSIX `base64 -d`、Windows `[IO.File]::WriteAllBytes`；64MiB 上限）。命令 `ide_upload_file`/`ide_download_file`/`ide_upload_paths`/`ide_list_remote_files`。前端右键菜单加"上传/下载"（仅远程项目），`capabilities` 加 `dialog:allow-save`。**真机 localhost + gongwei 验证通过**（含中文名二进制往返）。

### 关键待办（下一步）
- **阶段6 剩余**：6.3 `-L` 端口转发、6.4 Tailscale ssh 终端接入、6.5 多窗口/重连。
- 前端 `switchView` 插件化、错误结构化、日志落盘（评审结论 4.4/4.5）。
- 重新打包 targz（有头）供端到端实测。
- 注意：gongwei 未安装 git；Windows 远程 Git 需远端先装 git。

### 教训
- `write_file` 写入含尖括号字面量会被解析器破坏 → 用 Python `chr(60)/chr(62)` 构造。
- gongwei（Windows）PowerShell 不认 POSIX 命令，已触发 `FullyQualifiedErrorId: InvalidEndOfLine`。
