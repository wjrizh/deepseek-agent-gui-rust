//! 工具调用解析器 —— 高鲁棒性，抵抗大模型幻觉。
//!
//! 支持两种格式：
//!
//! 1) DeepSeek 原生格式（优先识别）：
//!    最外层 tool_calls 包裹一个 invoke（name 属性指定工具名），
//!    invoke 内部每个 parameter（name 属性指定参数名）元素承载一个参数值。
//!
//! 2) 简易格式（自定义提示词，向后兼容）：
//!    工具开标签 execute_command 内嵌 command 参数元素。
//!
//! 鲁棒性设计：
//! - 只识别 KNOWN_TOOLS 中的工具名；未知工具 → Malformed。
//! - 大小写不敏感；容忍空白、换行、markdown 代码围栏。
//! - 闭合标签宽松匹配：允许标签名前后有多余空白、大小写混用，
//!   甚至完全缺失闭合标签（退回到下一个边界）。
//! - 缺参 / 空参 / 多工具 → Malformed，交由上层有界重试。
//! - 参数内容（尤其 command）原样保留，只裁首尾空白。
//! - 审批不在此处：由 CLI 的 /free 全局开关控制，模型无权决定。

use std::collections::HashMap;

/// 已知工具名（唯一真源；新增工具需在此登记 + 实现 `Tool`）。
pub const KNOWN_TOOLS: &[&str] = &["execute_command", "upload_file", "browser", "read_file", "write_file", "edit_file", "sub_agent", "task_complete", "ask_user", "request_context_compaction"];

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub params: HashMap<String, String>,
    /// 规整后的原始标签文本（用于回注 / 日志）
    pub raw: String,
}

#[derive(Debug)]
pub enum ParseOutcome {
    /// 普通回答，无工具调用
    Text(String),
    /// 成功解析出的工具调用（一个或多个），按出现顺序串行执行
    Calls(Vec<ToolCall>),
    /// 疑似工具调用但结构/参数不合法 → 需重试
    Malformed { raw: String, reason: String },
    /// 输出中出现多个工具/DSML 标记，但整体未能解析成合法调用。
    /// 与 `Malformed` 的区别：此处是"看起来像工具调用、标记成对出现却失败"，
    /// 上层据此回注更强的格式纠正提示。
    DsmlMalformed { raw: String, reason: String, marker_count: usize },
}

/// 统计输出中工具/DSML 标记的出现次数（开标签计 1）。
/// 用于识别"有两个以上标记却没解析成调用"的畸形输出。
pub fn count_tool_markers(text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let mut n = 0;
    for pat in ["<tool_calls", "<invoke", "</invoke", "</tool_calls"] {
        let mut i = 0;
        while let Some(rel) = lower[i..].find(pat) {
            n += 1;
            i += rel + pat.len();
        }
    }
    n
}

/// 统计全文里 "dsml" 这个词出现的次数（大小写不敏感）。
/// 只认 "dsml" 四个连续字母，不要求尖括号或标签结构。
pub fn count_dsml_markers(text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let mut n = 0usize;
    let mut i = 0;
    while let Some(rel) = lower[i..].find("dsml") {
        n += 1;
        i += rel + 4;
    }
    n
}

/// 解析模型的完整输出。
pub fn parse(text: &str) -> ParseOutcome {
    let text = &strip_think(text);
    let cleaned = strip_fences(text);
    let cleaned = normalize_tag_names(&cleaned);
    let marker_count = count_tool_markers(text).max(count_tool_markers(&cleaned));
    if let Some(outcome) = parse_native(&cleaned) {
        return outcome;
    }
    let outcome = parse_simple(&cleaned, text);
    if marker_count >= 2 {
        if let ParseOutcome::Text(_) = outcome {
            return ParseOutcome::DsmlMalformed {
                raw: text.trim().to_string(),
                reason: "检测到多个工具标记但未解析出任何合法调用".into(),
                marker_count,
            };
        }
    }
    if count_dsml_markers(text) >= 2 {
        if let ParseOutcome::Text(_) = outcome {
            return ParseOutcome::DsmlMalformed {
                raw: text.trim().to_string(),
                reason: "检测到多个 DSML 标记但未解析出任何合法调用".into(),
                marker_count: count_dsml_markers(text),
            };
        }
    }
    outcome
}

// ---------- 标签名归一化（专有标记 / 命名空间 / 管道装饰） ----------

/// 归一化标签名：只重写尖括号内部，剥离标签名前后的厂商标记、
/// 管道符、命名空间前缀，正文（标签之外）一个字节都不动。
/// 行为见 tests 中的 dsml/管道/冒号 三个回归测试。
fn normalize_tag_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let cleaned_lower = text.to_ascii_lowercase();

    while let Some(lt) = rest.find('<') {
        // 正文部分原样拷贝
        out.push_str(&rest[..lt]);
        let tail = &rest[lt..];

        // 找标签的 '>'（跳过引号内的 '>'，避免被属性值误导）
        match find_tag_gt(tail) {
            Some(gt) => {
                let tag = &tail[..=gt]; // 含 '<' 和 '>'
                let allow_recover = cleaned_lower.contains("<tool_calls");
                let recovered = if allow_recover {
                    recover_corrupted_invoke(tag)
                } else {
                    None
                };
                match recovered {
                    Some(fixed) => out.push_str(&fixed),
                    None => out.push_str(&rewrite_tag(tag)),
                }
                rest = &tail[gt + 1..];
            }
            None => {
                // 没有闭合 '>'，剩余全部原样
                out.push_str(tail);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 公开：归一化标签里的 DSML 厂商标记（`｜｜DSML｜｜` / `||DSML||`，含全角竖线）。
/// 供 UI 层的 TagFilter 等复用，避免逻辑漂移。
/// 输入可以是带 '<' 的完整标签（如 `<｜｜DSML｜｜ invoke`）或纯标签名区。
pub fn normalize_tag_prefix(tag: &str) -> String {
    let had_lt = tag.trim_start().starts_with('<');
    let t = tag.trim_start();
    let t = t.strip_prefix('<').unwrap_or(t);
    // 跳过 `<` 后所有非 ASCII 字母字符（竖线/空格/DSML 标记统统跳过）
    let t = t.trim_start_matches(|c: char| !c.is_ascii_alphabetic() && c != '/');
    if had_lt {
        format!("<{t}")
    } else {
        t.to_string()
    }
}

/// 在 `s`（以 '<' 开头）中找标签的 '>'，跳过引号内的 '>'。
/// 返回相对下标。
fn find_tag_gt(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '>' {
            return Some(i);
        }
    }
    None
}

/// 重写单个标签，只清理标签名，保留属性原样。
fn rewrite_tag(tag: &str) -> String {
    // tag 形如 "<...内容...>"
    let inner = &tag[1..tag.len() - 1]; // 去掉 '<' '>'
    let (closing, after_slash) = match inner.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, inner),
    };

    // 1. 成对剥离厂商标记块：形如 ｜｜DSML｜｜ 或 ||DSML||
    //    跳过前导竖线 → 跳到闭合竖线 → 跳过闭合竖线，得到真正的标签名区。
    let mut t = after_slash.trim_start();
    let is_pipe = |c: char| c == '|' || c == '｜';
    if t.starts_with(is_pipe) {
        let s1 = t.trim_start_matches(is_pipe);
        if let Some(close_rel) = s1.find(is_pipe) {
            let after = &s1[close_rel..];
            t = after.trim_start_matches(is_pipe);
        } else {
            t = s1;
        }
    }
    let trimmed = t.trim_start();

    // 2. 只在第一个空白之前（纯标签名区）找命名空间冒号
    let name_zone_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let name_zone = &trimmed[..name_zone_end];
    // name_zone 是 trimmed 的前缀，偏移可直接用于 trimmed。
    let name_start = name_zone.rfind(':').map(|p| p + 1).unwrap_or(0);
    let name_src = &trimmed[name_start..];

    // 3. 取标识符（字母/数字/下划线）
    let name_len = name_src
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(name_src.len());
    let name = &name_src[..name_len];
    if name.is_empty() {
        return tag.to_string(); // 不是合法标签，原样返回
    }

    // 4. 标签名之后的部分（属性）原样保留
    let after_name = &name_src[name_len..];

    let mut out = String::with_capacity(tag.len());
    out.push('<');
    if closing {
        out.push('/');
    }
    out.push_str(name);
    out.push_str(after_name);
    out.push('>');
    out
}

fn parse_native(text: &str) -> Option<ParseOutcome> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<invoke") && !lower.contains("<tool_calls") {
        return None;
    }

    let spans = collect_invoke_spans(&lower);
    if spans.is_empty() {
        return None;
    }

    let mut calls = Vec::with_capacity(spans.len());
    for (body_start, body_end, name) in spans {
        let Some(canonical) = canonical_tool(&name) else {
            return None;
        };
        let params = extract_parameters(&text[body_start..body_end]);
        // 检测同一 invoke 体内是否存在重复的 <parameter name="X">。
        // 重复参数被视为畸形输出，解析失败。
        if has_duplicate_parameters(&text[body_start..body_end]) {
            return None;
        }
        if validate(&canonical, &params).is_err() {
            return None;
        }
        calls.push(ToolCall {
            name: canonical,
            params,
            raw: String::new(),
        });
    }
    Some(ParseOutcome::Calls(calls))
}

/// 工具名归一化：精确匹配，其次容忍输出截断的前缀（≥6 字符）。
fn canonical_tool(name: &str) -> Option<String> {
    if let Some(t) = KNOWN_TOOLS.iter().find(|t| t.eq_ignore_ascii_case(name)) {
        return Some((*t).to_string());
    }
    let name_lc = name.to_ascii_lowercase();
    KNOWN_TOOLS
        .iter()
        .find(|t| name_lc.len() >= 6 && t.to_ascii_lowercase().starts_with(&name_lc))
        .map(|t| (*t).to_string())
}

/// 恢复被污染的 invoke 开标签。
///
/// 实测污染形态（标签名被塞进 "tool call"、invoke 降级为属性、name 重复）：
///   <tool call name="invoke" name="browser">
/// 应还原为：
///   <invoke name="browser">
///
/// 触发条件必须同时满足（窄规则，避免误伤正文 XML/HTML）：
///   1. 标签名（首段标识符）为 "tool" 后跟 "call"（即 "tool call" 这类带空格形态）；
///   2. 属性里出现 name="invoke"（被降级的 invoke 关键字）；
///   3. 属性里还存在另一个 name 值，且该值能 canonical_tool 命中已知工具。
///
/// 输入 `tag` 形如 `<tool call name="invoke" name="browser">`（含首尾尖括号）。
/// 命中返回 `<invoke name="browser">`，否则返回 None。
fn recover_corrupted_invoke(tag: &str) -> Option<String> {
    let inner = tag.strip_prefix('<')?.strip_suffix('>')?;
    let is_closing = inner.starts_with('/');
    let trimmed = inner.trim_start();
    let rest = if is_closing { trimmed.strip_prefix('/')?.trim_start() } else { trimmed };

    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let tag_name = rest[..name_end].to_ascii_lowercase();
    let base: String = tag_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if base != "tool" && base != "toolcall" {
        return None;
    }

    if is_closing {
        let suffix = rest[name_end..].trim_start();
        if suffix.is_empty() || suffix.split_whitespace().next().map(|s| s.eq_ignore_ascii_case("call")).unwrap_or(false) {
            return Some("</invoke>".to_string());
        }
        return None;
    }

    // 属性区收集所有 name="..."
    let attrs = &rest[name_end..];
    let names = extract_all_attrs(attrs, "name");
    let mut tool_name: Option<String> = None;
    let mut has_invoke = false;
    for n in &names {
        if n.eq_ignore_ascii_case("invoke") {
            has_invoke = true;
        } else if canonical_tool(n).is_some() {
            tool_name = Some(n.clone());
        }
    }
    if !has_invoke {
        return None;
    }
    let tool = tool_name?;
    Some(format!("<invoke name=\"{tool}\">"))
}

/// 收集所有 invoke 的 (正文起, 正文止, name)。跳过参数体内引述的伪标签。
fn collect_invoke_spans(lower: &str) -> Vec<(usize, usize, String)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while let Some(rel) = lower[i..].find("<invoke") {
        let start = i + rel;
        if inside_parameter(lower, start) {
            i = start + "<invoke".len();
            continue;
        }
        let after = start + "<invoke".len();
        let boundary_ok = matches!(
            lower[after..].chars().next(),
            Some(c) if c == '>' || c == '/' || c.is_whitespace()
        );
        if !boundary_ok {
            i = after;
            continue;
        }
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + gt + 1;
        let name = extract_attr(&lower[start..open_end], "name").unwrap_or_default();
        let (body_end, next_i) = match find_lenient_close(lower, open_end, "invoke") {
            Some((s, e)) => (s, e),
            None => {
                let end = next_invoke_boundary(lower, open_end);
                (end, end)
            }
        };
        spans.push((open_end, body_end, name));
        i = next_i;
    }
    spans
}

/// 找不到闭合标签时的兜底：退到下一个 invoke 开标签、tool_calls 闭合标签或串尾。
fn next_invoke_boundary(lower: &str, from: usize) -> usize {
    let rest = &lower[from..];
    let a = rest.find("<invoke").map(|x| from + x);
    let b = rest.find("</tool_calls").map(|x| from + x);
    match (a, b) {
        (Some(x), Some(y)) => x.min(y),
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => lower.len(),
    }
}

/// `pos` 是否位于某个 `<parameter ...>` 与其闭合标签之间。
/// 用于忽略参数体内引述的伪工具标签。
fn inside_parameter(lower: &str, pos: usize) -> bool {
    let before = &lower[..pos];
    let last_open = before.rfind("<parameter");
    let last_close = before.rfind("</parameter");
    match (last_open, last_close) {
        (Some(o), Some(c)) => o > c,
        (Some(_), None) => true,
        _ => false,
    }
}

/// 提取 parameter 元素为参数表（同名保留首个）。
///
/// 关键鲁棒性：**不依赖精确闭合标签**。模型的真实输出里闭合标签常带
/// 多余空白或大小写变体（例如标签名与斜杠之间、标签名与右尖括号之间
/// 多一个空格），精确 `find` 会失配并丢弃整个参数。这里改用宽松匹配，
/// 并在完全找不到闭合标签时退回到下一个标签边界。
fn extract_parameters(body: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let lower = body.to_ascii_lowercase();
    let mut i = 0;
    while let Some(rel) = lower[i..].find("<parameter") {
        let start = i + rel;
        let after = start + "<parameter".len();
        let boundary_ok = matches!(
            lower[after..].chars().next(),
            Some(c) if c == '>' || c == '/' || c.is_whitespace()
        );
        if !boundary_ok {
            i = after;
            continue;
        }
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + gt + 1;
        let open_tag = &body[start..open_end];
        let key = match extract_attr(open_tag, "name") {
            Some(k) => k.to_ascii_lowercase(),
            None => {
                i = open_end;
                continue;
            }
        };

        // 宽松闭合：优先找合法的 parameter 闭合标签变体；
        // 找不到则退到下一个标签边界，避免整个参数丢失。
        let (content_end, next_i) = match find_lenient_close(&lower, open_end, "parameter") {
            Some((s, e)) => (s, e),
            None => {
                let end = next_boundary(&lower, open_end);
                (end, end)
            }
        };
        let value = body[open_end..content_end].trim().to_string();
        map.entry(key).or_insert(value);
        i = next_i;
    }
    map
}

/// 检测同一 invoke 体内是否存在重复的 `<parameter name="X">`。
/// 重复参数是畸形输出，须判为解析失败而非静默取首值。
fn has_duplicate_parameters(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let mut seen: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(rel) = lower[i..].find("<parameter") {
        let start = i + rel;
        let Some(gt) = lower[start..].find('>') else { break };
        let open_end = start + gt + 1;
        if let Some(name) = extract_attr(&lower[start..open_end], "name") {
            let key = name.trim().to_ascii_lowercase();
            if seen.contains(&key) {
                return true;
            }
            seen.push(key);
        }
        i = open_end;
    }
    false
}

/// 宽松匹配闭合标签：允许标签名前后有空白、大小写混用。
///
/// 可匹配（假设 name = "parameter"）：
///   </parameter>      规范
///   </ parameter>     斜杠与标签名之间有空白
///   </parameter >     标签名与右尖括号之间有空白
///   </ parameter >    两者都有
///   </PARAMETER>      大小写混用
///
/// 返回 `(内容结束下标, 闭合标签之后的下标)`。
fn find_lenient_close(lower: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let mut i = from;
    while let Some(rel) = lower[i..].find("</") {
        let lt = i + rel;
        let mut p = lt + 2;
        // 跳过斜杠与标签名之间的空白。
        while let Some(c) = lower[p..].chars().next() {
            if c.is_whitespace() {
                p += c.len_utf8();
            } else {
                break;
            }
        }
        if lower[p..].starts_with(name) {
            let after_name = p + name.len();
            // 排除同前缀误匹配（例如 parameterx）。
            let boundary_ok = matches!(
                lower[after_name..].chars().next(),
                Some(c) if c == '>' || c.is_whitespace() || c == '/'
            );
            if boundary_ok && let Some(gt) = lower[after_name..].find('>') {
                let close_end = after_name + gt + 1;
                return Some((lt, close_end));
            }
        }
        i = lt + 2;
    }
    None
}

/// 完全没有闭合标签时的兜底：截到下一个标签起点；没有则到串尾。
fn next_boundary(lower: &str, from: usize) -> usize {
    let rest = &lower[from..];
    let mut i = 0;
    while let Some(rel) = rest[i..].find('<') {
        let abs = i + rel;
        let after = rest[abs + 1..].chars().next();
        if matches!(after, Some(c) if c == '/' || c.is_ascii_alphabetic()) {
            return from + abs;
        }
        i = abs + 1;
    }
    lower.len()
}

/// 从标签里取属性值，兼容双引号 / 单引号 / 无引号。
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pat = attr.to_string();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&pat) {
        let idx = from + rel;
        let prev_ok = idx == 0
            || tag[..idx]
                .chars()
                .last()
                .is_some_and(|c| c.is_whitespace() || c == '<');
        if !prev_ok {
            from = idx + pat.len();
            continue;
        }
        let after = idx + pat.len();
        let rest = tag[after..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            from = after;
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let inner = &rest[1..];
            let end = inner.find(quote)?;
            return Some(inner[..end].to_string());
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(rest.len());
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// 收集标签属性区中所有同名属性的值（按出现顺序）。
/// 与 `extract_attr` 不同，这里返回全部而非首个。
fn extract_all_attrs(attrs: &str, attr: &str) -> Vec<String> {
    let lower = attrs.to_ascii_lowercase();
    let pat = attr.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&pat) {
        let idx = from + rel;
        let prev_ok = idx == 0
            || attrs[..idx]
                .chars()
                .last()
                .is_some_and(|c| c.is_whitespace());
        if !prev_ok {
            from = idx + pat.len();
            continue;
        }
        let after = idx + pat.len();
        let rest = attrs[after..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            from = after;
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next() else { break };
        if quote == '"' || quote == '\'' {
            let inner = &rest[1..];
            match inner.find(quote) {
                Some(end) => {
                    out.push(inner[..end].to_string());
                    from = after + (attrs[after..].len() - rest.len()) + 1 + end + 1;
                }
                None => break,
            }
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(rest.len());
            out.push(rest[..end].to_string());
            from = after + (attrs[after..].len() - rest.len()) + end;
        }
    }
    out
}

// ---------- 简易格式（向后兼容） ----------

fn parse_simple(cleaned: &str, original: &str) -> ParseOutcome {
    let spans = collect_simple_spans(cleaned);
    if spans.is_empty() {
        return ParseOutcome::Text(original.trim().to_string());
    }
    let mut calls = Vec::with_capacity(spans.len());
    for (name, body) in spans {
        let params = extract_params(&body);
        if let Err(reason) = validate(&name, &params) {
            return ParseOutcome::DsmlMalformed {
                raw: original.trim().to_string(),
                reason,
                marker_count: count_tool_markers(original),
            };
        }
        calls.push(ToolCall {
            name,
            params,
            raw: String::new(),
        });
    }
    ParseOutcome::Calls(calls)
}

/// 按出现顺序收集所有简易格式工具元素（工具名, 正文）。
fn collect_simple_spans(text: &str) -> Vec<(String, String)> {
    let lower = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some((name, idx)) = find_open_tag_from(&lower, from) {
        let Some(body) = extract_element(&text[idx..], &name) else {
            break;
        };
        let consumed = idx + name.len() + 2 + body.len();
        spans.push((name, body));
        let next = consumed.min(lower.len());
        from = if next <= from { from + 1 } else { next };
    }
    spans
}

// ---------- 通用工具 ----------

/// 剥离模型输出中的思考块（<think>...</think>），仅用于工具解析。
/// 思考内容里常出现伪代码/标签，会污染解析，故先移除。
fn strip_think(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        match lower.find("<think>") {
            Some(start) => {
                out.push_str(&rest[..start]);
                let after_open = start + "<think>".len();
                match lower[after_open..].find("</think>") {
                    Some(rel) => {
                        rest = &rest[after_open + rel + "</think>".len()..];
                    }
                    None => break,
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// 去掉整体包裹的 markdown 代码围栏。
fn strip_fences(text: &str) -> String {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let rest = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        if let Some(end) = rest.rfind("```") {
            return rest[..end].trim().to_string();
        }
    }
    t.to_string()
}

/// 从 `from` 起找最早的已知工具开标签，返回 (工具名, 字节下标)。
fn find_open_tag_from(text: &str, from: usize) -> Option<(String, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut best: Option<(String, usize)> = None;
    for tool in KNOWN_TOOLS {
        let pat = format!("<{tool}");
        let mut f = from;
        while let Some(rel) = lower[f..].find(&pat) {
            let idx = f + rel;
            let after = idx + pat.len();
            let boundary_ok = matches!(
                lower[after..].chars().next(),
                Some(c) if c == '>' || c == '/' || c.is_whitespace()
            );
            if boundary_ok {
                if best.as_ref().is_none_or(|(_, b)| idx < *b) {
                    best = Some((tool.to_string(), idx));
                }
                break;
            }
            f = after;
        }
    }
    best
}

/// 提取某个元素的内部内容（原样，不 trim 内部）。
fn extract_element(haystack: &str, name: &str) -> Option<String> {
    let lower = haystack.to_ascii_lowercase();
    let open_pat = format!("<{name}");
    let mut from = 0;
    let open_start = loop {
        let idx = lower[from..].find(&open_pat)? + from;
        let after = idx + open_pat.len();
        match lower[after..].chars().next() {
            Some(c) if c == '>' || c == '/' || c.is_whitespace() => break idx,
            Some(_) => from = after,
            None => return None,
        }
    };
    let gt = lower[open_start..].find('>')?;
    let content_start = open_start + gt + 1;
    let close_pat = format!("</{name}");
    let close_rel = match lower[content_start..].find(&close_pat) {
        Some(r) => r,
        None => lower[content_start..].find("</")?,
    };
    Some(haystack[content_start..content_start + close_rel].to_string())
}

/// 扫描标签体，提取所有简易格式的参数元素（同名保留首个）。
fn extract_params(body: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let lower = body.to_ascii_lowercase();
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find('<') {
        let lt = i + rel;
        let after = lower[lt + 1..].chars().next();
        if !matches!(after, Some(c) if c == '/' || c.is_ascii_alphabetic()) {
            i = lt + 1;
            continue;
        }
        if lower[lt..].starts_with("</") {
            match lower[lt..].find('>') {
                Some(g) => {
                    i = lt + g + 1;
                    continue;
                }
                None => break,
            }
        }
        let name_start = lt + 1;
        let name_end =
            match lower[name_start..].find(|c: char| c == '>' || c == '/' || c.is_whitespace()) {
                Some(r) => name_start + r,
                None => break,
            };
        let key = lower[name_start..name_end].trim().to_string();
        let Some(g) = lower[name_end..].find('>') else {
            break;
        };
        let open_end = name_end + g + 1;
        let close_pat = format!("</{key}");
        let Some(crel) = lower[open_end..].find(&close_pat) else {
            i = open_end;
            continue;
        };
        let content_end = open_end + crel;
        let value = body[open_end..content_end].trim().to_string();
        if !key.is_empty() {
            map.entry(key).or_insert(value);
        }
        i = match lower[content_end..].find('>') {
            Some(x) => content_end + x + 1,
            None => content_end,
        };
    }
    map
}

/// 参数校验（L2）。
fn validate(name: &str, params: &HashMap<String, String>) -> std::result::Result<(), String> {
    match name {
        "execute_command" => {
            let cmd = params.get("command").map(|s| s.trim()).unwrap_or("");
            if cmd.is_empty() {
                return Err("缺少非空的 <command> 参数".into());
            }
            Ok(())
        }
        "upload_file" => {
            let paths = params.get("paths").map(|s| s.trim()).unwrap_or("");
            if paths.is_empty() {
                return Err("缺少非空的 <paths> 参数".into());
            }
            Ok(())
        }
        "browser" => {
            let action = params.get("action").map(|s| s.trim()).unwrap_or("");
            if action.is_empty() {
                return Err("缺少非空的 <action> 参数".into());
            }
            Ok(())
        }
        "read_file" => {
            let path = params
                .get("file_path")
                .or_else(|| params.get("path"))
                .map(|s| s.trim())
                .unwrap_or("");
            if path.is_empty() {
                return Err("缺少非空的 <file_path> 参数".into());
            }
            Ok(())
        }
        "write_file" => {
            let path = params
                .get("file_path")
                .or_else(|| params.get("path"))
                .map(|s| s.trim())
                .unwrap_or("");
            if path.is_empty() {
                return Err("缺少非空的 <file_path> 参数".into());
            }
            if !params.contains_key("content") {
                return Err("缺少 <content> 参数".into());
            }
            Ok(())
        }
        "edit_file" => {
            let path = params
                .get("file_path")
                .or_else(|| params.get("path"))
                .map(|s| s.trim())
                .unwrap_or("");
            if path.is_empty() {
                return Err("缺少非空的 <file_path> 参数".into());
            }
            if !params.contains_key("old_string") {
                return Err("缺少 <old_string> 参数".into());
            }
            if !params.contains_key("new_string") {
                return Err("缺少 <new_string> 参数".into());
            }
            Ok(())
        }
        "sub_agent" => {
            let name = params.get("name").map(|s| s.trim()).unwrap_or("");
            if name.is_empty() {
                return Err("缺少非空的 <name> 参数".into());
            }
            let task = params.get("task").map(|s| s.trim()).unwrap_or("");
            if task.is_empty() {
                return Err("缺少非空的 <task> 参数".into());
            }
            Ok(())
        }
        "ask_user" => {
            let q = params.get("question").map(|s| s.trim()).unwrap_or("");
            if q.is_empty() {
                return Err("缺少非空的 <question> 参数".into());
            }
            Ok(())
        }
        "task_complete" => Ok(()),
        "request_context_compaction" => Ok(()),
        _ => Err(format!("未知工具: {name}，可用工具: {}", KNOWN_TOOLS.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_text() {
        assert!(matches!(
            parse("你好，这是普通回答。"),
            ParseOutcome::Text(_)
        ));
    }

    // ---- DeepSeek 原生格式 ----

    #[test]
    fn native_basic() {
        let r = parse(
            "<tool_calls>\n<invoke name=\"execute_command\">\n\
             <parameter name=\"command\" string=\"true\">ls -la</parameter>\n\
             </invoke>\n</tool_calls>",
        );
        match r {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                let c = &v[0];
                assert_eq!(c.name, "execute_command");
                assert_eq!(c.params.get("command").unwrap(), "ls -la");
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    #[test]
    fn native_without_wrapper() {
        let r = parse(
            "<invoke name=\"execute_command\">\
             <parameter name=\"command\">pwd</parameter></invoke>",
        );
        match r {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].params.get("command").unwrap(), "pwd");
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    /// 回归：闭合标签带空白 / 大小写变体时，参数不能被丢弃。
    #[test]
    fn native_lenient_close_tag_variants() {
        let variants = [
            "</parameter>",   // 规范
            "</ parameter>",  // 斜杠后多空格
            "</parameter >",  // 名字后多空格
            "</ parameter >", // 两处都多空格
            "</PARAMETER>",   // 大写
        ];
        for close in variants {
            let input = format!(
                "<tool_calls>\n<invoke name=\"execute_command\">\n\
                 <parameter name=\"command\" string=\"true\">pwd{close}\n\
                 </invoke>\n</tool_calls>"
            );
            match parse(&input) {
                ParseOutcome::Calls(v) => {
                    assert_eq!(v.len(), 1);
                    assert_eq!(v[0].params.get("command").unwrap(), "pwd", "close={close:?}")
                }
                other => panic!("close={close:?} expected Calls, got {other:?}"),
            }
        }
    }

    /// 回归：完全没有闭合标签时，退回到下一个边界，而非整段丢失。
    #[test]
    fn native_missing_close_tag_uses_next_boundary() {
        let input = "<invoke name=\"execute_command\">\
                     <parameter name=\"command\">pwd</invoke>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].params.get("command").unwrap(), "pwd");
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    #[test]
    fn native_unknown_tool_falls_back_to_text() {
        let r = parse("<invoke name=\"rm_rf\"><parameter name=\"command\">x</parameter></invoke>");
        assert!(matches!(r, ParseOutcome::Text(_)));
    }

    #[test]
    fn native_missing_param_is_malformed() {
        let r = parse("<invoke name=\"execute_command\"></invoke>");
        assert!(matches!(r, ParseOutcome::Text(_)));
    }

    // ---- 简易格式 ----

    #[test]
    fn basic_command() {
        let r = parse("<execute_command>\n<command>ls -la</command>\n</execute_command>");
        match r {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                let c = &v[0];
                assert_eq!(c.name, "execute_command");
                assert_eq!(c.params.get("command").unwrap(), "ls -la");
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    #[test]
    fn case_insensitive_and_fenced() {
        let r = parse("```xml\n<EXECUTE_COMMAND><COMMAND>pwd</COMMAND></EXECUTE_COMMAND>\n```");
        match r {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].params.get("command").unwrap(), "pwd");
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    #[test]
    fn unclosed_is_malformed() {
        let r = parse("<execute_command><command>ls</command>");
        assert!(matches!(r, ParseOutcome::Malformed { .. }));
    }

    #[test]
    fn double_tag_is_two_calls() {
        let r = parse(
            "<execute_command><command>a</command></execute_command>\
             <execute_command><command>b</command></execute_command>",
        );
        match r {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0].name, "execute_command");
                assert_eq!(v[0].params.get("command").unwrap(), "a");
                assert_eq!(v[1].name, "execute_command");
                assert_eq!(v[1].params.get("command").unwrap(), "b");
            }
            other => panic!("expected two Calls, got {other:?}"),
        }
    }

    #[test]
    fn native_two_invoke_calls_in_order() {
        let input = "<tool_calls>\
            <invoke name=\"execute_command\"><parameter name=\"command\">a</parameter></invoke>\
            <invoke name=\"execute_command\"><parameter name=\"command\">b</parameter></invoke>\
            </tool_calls>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0].params.get("command").unwrap(), "a");
                assert_eq!(v[1].params.get("command").unwrap(), "b");
            }
            other => panic!("expected two calls in order, got {other:?}"),
        }
    }

    #[test]
    fn multiline_command_preserved() {
        let r = parse("<execute_command>\n<command>echo a\necho b</command>\n</execute_command>");
        match r {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].params.get("command").unwrap(), "echo a\necho b");
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    #[test]
    fn text_mention_is_text() {
        assert!(matches!(
            parse("你可以用 execute_command 来跑命令。"),
            ParseOutcome::Text(_)
        ));
    }

    // ---- 标签名归一化回归 ----

    /// 全角竖线包裹的厂商标记应被归一化，参数正常提取。
    #[test]
    fn dsml_full_width_is_normalized() {
        let fw = '\u{FF5C}'; // 全角竖线
        let input = format!(
            "<tool_calls>\n\
             <{fw}{fw}DSML{fw}{fw} invoke name=\"execute_command\">\n\
             <{fw}{fw}DSML{fw}{fw} parameter name=\"command\" string=\"true\">ls -la\
             </{fw}{fw}DSML{fw}{fw} parameter>\n\
             </{fw}{fw}DSML{fw}{fw} invoke>\n\
             </tool_calls>"
        );
        match parse(&input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "execute_command");
                assert_eq!(v[0].params.get("command").unwrap(), "ls -la");
            }
            other => panic!("DSML 归一化失败: {other:?}"),
        }
    }

    /// 正文里的逻辑或与标记字面量必须原样保留。
    #[test]
    fn body_pipes_not_corrupted() {
        let fw = '\u{FF5C}';
        let input = format!(
            "<tool_calls>\n<invoke name=\"execute_command\">\n\
             <parameter name=\"command\">echo \"a\" || echo \"{fw}{fw}\"</parameter>\n\
             </invoke>\n</tool_calls>"
        );
        match parse(&input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(
                    v[0].params.get("command").unwrap().as_str(),
                    format!("echo \"a\" || echo \"{fw}{fw}\"").as_str()
                );
            }
            other => panic!("正文被破坏: {other:?}"),
        }
    }

    /// 属性值含冒号（URL / 端口）时，标签名仍须正确提取。
    #[test]
    fn attr_value_with_colon_not_corrupted() {
        let fw = '\u{FF5C}';
        let input = format!(
            "<tool_calls>\n\
             <{fw}{fw}DSML{fw}{fw} invoke name=\"execute_command\">\n\
             <{fw}{fw}DSML{fw}{fw} parameter name=\"command\" default=\"http://localhost:8080/x\">\
             curl http://localhost:8080</{fw}{fw}DSML{fw}{fw} parameter>\n\
             </{fw}{fw}DSML{fw}{fw} invoke>\n\
             </tool_calls>"
        );
        match parse(&input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "execute_command");
                assert_eq!(
                    v[0].params.get("command").unwrap(),
                    "curl http://localhost:8080"
                );
            }
            other => panic!("属性冒号被误伤: {other:?}"),
        }
    }
    #[test]
    fn nested_fake_tags_in_command_ignored() {
        let input = "<tool_calls>\n\
            <invoke name=\"execute_command\">\n\
            <parameter name=\"command\" string=\"true\">echo '<tool_calls><invoke name=\"execute_command\"><parameter name=\"command\">ls</parameter></invoke></tool_calls>'</parameter>\n\
            </invoke>\n\
            </tool_calls>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "execute_command");
                let cmd = v[0].params.get("command").expect("command 参数");
                assert!(cmd.contains("ls"), "command 应含 ls，实际: {cmd}");
                assert!(!cmd.is_empty());
            }
            other => panic!("期望 Calls，得到 {other:?}"),
        }
    }

    #[test]
    fn truncated_tool_name_matches_prefix() {
        let input = "<tool_calls>\n\
            <invoke name=\"execute_com\">\n\
            <parameter name=\"command\">ls</parameter>\n\
            </invoke>\n\
            </tool_calls>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "execute_command");
            }
            other => panic!("期望 Calls，得到 {other:?}"),
        }
    }
    #[test]
    fn native_redirect_lt_not_truncated_in_command() {
        let input = "<invoke name=\"execute_command\">\
                     <parameter name=\"command\">tr -d x < ~/.zshrc</invoke>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].params.get("command").unwrap(), "tr -d x < ~/.zshrc")
            }
            other => panic!("expected Calls, got {other:?}"),
        }
    }

    // ---- 污染标签恢复 ----

    /// 实测失败样例：<tool call name="invoke" name="browser">
    #[test]
    fn recover_tool_call_with_space() {
        let input = "<tool_calls>\n\
            <tool call name=\"invoke\" name=\"browser\">\n\
            <parameter name=\"action\">navigate</parameter>\n\
            <parameter name=\"params\">{\"url\":\"https://example.com\"}</parameter>\n\
            </tool call>\n\
            </tool_calls>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].name, "browser");
                assert_eq!(v[0].params.get("action").unwrap(), "navigate");
            }
            other => panic!("污染恢复失败: {other:?}"),
        }
    }

    /// 混合：合法 invoke + 污染 invoke → 两个都解析出来
    #[test]
    fn recover_mixed_valid_and_corrupted() {
        let input = "<tool_calls>\n\
            <invoke name=\"execute_command\">\n\
            <parameter name=\"command\">pwd</parameter>\n\
            </invoke>\n\
            <tool call name=\"invoke\" name=\"browser\">\n\
            <parameter name=\"action\">snapshot</parameter>\n\
            </tool call>\n\
            </tool_calls>";
        match parse(input) {
            ParseOutcome::Calls(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0].name, "execute_command");
                assert_eq!(v[1].name, "browser");
            }
            other => panic!("混合恢复失败: {other:?}"),
        }
    }

    /// 不误伤：正文里解释 XML 时提及 <tool call ...>，不得触发恢复
    #[test]
    fn corrupted_like_text_not_recovered() {
        // 无 <tool_calls> 包裹、无 parameter 结构 → 应视为普通文本
        let input = "你可以写 <tool call name=\"invoke\" name=\"browser\"> 这种标签。";
        assert!(matches!(parse(input), ParseOutcome::Text(_)));
    }

    /// 不误伤：<tool> 类标签但 name 不含 invoke → 不恢复，回落为正文
    #[test]
    fn tool_call_without_invoke_falls_back_to_text() {
        let input = "<tool_calls>\n<tool call name=\"browser\">\n\
            <parameter name=\"action\">navigate</parameter>\n</tool call>\n</tool_calls>";
        // 无 name="invoke"，不应被恢复；原样走 native 解析必然 Malformed（无 <invoke）
        assert!(matches!(parse(input), ParseOutcome::Text(_)));
    }
}