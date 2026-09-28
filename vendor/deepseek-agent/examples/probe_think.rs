use deepseek_agent::agent::model::Model;
use deepseek_agent::agent::parser::{self, ParseOutcome};
use deepseek_agent::api::types::{CompletionReq, DeltaKind};
use deepseek_agent::{Config, Runtime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::for_account("z")?;
    let rt = Runtime::new(&cfg).await?;
    let model = rt.model();
    let sid = model.new_session().await?;
    eprintln!("[session] {sid}");
    let prompt = std::env::args().nth(1).unwrap_or_else(|| "请一步步推理：35个头.94只脚，几只几只？".to_string());
    let mut req = CompletionReq::new(sid, prompt);
    req.thinking_enabled = true;
    let mut answer = String::new();
    let mut thinking = String::new();
    let mut on_delta = |kind: DeltaKind, text: &str| {
        match kind {
            DeltaKind::Thinking => { thinking.push_str(text); eprintln!("[THINK] {text}"); }
            DeltaKind::Answer => { answer.push_str(text); eprintln!("[ANSWER] {text}"); }
        }
    };
    let reply = model.complete(&req, &mut on_delta).await?;
    eprintln!("\n---------- 统计 ----------");
    eprintln!("thinking len={} answer len={} content len={}", thinking.len(), answer.len(), reply.content.len());
    eprintln!("content==answer? {}", reply.content == answer);
    eprintln!("\n---------- parser ----------");
    match parser::parse(&reply.content) {
        ParseOutcome::Text(_) => eprintln!("Text"),
        ParseOutcome::Calls(c) => eprintln!("Calls {}", c.len()),
        ParseOutcome::Malformed {reason, ..} => eprintln!("Malformed: {reason}"),
        ParseOutcome::DsmlMalformed {reason, marker_count, ..} => eprintln!("DsmlMalformed: {reason} mc={marker_count}"),
    }
    Ok(())
}
