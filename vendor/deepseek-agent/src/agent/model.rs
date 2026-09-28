//! Model trait —— LLM 后端抽象（扩展点）。
//!
//! 现在实现 `DeepSeekModel`；未来可加 `OpenAIModel`、`LocalModel` 等。

use crate::api::types::{CompletionReq, DeltaKind};
use crate::client::http::HttpClient;
use crate::client::pow::PowSolver;
use crate::error::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 模型回复
#[derive(Debug, Clone)]
pub struct ModelReply {
    pub content: String,
    /// 本轮回复的消息 id，用于下一轮 parent_message_id
    pub message_id: Option<i64>,
    /// 会话标题（来自 SSE Title 事件，可能为空）
    pub title: Option<String>,
    /// Bridge 后端专用：本轮返回的服务端 session id（首次请求时才存在，用于延迟绑定）
    pub session_id: Option<String>,
}

#[async_trait]
pub trait Model: Send + Sync {
    /// 发一轮消息。`on_delta` 用于流式回调。
    async fn complete(
        &self,
        req: &CompletionReq,
        on_delta: &mut (dyn for<'a> FnMut(DeltaKind, &'a str) + Send),
    ) -> Result<ModelReply>;

    /// 新建对话会话
    async fn new_session(&self) -> Result<String>;

    /// 向服务器查询该会话的最新 message_id。
    /// 返回 `Some(mid)` 表示应以服务器 mid 为准；`None` 表示后端不支持查询。
    /// 默认实现不做任何事（返回 None），由具体后端按需覆盖。
    async fn sync_parent(&self, _session_id: &str, _local: Option<i64>) -> Result<Option<i64>> {
        Ok(None)
    }

    /// 请求后端中断当前生成（仅浏览器后端有意义）。默认不做任何事。
    async fn stop(&self) {}

    async fn message_count(&self, _session_id: &str) -> Result<Option<i64>> {
        Ok(None)
    }

    /// 删除服务端会话。默认不做任何事，由具体后端按需覆盖。
    async fn delete_session(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }

    /// 同步 toggle 状态到后端（仅浏览器后端有意义）。默认不做任何事。
    async fn set_toggles(&self, _thinking: bool, _search: bool) -> Result<()> {
        Ok(())
    }

    async fn upload_files(&self, _session_id: Option<&str>, _paths: &[String]) -> Result<usize> {
        Ok(0)
    }

    async fn list_files(&self, _session_id: Option<&str>) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn remove_file(&self, _session_id: Option<&str>, _name: &str) -> Result<bool> {
        Ok(false)
    }

    async fn clear_files(&self, _session_id: Option<&str>) -> Result<usize> {
        Ok(0)
    }
}

/// DeepSeek 后端实现
pub struct DeepSeekModel {
    http: Arc<HttpClient>,
    solver: Arc<Mutex<PowSolver>>,
}

impl DeepSeekModel {
    pub fn new(http: Arc<HttpClient>, solver: Arc<Mutex<PowSolver>>) -> Self {
        Self { http, solver }
    }
}

#[async_trait]
impl Model for DeepSeekModel {
    async fn complete(
        &self,
        req: &CompletionReq,
        on_delta: &mut (dyn for<'a> FnMut(DeltaKind, &'a str) + Send),
    ) -> Result<ModelReply> {
        let mut solver = self.solver.lock().await;
let (content, message_id, title) =
    crate::api::chat::completion(&self.http, &mut solver, req, on_delta).await?;
Ok(ModelReply {
    content,
    message_id,
    title,
    session_id: None,
})
    }

    async fn new_session(&self) -> Result<String> {
        crate::api::chat::create_session(&self.http).await
    }

    async fn sync_parent(&self, session_id: &str, _local: Option<i64>) -> Result<Option<i64>> {
        match crate::api::chat::latest_message_id(&self.http, session_id).await {
            Ok(server_mid) => Ok(server_mid),
            Err(_) => Ok(None),
        }
    }

    async fn message_count(&self, session_id: &str) -> Result<Option<i64>> {
        crate::api::chat::message_count(&self.http, session_id).await
    }

    async fn delete_session(&self, session_id: &str) -> Result<()> {
        crate::api::chat::delete_session(&self.http, session_id).await
    }
}