以下方案基于当前源码中已核实的结构：src/runner.rs::run_turn、src/state.rs::{Inner,RunCtx,AppState}、src/commands.rs::{send_message,interrupt,approve_command}、vendor/deepseek-agent/src/bridge/model.rs::BridgeModel、bridge_server.mjs、agent/parser.rs::KNOWN_TOOLS、src/prompt_gui.rs 以及前端 ui/app.js、ui/ide/agent/*。

## A. 整体架构与模块划分

建议增加一个“Agent 编排层”，将主 Agent 与子 Agent 视为同一种运行单元，只是角色、工具白名单、父子关系和输出策略不同。

### 1. src/sub_agent.rs

职责：

- 子 Agent 定义、创建、关闭、重试。
- 子 Agent 状态机。
- 子 Agent 名称和专职职责校验。
- 子 Agent 结果暂存与主 Agent 汇总。
- 主取消信号向子 Agent 级联。
- 子 Agent 计数、运行状态查询。

建议核心结构：

- AgentRole
  - Main
  - Sub { duty: String }
- SubAgentSpec
  - id
  - name
  - duty
  - parent_project_id
  - account
  - tool_allowlist
  - max_retries
  - depth
- SubAgentState
  - Created
  - Starting
  - Running
  - WaitingRetry
  - Succeeded
  - Failed
  - Cancelled
  - Closed
- SubAgentRecord
  - 子 Agent 元数据
  - 当前状态
  - 当前重试次数
  - CancellationToken
  - 独立 session_id
  - 独立 BridgeModel
  - 结果暂存对象
- SubAgentResult
  - agent_id
  - name
  - duty
  - status
  - summary
  - artifacts
  - error
  - finished_at

建议接口签名：

- create_sub_agent(spec: SubAgentSpec) -> Result<SubAgentId, String>
- retry_sub_agent(agent_id: &str) -> Result<(), String>
- close_sub_agent(agent_id: &str) -> Result<(), String>
- cancel_children(parent_id: &str)
- pending_results(parent_id: &str) -> Vec<SubAgentResult>
- merge_results(parent_id: &str, main_reply: String) -> String

### 2. src/agent_runtime.rs

从 src/runner.rs::run_turn 抽出通用主循环。

职责：

- 会话初始化。
- 系统提示词注入。
- Completion 请求。
- 流式 delta。
- parser 解析。
- 工具调用。
- 交接报告。
- 消息持久化。
- 取消处理。
- 子 Agent 和主 Agent 共用。

建议抽象：

- AgentRunSpec
  - agent_id
  - project_id
  - account
  - role
  - model
  - session_id
  - tool_allowlist
  - system_prompt
  - cancel
  - parent_agent_id
- AgentRunResult
  - final_text
  - tool_count
  - session_id
  - message_id
  - status

建议接口：

- run_agent(spec: AgentRunSpec, input: String, hooks: AgentRunHooks) -> Result<AgentRunResult, String>
- run_turn(...) 保留为主 Agent 适配器，内部构造 AgentRunSpec 后调用 run_agent。
- 子 Agent 由 SubAgentManager 构造同样的 AgentRunSpec。

### 3. src/state.rs

现有：

- Inner.running: HashMap<String, RunCtx>
- Inner.bridges: HashMap<String, Arc<BridgeModel>>
- account_locks
- init_locks
- bridge_last_used

建议调整为：

- running: HashMap<String, AgentGroup>
- AgentGroup
  - 主 Agent MainRunCtx
  - 子 Agent HashMap<SubAgentId, SubAgentRecord>
  - group_cancel
  - busy_count
  - pending_results
  - finalizing
- bridges
  - 从账号唯一 bridge 改为 (account, bridge_id) 索引。
- bridge_profiles
  - bridge 对应 profile 路径、来源版本、使用状态。
- account_limiters
  - 每账号限流器。
- profile_locks
  - master profile 快照/复制/回收锁。

建议保留 RunCtx 的会话字段，避免一次性大改；可以先在 AgentGroup 中嵌入旧 RunCtx，逐步迁移。

### 4. src/bridge_pool.rs

职责：

- master profile 快照。
- 子 Agent profile 克隆。
- bridge 进程启动、复用、关闭。
- profile 生命周期和清理。
- 子 Agent 与 bridge 绑定。

建议接口：

- acquire_main_bridge(account: &str) -> Result<BridgeHandle, String>
- acquire_sub_bridge(account: &str, agent_id: &str) -> Result<BridgeHandle, String>
- release_bridge(bridge_id: &str)
- close_idle_bridges()
- snapshot_master_profile(account: &str) -> Result<ProfileSnapshot, String>
- clone_profile(snapshot: &ProfileSnapshot, agent_id: &str) -> Result<PathBuf, String>

### 5. src/rate_limit.rs

替代 src/runner.rs 中的全局 GLOBAL_REQUEST_GAP 和 AppState.last_request_at。

职责：

- 账号级最小请求间隔。
- bridge 级并发计数。
- 主 Agent/子 Agent 权重。
- 随机错峰。
- 失败后的退避。

### 6. src/commands.rs

新增命令：

- list_sub_agents(project_id)
- close_sub_agent(project_id, agent_id)
- retry_sub_agent(project_id, agent_id)
- interrupt(project_id) 改为级联取消主 Agent 和全部子 Agent。
- 可增加 sub_agent_status(project_id)，用于前端初始同步。

现有 send_message 仍只启动主 Agent。

### 7. vendor/deepseek-agent/src/agent/parser.rs

KNOWN_TOOLS 是唯一工具白名单，新增控制工具必须登记。

建议新增：

- spawn_sub_agent
- close_sub_agent
- retry_sub_agent
- 可选 await_sub_agents

这些工具不是直接执行 browser 操作，而是由主 Agent 请求后交给后端编排层执行。

### 8. src/tools.rs 与 src/tools/

新增控制工具适配器：

- SpawnSubAgentTool
- CloseSubAgentTool
- RetrySubAgentTool

必须让工具执行上下文携带：

- 当前 agent_id
- 当前角色
- 允许的最大递归深度
- 父 Agent ID
- 当前项目 ID

### 9. src/prompt_gui.rs

拆分为：

- MAIN_SYSTEM_PROMPT
- SUB_AGENT_SYSTEM_PROMPT
- MAIN_TOOLS_SECTION
- SUB_AGENT_TOOLS_SECTION
- SUB_AGENT_REQUEST_RULES

主 Agent 提示词删除 browser 工具；子 Agent 提示词按专职职责只暴露必要工具。

### 10. 前端

修改：

- ui/app.js
- ui/ide/agent/events.js
- ui/ide/agent/store.js
- ui/ide/agent/render.js

新增显示：

- 子 Agent 名称。
- 职责。
- 当前状态。
- 当前重试次数。
- 当前项目子 Agent 数量。
- 是否仍有运行中的子 Agent。
- 子 Agent 结果已完成但尚未合并的状态。

———

## B. 单账号多 bridge 的 profile 复制方案

### 1. 目录布局

保留现有 master profile：

~/.deepseek-agent/accounts/<account>/browser/

新增：

~/.deepseek-agent/accounts/<account>/sub-agents/
  <agent_id>/
    browser/
    metadata.json

建议 bridge 运行目录与 profile 分离：

~/.deepseek-agent/runtime/bridges/<bridge_id>/
  metadata.json
  bridge.log

metadata.json 至少记录：

- account
- agent_id
- profile_source_version
- created_at
- last_used_at
- state
- pid

### 2. 复制策略

master profile 由主 Agent 使用，子 Agent 使用独立副本。

复制前必须：

1. 暂停 master profile 的写入。
2. 确认 master bridge 当前没有 ask() 请求。
3. 确认 Chromium 已刷新 SQLite WAL。
4. 复制到临时目录。
5. 对临时目录执行一致性检查。
6. 原子 rename 为正式子 Agent profile。

建议优先使用：

- 同文件系统的目录复制。
- 大文件先复制，再复制 SQLite/WAL 等小文件。
- 必要时使用 reflink/Copy-on-Write。
- 不建议直接使用硬链接，因为 Chromium 多进程写入会互相污染。

### 3. 复制内容

通常需要复制：

- Cookies。
- Local Storage。
- IndexedDB。
- 登录状态。
- Chromium Preferences。
- DeepSeek 站点相关缓存和授权信息。

建议默认排除：

- SingletonLock
- SingletonSocket
- SingletonCookie
- DevToolsActivePort
- 临时缓存目录。
- Crashpad 运行状态。
- 进程锁文件。
- 与窗口/设备实例强绑定的临时文件。

可选排除：

- Cache
- Code Cache
- GPUCache
- Service Worker/CacheStorage

是否排除缓存需要实验确认；过度排除可能导致重新登录或行为差异。

### 4. 生命周期

#### 创建

- 主 Agent 申请一个有明确名称和职责的子 Agent。
- 后端分配 agent_id。
- 获取账号 profile 锁。
- 创建 profile 快照。
- 复制 profile。
- 使用子 profile 启动独立 BridgeModel。
- 创建独立 session。
- 状态从 Created -> Starting -> Running。

#### 复用

不建议复用已关闭的子 Agent profile 作为不同职责的新 Agent。

可以复用同一个子 Agent 在最多 3 次重试中的 bridge 和 session，前提是：

- 失败原因属于模型输出不完整、工具格式错误等可恢复错误。
- bridge 进程和 profile 状态正常。
- 不需要重置上下文。

如果失败疑似页面状态异常，应关闭原 bridge，重新创建 profile/bridge。

#### 同步

不建议运行中把子 Agent profile 的文件反向同步回 master profile。

原因：

- Chromium SQLite/WAL 可能不一致。
- Cookie、localStorage、session 状态可能互相覆盖。
- 多个 profile 之间的服务器会话状态没有可安全合并语义。

仅允许同步：

- 子 Agent 的工作结果。
- 明确指定的文件产物。
- 结构化任务报告。

#### 回收

子 Agent 进入 Succeeded / Failed / Cancelled / Closed 后：

1. 等待其 bridge 请求完全退出。
2. 调用 BridgeModel::shutdown()。
3. 等待 Node/Chromium 进程组退出。
4. 清理子 profile。
5. 保留 metadata.json 和摘要日志一段时间，便于诊断。
6. 按 idle 时间延迟删除，避免结果尚未合并时丢失诊断信息。

### 5. 并发安全

现有 BridgeModel::ask() 对 stdin/stdout 使用双 Mutex，单个 bridge 严格串行；bridge_server.mjs 也通过 enqueue 串行使用同一个 page。

因此：

- 一个 bridge 只能服务一个正在执行的 completion。
- 主 Agent和子 Agent必须使用不同 bridge。
- Inner.account_locks 不能再覆盖整个 Agent turn，否则会把多 bridge 又串行化。
- 账号锁只用于：
  - profile 快照。
  - profile clone。
  - master profile 更新。
  - bridge 初始化。
- 请求执行阶段改用 bridge 级锁和账号限流器。

### 6. 可行性风险

必须先验证：

1. 同一 DeepSeek 账号是否允许多个 Chromium profile 同时完成请求。
2. 多个页面是否会触发账号风控、验证码、登录失效。
3. profile 复制时 SQLite/WAL 是否一致。
4. 复制后的 Cookies/IndexedDB 是否仍能直接使用。
5. state.json cookie 注入是否会覆盖或破坏副本状态。
6. 子 bridge 同时访问网页时，服务器是否按账号而非 profile 限制并发。
7. 两个 bridge 是否会出现同一账号会话互相踢出。
8. Chromium 多实例资源占用是否可接受。
9. BridgeModel::shutdown() 是否能彻底回收 Node 和 Chromium 子进程。
10. Windows 下 process group 行为是否与 Unix 一致。

### 7. 资源优化建议

默认策略：

- 每个账号最多 2 个 bridge：主 Agent 1 个、子 Agent 1 个。
- 默认最多一个子 Agent 并行。
- 子 Agent 完成后尽快关闭 bridge。
- 空闲 bridge 不常驻。
- 同时使用 CoW/reflink 时减少复制成本。
- 只在主 Agent明确需要并行时创建子 bridge。

这符合“允许一主一子并行”，也避免无限制多 bridge。

———

## C. 抽出可复用 Agent 主循环

现有 run_turn 既负责生命周期，又负责主循环，建议分成三层。

### 1. 生命周期层

仍由主 Agent适配器负责：

- 获取项目。
- 设置 RunCtx.busy。
- 创建 BusyGuard。
- 创建 DoneGuard。
- 创建 IdleGuard。
- 绑定主取消令牌。
- 调用通用运行时。
- 清理 busy/cancel。
- 发出最终 agent://done 或 agent://error。

### 2. 通用循环层

从 run_turn 提取以下逻辑：

- session 恢复和创建。
- 系统提示词拼接。
- 工作目录通知。
- 交接报告。
- CompletionReq 构造。
- thinking/search 设置。
- delta 缓冲和 AnswerStreamGate。
- model.complete()。
- parser 处理。
- 工具调用。
- parser malformed / DSML malformed 处理。
- message_count 更新。
- assistant/tool 消息持久化。
- parent message 更新。

建议通用循环不直接假设 project_id 是唯一 Agent 标识，而是使用：

- project_id
- agent_id
- parent_agent_id
- role

### 3. 事件层

AgentRunHooks 负责：

- on_delta
- on_tool_call
- on_tool_done
- on_error
- on_handoff
- on_retry
- on_finished

主 Agent hooks 继续发送现有事件格式。

子 Agent hooks 发送同一事件名称，但额外携带：

- agent_id
- agent_name
- agent_role
- parent_agent_id

这样可与现有 project_id 过滤逻辑兼容。

### 4. BusyGuard / DoneGuard 处理

不建议让子 Agent直接复用现有 BusyGuard 的语义，因为现有 BusyGuard 会对项目发送 busy=true/false，子 Agent提前结束可能误报主 Agent已完成。

建议：

- 主 Agent保留原 BusyGuard。
- 子 Agent使用 ChildBusyGuard，只发送子 Agent 状态事件。
- 项目级 busy=false 只能由主 Agent最终结束时发出。
- DoneGuard 只由主 Agent创建。
- 子 Agent使用 SubAgentDoneHook，不能发送项目级 agent://done。

### 5. 交接报告与持久化

主 Agent的：

- REPORT_PROMPT
- HANDOFF_INTRO
- 新 session 创建
- projects.add_conversation
- append_message
- append_tool
- refresh_message_count

保持不变，由主 Agent模式继续执行。

子 Agent建议：

- 使用独立 session。
- 不默认创建项目可见 conversation。
- 结果写入内存 SubAgentResult。
- 必要时将报告作为主 Agent消息中的结构化片段持久化。
- 子 Agent工具调用可单独写入调试日志或 sub_agent_runs/<agent_id>.jsonl。

———

## D. 子 Agent 数据结构、状态机和事件协议

### 1. 命名要求

创建请求必须包含：

- name
- duty
- task

名称不得使用默认值如 child-1，必须体现职责，例如：

- 网页资料检索
- 浏览器登录检查
- 接口文档核对
- 测试失败分析

后端拒绝：

- 空名称。
- 仅数字名称。
- 与职责不匹配的泛化名称。
- 重复名称。

### 2. 状态机

Created
  -> Starting
  -> Running
  -> Succeeded
  -> Closed

Running
  -> WaitingRetry
  -> Cancelled
  -> Failed

WaitingRetry
  -> Starting
  -> Failed       （达到 3 次）
  -> Cancelled

规则：

- 只有主 Agent可以触发 retry。
- 子 Agent不能自行重试。
- 最多 3 次。
- “空回复、格式错误、工具调用不完整、明确运行错误”都只产生失败结果，由主 Agent决定是否重试。
- 底层 bridge 错误可以让本次执行失败，但不能自动重新执行任务。
- 达到 3 次后，子 Agent终态为 Failed，主 Agent直接给用户解决方案。

### 3. 关闭机制

主 Agent判断子 Agent不再需要时调用 close_sub_agent。

关闭操作：

1. 标记 Closing 或直接进入 Cancelled。
2. 触发子 Agent cancellation token。
3. 中断当前 completion。
4. 关闭 child bridge。
5. 回收 profile。
6. 保留最终状态和已产生的部分结果。

### 4. 事件协议

继续使用现有：

- agent://delta
- agent://tool
- agent://error
- agent://approval
- agent://project-busy
- agent://done
- agent://retry

新增：

- agent://sub-agent/created
- agent://sub-agent/status
- agent://sub-agent/result
- agent://sub-agent/closed
- agent://sub-agent/merged

事件必须携带：

project_id
agent_id
agent_name
agent_role
parent_agent_id
status
running_sub_agent_count
has_running_sub_agents

agent://delta 和 agent://tool 对子 Agent也增加上述字段，但仍保留 project_id，保证现有前端过滤逻辑不冲突。

### 5. 每次回复附带子 Agent状态

主 Agent最终消息末尾追加结构化状态摘要：

[子Agent状态]
当前项目子Agent数：N
运行中：是/否

更推荐由后端生成统一 metadata，再由前端渲染，避免依赖模型自行遵守。

建议主回复 payload 增加：

- sub_agent_count
- sub_agent_running
- sub_agent_summaries

———

## E. 并行结果拼接机制

采用“结果暂存队列 + 主 Agent完成屏障”。

### 1. 暂存结构

AgentGroup.pending_results 按完成顺序或创建顺序保存：

- 子 Agent ID。
- 名称。
- 职责。
- 状态。
- 摘要。
- 产物。
- 错误。
- 完成时间。

### 2. 子 Agent先完成

子 Agent完成时：

1. 发送 agent://sub-agent/result 状态事件。
2. 不发送独立用户最终消息。
3. 把结果放入 pending_results。
4. 主 Agent继续执行。
5. 主 Agent最终回复前读取并排序结果。

### 3. 主 Agent先完成

主 Agent产生最终文本后，不立即执行 DoneGuard 的最终结束逻辑，而是：

1. 将主回复暂存为 pending_main_reply。
2. 等待所有子 Agent进入终态。
3. 对仍运行的子 Agent执行受限等待。
4. 子 Agent返回后合并。
5. 最终一次性发送主 Agent消息和子 Agent结果。

建议等待策略：

- 默认等待所有子 Agent结束。
- 设置最大等待时间。
- 超时后将未完成子 Agent标记为 pending，并在主回复中明确说明。
- 不能让无限等待阻塞项目。

### 4. 合并顺序

建议：

1. 主 Agent最终结论。
2. 子 Agent结果，按创建顺序。
3. 失败和未完成项。
4. 子 Agent数量和运行状态。

合并内容应由后端拼接模板完成，避免模型二次改写导致信息丢失。

### 5. 主 Agent继续工作的情况

主 Agent可以在子 Agent运行时继续完成代码编辑、分析和测试。

但主 Agent最终结束时必须经过 join_children 阶段，确保满足“子 Agent结果不单独发送”。

———

## F. 限流设计

现有 GLOBAL_REQUEST_GAP 和 AppState.last_request_at 是全局粒度，会导致不同账号、不同 bridge 之间不必要串行。

### 1. 建议粒度

分三层：

#### 账号级

每个账号维护：

- 最近一次请求时间。
- 当前请求数。
- 失败次数。
- 最近一次风控信号。

主 Agent和子 Agent共享账号级限流。

#### bridge 级

每个 bridge 保证：

- 同时最多一个 completion。
- 由 BridgeModel 自身 stdin/stdout 锁和运行状态保证。

#### 全局级

仅保留一个较宽松的全局并发上限，防止机器资源耗尽，不再使用固定 10 秒全局间隔。

### 2. 建议策略

- 主 Agent权重高于子 Agent。
- 子 Agent使用错峰启动。
- 首次请求沿用 SEND_DELAY_BASE + jitter。
- 同账号请求间隔由账号 limiter 决定。
- 失败或页面异常时指数退避。
- 账号检测到验证码、登录失效、429 时暂停该账号全部 bridge。
- 不同账号可以并发，前提是资源上限允许。

### 3. 与现有代码衔接

替换 src/runner.rs 中以下调用：

- state.throttle(GLOBAL_REQUEST_GAP, &cancel).await

改为：

- state.account_rate_limiter(account).acquire(role, cancel).await

原有 account_lock 只保留 profile 操作和 bridge 初始化，不能包住整个 run_turn。

———

## G. 工具白名单、递归深度、取消级联

### 1. 主 Agent工具

主 Agent移除：

- browser

主 Agent保留：

- execute_command
- read_file
- write_file
- edit_file
- upload_file
- spawn_sub_agent
- close_sub_agent
- retry_sub_agent
- 可选 await_sub_agents

src/prompt_gui.rs、execute_tool()、KNOWN_TOOLS、格式错误提示必须保持一致。

### 2. 子 Agent工具

按职责最小授权：

#### 浏览器专职子 Agent

- browser
- upload_file，如确有需要
- read_file，如需读取任务材料

#### 代码分析子 Agent

- read_file
- execute_command

#### 测试子 Agent

- read_file
- execute_command

#### 文件修改子 Agent

- read_file
- write_file
- edit_file
- 默认禁止 browser

子 Agent不能使用：

- spawn_sub_agent
- close_sub_agent
- retry_sub_agent

避免递归扩散。

### 3. 递归深度

建议：

- 主 Agent深度为 0。
- 子 Agent深度为 1。
- max_depth = 1。
- 任何 depth >= 1 的 Agent都不能创建新子 Agent。

### 4. 工具执行校验

后端必须二次校验工具白名单，不能只依赖提示词。

解析成功后仍需检查：

- 当前 Agent角色。
- 工具是否允许。
- 是否满足参数要求。
- 是否超出调用次数。
- 是否越权操作其他子 Agent。

### 5. 取消级联

主 interrupt 流程：

1. 取消主 Agent token。
2. 遍历 AgentGroup.children。
3. 取消每个子 Agent token。
4. 向对应 bridge 发送 stop。
5. 等待 completion 返回或超时。
6. 关闭 child bridge。
7. 回收 child profile。
8. 统一发出项目级完成/取消事件。

子 Agent关闭不会取消主 Agent。

主 Agent取消后，不再接受新的子 Agent创建请求。

———

## H. 提示词改动要点

### 1. 主 Agent提示词

在 src/prompt_gui.rs 的 SYSTEM_PROMPT_GUI 中：

- 删除 browser 工具说明。
- 明确浏览器操作必须委托给专职子 Agent。
- 增加“申请子 Agent”规则。
- 子 Agent必须有职责化名称。
- 说明主 Agent负责：
  - 总体计划。
  - 任务拆解。
  - 子 Agent调度。
  - 结果校验。
  - 重试决策。
  - 最终回复。
- 说明子 Agent失败时：
  - 主 Agent判断是否重试。
  - 最多 3 次。
  - 不得自动无限重试。
- 说明子 Agent结果暂存，不单独发送给用户。
- 最终回复附带子 Agent数量和运行状态。

建议使用极短结构化规则，而非长篇解释：

浏览器任务必须委托给命名且职责明确的子Agent。
你负责计划、调度、校验和最终答复。
子Agent失败由你决定是否重试，最多3次。
子Agent结果由系统暂存并在你的最终答复中合并。

### 2. 子 Agent提示词

子 Agent提示词独立生成，包含：

- 名称。
- 专职职责。
- 当前任务。
- 允许工具。
- 禁止创建子 Agent。
- 输出格式。
- 结果必须包含：
  - 已完成内容。
  - 证据。
  - 未完成内容。
  - 错误。
  - 建议下一步。

浏览器子 Agent应明确：

- 只执行网页操作。
- 不承担总体计划。
- 不修改无关文件。
- 不输出面向用户的最终长篇答复。

### 3. Prompt 与后端约束一致

提示词只能辅助模型，真正限制必须在：

- parser 白名单。
- execute_tool。
- SubAgentManager。
- depth 校验。
- 取消状态校验。

———

## I. 落地步骤与风险清单

### 阶段 1：先做运行模型重构

1. 抽出 AgentRunSpec、AgentRunHooks。
2. 将 run_turn 改为主 Agent适配器。
3. 保持现有主 Agent行为不变。
4. 确保 BusyGuard、DoneGuard、交接报告、持久化逻辑测试通过。

### 阶段 2：增加 Agent 编排状态

1. 增加 AgentGroup。
2. 增加 SubAgentRecord。
3. 增加状态机和结果暂存。
4. 增加主取消级联。
5. 增加子 Agent事件。

### 阶段 3：增加工具与 parser 支持

1. 扩展 KNOWN_TOOLS。
2. 增加子 Agent控制工具。
3. 主 Agent工具白名单移除 browser。
4. 子 Agent工具白名单后端校验。
5. 增加深度限制。

### 阶段 4：实现 bridge pool

1. 将 Inner.bridges 从账号唯一改为 bridge ID。
2. 实现 profile 快照。
3. 实现子 profile 克隆。
4. 实现独立 BridgeModel::launch。
5. 实现 bridge 回收。
6. 保留主 bridge 与子 bridge 的独立 stdin/stdout 锁。

### 阶段 5：限流改造

1. 移除固定全局 gap。
2. 增加账号级 limiter。
3. 增加 bridge 级并发保护。
4. 增加随机错峰和退避。
5. 增加验证码/429暂停账号机制。

### 阶段 6：结果汇总

1. 增加 pending result queue。
2. 增加主 Agent完成屏障。
3. 增加超时策略。
4. 增加统一最终消息拼接。
5. 增加子 Agent状态摘要。

### 阶段 7：前端适配

1. 监听子 Agent事件。
2. 显示名称、职责、状态、重试次数。
3. 显示项目子 Agent数量和运行状态。
4. 子 Agent结果只显示为主消息的一部分。
5. 保留现有 project_id 过滤逻辑。

### 主要风险

- 多 Chromium 实例显著增加内存和 CPU。
- 同账号多 profile 可能触发 DeepSeek 风控。
- profile 复制期间 SQLite/WAL 可能不一致。
- Cookies 复制成功不代表网页 session 可并发使用。
- 账号级锁调整错误会重新导致所有 Agent 串行。
- 子 Agent事件若误发 agent://done，会破坏现有 UI busy 状态。
- 主 Agent在等待子 Agent时可能长时间占用项目状态。
- 子 Agent结果若没有持久化，应用崩溃后可能丢失。
- bridge shutdown 失败可能留下 Chromium 孤儿进程。
- 当前 browser 工具实际位于 vendor/deepseek-agent/src/agent/browser.rs，其 profile/key 管理方式与 BridgeModel profile 体系可能不是同一套，需要适配。

———

## J. 需人工确认或先实验验证的假设

以下项目不能仅凭当前源码确认，必须先做实验：

1. 同一 DeepSeek 账号是否允许两个独立 persistent profile 同时请求。
2. 同账号多 bridge 是否会触发验证码、限流、登录失效或账号踢出。
3. master profile 在 bridge 运行期间是否能够安全复制。
4. Chromium SQLite/WAL 在复制前是否必须完整关闭。
5. 哪些 profile 子目录必须保留才能维持登录状态。
6. 哪些缓存目录可以排除而不导致重新登录。
7. state.json cookie 注入与副本自带 Cookies 同时存在时的优先级。
8. 子 profile 是否需要独立 state.json。
9. bridge_server.mjs 的 captured、page、SSE 监听逻辑在多实例下是否稳定。
10. BridgeModel::shutdown() 是否能在各平台彻底回收 Chromium。
11. Windows 下 process_group 和进程树清理是否可靠。
12. 现有 Browser::with_key(project_id) 是否能直接改为子 Agent 级 key。
13. browser MCP 与 DeepSeek bridge 是否需要分别维护 profile。
14. 是否允许子 Agent直接操作项目文件，还是必须通过主 Agent提交变更。
15. 子 Agent结果应只存内存、写 jsonl，还是写入项目会话历史。
16. 主 Agent完成后最多等待子 Agent多久。
17. 子 Agent失败是否允许更换职责或只能原任务重试。
18. 账号级请求间隔和最大并发的具体数值。
19. 是否允许同时存在多个子 Agent；当前建议默认最多一个，以满足一主一子并行。
20. “每次回复附带子 Agent数量和运行状态”是由后端自动追加，还是由前端 UI 独立显示。
21. 子 Agent产生的浏览器截图、下载文件、网页证据是否需要纳入最终回复。
22. 是否需要支持应用重启后恢复运行中的子 Agent。
23. 是否需要在项目关闭、账号切换、窗口退出时自动关闭所有子 Agent。
24. 是否接受将 profile 克隆限定为“主 bridge 空闲时执行”；否则需要更复杂的 Chromium 级快照方案。
25. 如果多 bridge 的资源占用或风控不可接受，是否允许退化为“单 bridge 串行子 Agent + 后端任务并发”的兼容模式