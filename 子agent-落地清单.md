# 子 Agent 功能 —— 分步落地清单

> 原则：**增量、一步一验、绝不损害原有功能**。每完成一项打勾并手测；未通过不得进入下一步。
> 重试=再次调用同名子 agent（空闲即续跑）；结果=好了就随当时那次直接发；子 agent 只读，写操作申请主 agent。
>
> **进度**：
- 阶段 1 已完成（最小重构：新增 `EmitCtx` + 替换 4 处 emit，控制流零变化，编译通过、手测无回归）。
- 阶段 2 已完成（`Inner.sub_runs` + `SubRunStatus`/`SubRunCtx`；新增 `clone_profile`/`ensure_sub_bridge`/`release_sub_bridge`/`copy_profile_dir`；`src/subagent.rs` 骨架 + `lib.rs` 注册；主 agent 未接线，编译通过）。
- 阶段 3 已完成（新增账号级 `throttle_account`；三处请求节流改账号级；移除 `run_turn` 里覆盖整 turn 的 `account_lock`；多项目/多账号并发正常，无回归）。
- 阶段 4A 已完成（数据层：`SubAgent`/`SubAgentConv` + 项目归属 + 增删查；命令 `list_sub_agents`/`delete_sub_agent`/`delete_sub_agent_conv`；删除时删服务端会话）。
- 阶段 4B-1 已完成（`agent_loop` 就绪，仅供子 agent 用；`run_turn` 未动；编译通过）。
- 阶段 4C 已完成（`run_subagent` 对外入口：独立 bridge + 按 name 复用会话 + `sub_runs` 注册 + `agent://sub-*` 事件；尚未被工具调用）。
- 阶段 5 已完成（三工具登记 + `execute_tool` 分支 + `run_sub_agent_tool`；路线 1 落地；编译通过）。
- 阶段 6 已完成（提示词：主 agent 去 browser + 加 sub_agent 三工具说明与规则；三处工具名清单更新；子 agent 提示词就绪）。
- 阶段 5 补充：`parser::validate` 加 sub_agent 三工具参数校验（修复调 `sub_agent` 报"格式错误"）。
- 阶段 0 地基验证：**已通过**（子 agent 端到端跑通 + 免登录）。

---

## 阶段 0：地基验证（不写业务代码，只实验，决定方案是否成立）

- [x] 0.1 复制 `~/.deepseek-agent/accounts/<acc>/browser` 到临时目录（排除 `Singleton*`/`lockfile`/`DevToolsActivePort`），用 `BridgeModel::launch` 起 bridge，确认**免登、能 ask**。
- [x] 0.2 同账号开**两个**独立 profile 的 bridge，同时 `ask`，观察是否触发风控/验证码/互踢/掉登录。
- [x] 0.3 记录结论。若 0.1 或 0.2 失败 → 本方案（单账号多 bridge）不成立，需改跨账号或退回串行，**暂停后续**。

---

## 阶段 1：抽出通用 `agent_loop`（行为零变化）

> 目标：把 `src/runner.rs::run_turn` 的核心循环抽成可复用函数，主 agent 行为完全不变。

- [x] 1.1 定义 `AgentLoopCtx`（含 `project_id`、`model`、`session_id`、`event_prefix`、`tools_allow`、`system_prompt`、`first_input`、`cancel`、`persist`、`sub_id/name`(可选)）。
- [x] 1.2 把 `run_turn` 主体循环（现 `runner.rs` 约 468–775 行）移入 `agent_loop(ctx, state) -> Result<LoopOutcome>`；`LoopOutcome { text, ok, reason }`。
- [x] 1.3 `run_turn` 改为薄适配器：构造 `AgentLoopCtx`（`event_prefix=""`、主工具集、主 system_prompt）后调用。
- [x] 1.4 `BusyGuard`/`DoneGuard`/交接报告/`projects` 持久化 仍留在 `run_turn` 适配器，不进 `agent_loop`。
- [x] 1.5 验证：`cargo build`；手测发消息、工具调用、审批、交接报告、多项目并发**全部与改前一致**。
- [x] 1.6 提交（独立 commit），标记"纯重构"。

---

## 阶段 2：状态与 bridge 池（先不动主流程）

- [x] 2.1 `src/state.rs`：`Inner.bridges` 键由 `account` 改为 `bridge_key`（主=`<account>`，子=`<account>#sub-<sid>`）。**保持主 agent 用 `<account>` 键，行为不变**。
- [x] 2.2 新增 `Inner.sub_runs: HashMap<String /*sid*/, SubRunCtx>`（`name/duty/project_id/account/status/cancel/started_at/attempts/last_result`）。
- [x] 2.3 新增 `bridge_pool`：`ensure_sub_bridge(account, sid)`（复制 profile → `BridgeModel::launch`）；`snapshot_master_profile`、`clone_profile`、`release_bridge`。
- [x] 2.4 复制排除项：`SingletonLock`/`SingletonSocket`/`SingletonCookie`/`DevToolsActivePort`/`lockfile`。
- [x] 2.5 验证：编译；主 agent 功能不变（此时子 bridge 未被调用）。

---

## 阶段 3：限流改造（账号级）

- [x] 3.1 `src/state.rs`：新增账号级 limiter（`HashMap<account, Arc<Mutex<Instant>>>`），保留全局作为资源上限。
- [x] 3.2 `src/runner.rs`：`state.throttle(GLOBAL_REQUEST_GAP, …)` 改为 `state.account_throttle(account, gap, …)`（3 处：489/596/621）。
- [x] 3.3 移除 `run_turn` 里 `account_mutex.lock()` **覆盖整个 turn** 的用法（`runner.rs:396`）——account 锁仅用于 bridge 初始化/profile 操作。
- [x] 3.4 验证：单账号主 agent 仍正常限速；多项目并发不回归。

---

## 阶段 4：子 agent 运行器 `run_subagent`

> 修订：子 agent 对话**内容不落本地**；本地只存子 agent 身份 + 会话段引用（`SubAgent.conversations: Vec<SubAgentConv>`），用于复用/压缩链/删除。删除子 agent 或单段时**必须删服务端会话**（`Model::delete_session` 已存在）。

### 阶段 4A：数据层（已完成）
- [x] 4A.1 `projects.rs`：`SubAgent`/`SubAgentConv` + `Project.sub_agents` + 方法（list/find/add/push_sub_conv/set_sub_parent/active_sub_conv/remove_sub_agent/remove_sub_conv）。
- [x] 4A.2 `commands.rs`：`list_sub_agents`/`delete_sub_agent`/`delete_sub_agent_conv`（删本地 + 删服务端会话）。
- [x] 4A.3 `lib.rs` 注册命令。
- [x] 4A.4 编译通过（`Model::delete_session` 已存在，非假通过）。

### 阶段 4B-1：新增 `agent_loop`（仅供子 agent 用，`run_turn` 不动）——已完成
- [x] 4B-1.1 `runner.rs` 放宽可见性：`AnswerStreamGate`(+new/push/finish_text)、`send_delay`、`find_tool_marker`、`ends_with_tool_prefix`、`execute_tool`、`REPORT_PROMPT`、`HANDOFF_INTRO` → `pub(crate)`。
- [x] 4B-1.2 `prompt_gui.rs` 加 `SUBAGENT_PROMPT` / `SUBAGENT_TOOLS_SECTION`（占位，阶段 6 细化）。
- [x] 4B-1.3 `subagent.rs` 实现 `agent_loop`（独立循环：压缩/工具/事件 `agent://sub-*`，只读工具集，内容不落本地）。
- [x] 4B-1.4 编译通过。
- [ ] 4B-2（后期可选）：把 `run_turn` 也切到 `agent_loop`，统一收口。
- [ ] **保留项：路线 2（后期可选）**——把 `AppState` 改为 `Arc` 管理（`manage(Arc::new(AppState))` + 命令 `state::<AppState>`→`state::<Arc<AppState>>`），使 `SubLoopCtx` 可持有 `Arc<AppState>`。当前走路线 1（`AppHandle` + `state.inner()`），功能等价、风险更低；路线 2 仅在后期需要跨任务长持 state 时再考虑，**非必需**。

### 阶段 4C：`run_subagent` 对外入口——已完成
- [x] 4C.1 `SubAgentSpec` + `resolve_sub_agent`（按 name 复用同名子 agent 的 active 段续跑；否则新建 SubAgent + 首段）。
- [x] 4C.2 `run_subagent`：取项目账号 → `ensure_sub_bridge`（独立 profile/bridge）→ 解析/复用 session → 注册 `sub_runs` → 发 `agent://sub-start` → `agent_loop` → 更新状态 + 发 `agent://sub-done`。
- [x] 4C.3 编译通过（`SubRunCtx` 字段匹配）。

- [ ] 4.1 新 `src/subagent.rs`：`SubAgentManager`（创建/关闭/状态/结果暂存/取消级联）。
- [ ] 4.2 `run_subagent(project_id, sid, name, duty, task, model)`：独立 session + `ensure_sub_bridge` + 调 `agent_loop`（`event_prefix="sub"`、子工具集）。
- [ ] 4.3 子 agent **只读工具集**：`read_file`/`execute_command`(只读用法)/`browser`；**禁** `write_file`/`edit_file`/`sub_agent`。
- [ ] 4.4 状态机：`Starting→Running→(Succeeded|Failed|Cancelled|Closed)`；无自动重试。
- [ ] 4.5 结果 `SubAgentResult{ sid,name,duty,status,text,ok,reason }` 写入 `sub_runs`。
- [ ] 4.6 验证：直接调 `run_subagent`（临时命令或测试）能跑通、能返回结果；主 agent 不受影响。

---

## 阶段 5：工具与 parser——已完成
- [x] 5.1 `KNOWN_TOOLS` 登记 `sub_agent`/`sub_agent_close`/`sub_agent_status`。
- [x] 5.2 `execute_tool` 加子 agent 工具分支（`Box::pin` 断 async 递归）。
- [x] 5.3 `run_sub_agent_tool`：`sub_agent`（一主一子上限校验 → `run_subagent`）、`sub_agent_close`（取消 cancel token）、`sub_agent_status`（列状态）。
- [x] 5.4 子 agent 循环走路线 1（`SubLoopCtx` 用 `AppHandle` + `state.inner()`），不持有 `Arc<AppState>`。
- [x] 5.5 编译通过。
- [ ] 5.6 主 agent 去 browser（提示词 `TOOLS_SECTION_GUI` 删 browser 段）——**并入阶段 6 一起做**。

## 阶段 6：提示词——已完成
- [x] 6.1 `TOOLS_SECTION_GUI`：browser 工具段 → `sub_agent`/`sub_agent_close`/`sub_agent_status` 说明；主 agent 无 browser。
- [x] 6.2 三处"工具名清单"（`TOOLS_SECTION_GUI` 的 Important、`FORMAT_ERROR_PROMPT`、`DSML_ERROR_PROMPT`）更新为含 sub_agent 三工具、去 browser。
- [x] 6.3 `TOOLS_SECTION_GUI` 加主 agent 规则（总体计划+派子 agent、失败重试≤3、回复附子 agent 状态）。
- [x] 6.4 `SUBAGENT_PROMPT` / `SUBAGENT_TOOLS_SECTION`（子 agent 只读 + 可 browser + 禁写/禁开子 agent）。
- [x] 6.5 编译通过（browser 残留均为有意保留）。

## 阶段 6 原始说明：提示词

- [ ] 6.1 `src/prompt_gui.rs::TOOLS_SECTION_GUI`：移除 browser；加 `sub_agent`/`sub_agent_close`/`sub_agent_status` 说明与规则（命名、专职、主 agent 只读子结果、失败由主 agent 决策重试≤3、3 次失败给用户方案、无用则关闭）。
- [ ] 6.2 新增 `SUBAGENT_PROMPT`（子 agent 系统提示：专职、只读、结论简洁、可 browser、禁写、禁开子 agent、需要写时在结论里"申请写操作"）。
- [ ] 6.3 验证：模型能正确申请子 agent；子 agent 收到正确提示。

---

## 阶段 7：子 agent 状态注入与结果拼接

- [ ] 7.1 `agent_loop` 回灌工具结果前，追加 `[系统] 本项目子agent：共 N（运行 M）—— name1(status)…`。
- [ ] 7.2 子 agent 完成：结果存 `pending_results`，**不单独发用户最终消息**。
- [ ] 7.3 **结果发送策略（已定）**：主 agent 回复立即发；子 agent 结果**好了就随当时那次直接发**（若主 agent 空闲则直接发一条子结果消息；若主 agent 正在跑则并入其下一次输出）。
- [ ] 7.4 主 agent 完成/空闲时，取 `pending_results` 拼接（`### 子agent结果` 段落），标记已合并。
- [ ] 7.5 验证：主/子并行时，子先完成、主后完成，两者结果都正确呈现且不重复。

---

## 阶段 8：事件与前端

- [ ] 8.1 新增事件：`agent://sub-start`、`agent://sub-delta`、`agent://sub-tool`、`agent://sub-done`、`agent://sub-closed`、`agent://sub-count`（均带 `project_id/sub_id/name`）。
- [ ] 8.2 `ui/app.js`、`ui/ide/agent/{events,store,render}.js`：监听 `agent://sub-*`，按 `name` 建折叠卡，实时显示；`agent://sub-count` 更新状态栏"子agent M/N"。
- [ ] 8.3 子 agent 结果只作为主消息的一部分（折叠）展示，不单独成气泡。
- [ ] 8.4 验证：前端不回归；子 agent 卡片与状态栏正确。

---

## 阶段 9：取消级联与收尾

- [ ] 9.1 `src/commands.rs::interrupt`：主 agent 取消 → 遍历取消其所有子 agent（`cancel`），子 bridge stop/关闭。
- [ ] 9.2 项目关闭/窗口退出时关闭所有子 agent（可选）。
- [ ] 9.3 子 agent 结束后回收其 bridge/profile（延迟删，保留诊断）。
- [ ] 9.4 验证：interrupt 时主+子一起停；无孤儿 chromium 进程。

---

## 阶段 10：打包与回归

- [ ] 10.1 全量手测：主 agent 原有全部功能（发消息/工具/审批/交接/多项目/IDE 窗口）**零回归**。
- [ ] 10.2 子 agent 全流程：开→跑→结果→重试→关闭；一主一子并行。
- [ ] 10.3 `scripts/package_linux.sh` 打包（无头、无 seed cookie）。
- [ ] 10.4 记录已知问题与"需人工确认"项。

---

## 红线（每步必须遵守）
1. 不改动与本步无关的代码。
2. 每步 `cargo build` 通过 + 手测主 agent 无回归，才进下一步。
3. 阶段 0 未通过前，不写阶段 4 及以后。
4. 不确定即标注"需确认"，不臆造。

---

## 进度记录（本会话）

### 阶段 7A —— 完成
- 根因：`run_subagent → agent_loop → execute_tool → run_sub_agent_tool → spawn run_subagent` 异步递归环，编译器无法证明 Send。
- 修复：抽出 `dispatch_tool`（主 agent 专用，含 sub_agent 三工具）；`agent_loop` 只走 `execute_tool`（不含三工具），环断。
- `run_turn` 762 行改调 `dispatch_tool`；`execute_tool` 删除 sub_agent 分支。
- `sub_agent` 分支改 spawn + 立即返回"已派发"。

### 阶段 7B —— 完成
- `state.rs`：`SubResult` + `Inner.pending_sub_results` + `Inner.start_turn`（`StartTurnFn` 函数指针，打破递归环）。
- `commands.rs`：抽 `start_turn(app, project_id, input)`，`send_message` 改调它；`start_turn` 收尾（busy=false 后）取 pending 非空则经 `start_turn` 函数指针再唤起。
- `lib.rs`：setup 注入 `start_turn` 函数指针。
- `run_turn` `Calls` 分支注入 pending 子结果 + `SUB_REVIEW_PROMPT`。
- `subagent.rs`：`run_subagent` 结束 push 结果；忙→pending，空闲→经函数指针唤起。
- `SUB_REVIEW_PROMPT` 定义于 `runner.rs:180`。

### 阶段 7 补充（本会话）
- `run_turn` `Calls` 分支：工具结果 + pending 子结果后，**系统自动注入**子 agent 状态行
  `[系统] 本项目子agent：共 N（运行 M）—— name1（状态）、…`。
- 删除 `prompt_gui.rs` 里"模型自己输出状态行"的错误规则（原 125 行）。
- 删除 `sub_agent_status` 工具（7 处）：prompt_gui 工具说明+4 处清单、runner matches+分支、parser KNOWN_TOOLS+validate。
  理由：状态行已自动注入，该工具冗余。
