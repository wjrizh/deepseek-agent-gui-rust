# IDE 远程插件方案（Tailscale + SSH Remote）— 待评审

> 目标：为「力工Studio-Rust」的 IDE 加入远程开发能力（类 VSCode Remote-SSH）：
> 通过 Tailscale / SSH 连接远程主机，**远程浏览、编辑、增删改文件、跑终端、执行 Git、端口转发**。
> 本文档用于外部 AI 评审与补全，请给出：**实施功能全清单 + 优化意见 + 风险与取舍**。

---

## 0. 项目背景（评审者必读）

- 技术栈：**Tauri 2 + Rust 后端 + 原生 JS 前端**（无框架、无打包器，直接 `invoke` 调命令）。
- 现有 IDE（`src/ide/`）已具备：文件树、Monaco 编辑器、PDF/图片预览、Git 面板、**内嵌 PTY 终端**（`portable_pty` + xterm）。
- IDE 每个窗口绑定一个 `project_id`；所有文件/Git 命令用 `project_id` → `project_root()` 解析**本地根目录**，直接 `std::fs` / `git`。
- 项目风格：**大量功能通过「shell 出去调系统命令」实现**（git、tailscale 均如此），而非引库。
- 约束：**增量、一步一验、零回归**；改动要最小且不破坏现有本地功能。
- 本机已装：`tailscale 1.102.5`（已登录）、`ssh`、`sshd`。z 用户在 wheel 组、sudo 免密。

### 现有可复用资产
| 能力 | 路径 | 复用点 |
|---|---|---|
| PTY 终端 | `src/ide/term.rs`（`term_open(id,cwd,shell,cols,rows,on_event)`） | 跑 `ssh`/`tailscale ssh` 直接复用 |
| 文件命令 | `src/ide/fs.rs`（list/read/write/delete/restore/create/copy/move） | 抽成 Backend |
| 路径安全 | `src/ide/path.rs`（`resolve(root,rel,restrict,...)` 越界/软链校验） | 远程需 POSIX 版 |
| Git 命令 | `src/ide/git.rs`（约 26 个命令） | 改执行位置 |
| 文件监听 | `src/ide/watch.rs`（notify，本地 inotify） | 远程需轮询 |
| IDE 窗口 | `src/ide/window.rs`（`open_ide_window`） | 远程项目模式 |
| 侧边栏视图 | `ui/ide/index.html`（`act-item data-view=`）+ `ui/ide/ide.js` | 加「远程」视图 |

---

## 1. 总体设计

### 1.1 核心思想：后端抽象（Backend Abstraction）
把所有「文件/执行」操作抽成 `Backend`，IDE 命令按项目类型分派：

```
              ┌─────────────┐
  IDE 命令 ──▶│  Backend    │──▶ Local(std::fs / 本地 git)
 (fs/git/…)   │  (trait)    │──▶ Ssh(远程主机, POSIX)
              └─────────────┘
```

**为什么必须这样**：现有 `ide_list_dir/ide_read_file/...` 硬编码本地 `std::fs`。要支持远程而不复制整套命令，就得在后端分派。

### 1.2 传输层三条候选路线（**请评审者选型**）
| 方案 | 依赖 | 优点 | 缺点 |
|---|---|---|---|
| A. 系统 `ssh` + ControlMaster | 零新依赖 | 与项目风格一致；SFTP 走 `sftp`/`scp`；认证/known_hosts/config 全免费 | 解析文本输出脆弱；并发控制复杂 |
| B. `russh`（纯 Rust, tokio） | +crate | SFTP 二进制可靠；无外部进程；跨平台一致 | 体积大；需自实现认证/agent/known_hosts |
| C. `ssh2`（libssh2 C） | +crate+C 库 | API 成熟 | C 依赖，打包/交叉编译麻烦 |

> 倾向 A（贴合现有 shell-out 风格 + 复用用户 ssh 配置），但请评估 B 是否更稳。

### 1.3 连接与项目模型
- `Project` 增加 `remote: Option<RemoteSpec>`：`{ host, user, port, root, auth, proxy_jump, forwards[] }`。
- 远程项目 = 一个 `project_id` 指向远程 `root`（如 `/home/u/app`）。
- 一个主机一个连接（ControlMaster/pool），多个项目可共享。

---

## 2. 改动路径清单

### 2.1 后端抽象（Rust）
| | 文件 | 说明 |
|---|---|---|
| 🆕 | `src/ide/backend/mod.rs` | `Backend` trait + 工厂（按 project_id 选 Local/Ssh） |
| 🆕 | `src/ide/backend/local.rs` | 迁移现有 `fs.rs` 的 `std::fs` 实现 |
| 🆕 | `src/ide/backend/ssh.rs` | SSH 后端：各 fs 操作 |
| 🆕 | `src/ide/backend/transport.rs` | 传输层抽象（ControlMaster / SFTP / russh） |
| 🆕 | `src/ide/ssh/pool.rs` | SSH 连接池 + ControlMaster socket 生命周期 |
| 🆕 | `src/ide/ssh/auth.rs` | 密钥/agent/密码、known_hosts、host key 校验 |
| 🆕 | `src/ide/ssh/forward.rs` | 本地/远程/SOCKS 端口转发 |
| 🆕 | `src/ide/remote.rs` | Tailscale 状态、SSH config 解析、连接测试 |
| ✏️ | `src/ide/fs.rs` | 改为走 `Backend`，不再直连 `std::fs` |
| ✏️ | `src/ide/path.rs` | 远程 POSIX 路径 + restrict 后端感知 |
| ✏️ | `src/ide/git.rs` | Git 按后端在远程执行 |
| ✏️ | `src/ide/watch.rs` | 远程轮询 mtime 策略 |
| ✏️ | `src/ide/mod.rs` | 注册 `backend`、`ssh`、`remote` |
| ✏️ | `src/state.rs` | 新增 SSH 连接池全局状态 |
| ✏️ | `src/projects.rs` | `Project.remote: Option<RemoteSpec>` |
| ✏️ | `src/commands.rs` | 建/改项目带 remote；连接测试/断开 |
| ✏️ | `src/lib.rs` | 注册新命令 |
| ✏️ | `Cargo.toml` | 视选型加 `russh` 或无新依赖 |

### 2.2 前端
| | 文件 | 说明 |
|---|---|---|
| 🆕 | `ui/ide/remote.js` | 「远程」侧边栏：设备/主机列表、连接、状态 |
| 🆕 | `ui/ide/remoteExplorer.js` | 远程文件树（复用 tree.js + backend 标记） |
| 🆕 | `ui/ide/forward.js` | 端口转发面板 |
| ✏️ | `ui/ide/api.js` | 远程命令封装 |
| ✏️ | `ui/ide/tree.js` | 接受 backend 参数 |
| ✏️ | `ui/ide/editor.js` | 读写经 backend |
| ✏️ | `ui/ide/term.js` | 打开 `ssh`/`tailscale ssh` 终端 |
| ✏️ | `ui/ide/ide.js` | 远程项目模式、状态栏 `SSH: host` |
| ✏️ | `ui/ide/index.html` | 侧边栏远程入口 + 远程指示 |
| ✏️ | `ui/ide/ide.css` | 样式 |
| ✏️ | `ui/app.js` / `ui/index.html` / `ui/style.css` | 项目创建/设置填远程信息 |

---

## 3. 功能全清单（请外部 AI 补全、排序、标注 P0/P1/P2）

### 3.1 连接与认证
1. 目标来源：`~/.ssh/config`、Tailscale 设备、手动 `user@host:port`
2. 认证：私钥 / `ssh-agent` / 密码 / `ProxyJump` 多跳
3. Host key 校验与 `known_hosts` 写入策略
4. 凭据安全存储（不落明文）
5. 连接保活（ServerAliveInterval）、断线自动重连

### 3.2 远程文件系统（核心）
6. 目录树浏览、懒加载、大目录性能
7. 打开/编辑/保存远程文件（二进制检测、编码）
8. 新建/改名/删除/复制/移动/回收站
9. 远程全文搜索（grep/ripgrep）
10. 远程文件监听（轮询 vs inotify-over-ssh）
11. 上传/下载（拖拽、目录递归、断点）

### 3.3 终端与执行
12. 交互式远程终端（复用 `term.rs` PTY 跑 `ssh`）
13. 远程命令/任务执行与输出
14. 附加远程进程/日志

### 3.4 Git（远程）
15. 远程 status/diff/stage/commit/push/pull/branch/log/blame/stash（复用 `git.rs` 逻辑改执行位置）

### 3.5 端口转发与网络
16. 本地转发 `-L` / 远程转发 `-R` / 动态 SOCKS `-D`
17. Tailscale：设备列表、`tailscale ssh`、exit node、状态刷新

### 3.6 多机与体验
18. 连接管理（保存/编辑/最近）
19. 多远程窗口、每主机一 IDE 窗口
20. 状态指示（在线/延迟/当前主机/当前根）
21. 失败降级与错误提示

---

## 4. 请评审者回答的开放问题

1. **传输层选型**：A（系统 ssh+ControlMaster）/ B（russh）/ C（ssh2），结合本项目 shell-out 风格、跨平台打包、零回归，选哪个？理由？
2. **Backend 抽象方式**：改 `fs.rs` 分派 vs 复制一套 `ssh_fs` 命令，哪种对零回归更稳？
3. **远程文件监听**的可行方案与成本（轮询频率、SSH 通道复用）。
4. **远程 Git**：直接 `ssh host 'git -C root ...'` 还是 SFTP+本地 git？blame/diff 大输出如何处理？
5. **远程搜索**：`ssh host 'rg ...'` 流式返回 vs SFTP 遍历。
6. **端口转发**用系统 `ssh -L/-R/-D` 还是 russh 直连通道？
7. **Windows 双向兼容**：Windows 作客户端 / 作远端（`sshd`、PowerShell 后端）。
8. **安全面**：命令注入、路径逃逸、凭据、host key、TOFU。
9. **restrict 越界保护**如何映射到远程根？
10. **性能**：文件树/搜索/大文件读写的延迟优化（连接复用、批量 SFTP）。
11. **UX**：远程/本地项目如何区分与切换，避免用户混淆。
12. **失败与降级**：断线、权限、命令缺失（无 rg/git）的提示与回退。
13. 是否有更简单的**中间方案**（如仅远程终端 + 远程 Git，不做远程文件树）能覆盖 80% 需求？
14. **凭据存储**用 OS keyring（`keyring` crate）还是本项目自管文件？
15. **Tailscale 权限**：`tailscale status` 需 operator 时的处理（提示 `sudo tailscale set --operator=$USER`）。

---

## 5. 落地阶段（待评审确认）

- **P0**：Backend 抽象 + SSH 后端（浏览/读写/增删改）+ 远程终端 + 连接管理。
- **P1**：远程 Git、远程搜索、上传下载、端口转发、Tailscale 集成。
- **P2**：远程 watch、ProxyJump/多跳、断线重连、exit node。

---

## 6. 评审输出格式（请外部 AI 按此回复）

1. **选型结论**（传输层/Backend 抽象/凭据存储）。
2. **功能全清单**：逐条给 P0/P1/P2 + 依赖 + 风险。
3. **对本方案的优化/修正**（指出设计缺陷）。
4. **最小可用路径（MVP）建议**。
5. **不建议做的部分**（避免过度工程）。