use config::AgentConfig;
use protocol::{
    decode_json, read_frame, write_json_frame, MsgType, StatusRequestPayload, StatusResponsePayload,
};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

#[derive(Debug, Clone)]
pub struct AgentScore {
    pub config: AgentConfig,
    pub status: Option<StatusResponsePayload>,
    pub latency: Duration,
}

/// Probes an agent daemon at `host` using the STATUS protocol exchange.
///
/// Sends `MsgType::Status` containing the provided token and awaits `MsgType::StatusResp`.
/// Returns the parsed `StatusResponsePayload` on success, or an error if unreachable/unauthorized.
pub async fn probe_agent_status(
    host: &str,
    token: &str,
) -> Result<StatusResponsePayload, Box<dyn std::error::Error + Send + Sync>> {
    let connect_future = TcpStream::connect(host);
    let mut stream = tokio::time::timeout(Duration::from_secs(3), connect_future)
        .await
        .map_err(|_| "connection timeout")??;

    let req = StatusRequestPayload {
        token: token.to_string(),
    };
    write_json_frame(&mut stream, MsgType::Status, &req).await?;

    let read_future = read_frame(&mut stream);
    let (msg_type, payload) = tokio::time::timeout(Duration::from_secs(3), read_future)
        .await
        .map_err(|_| "read timeout")??;

    if msg_type != MsgType::StatusResp {
        return Err(format!("unexpected response frame: {:?}", msg_type).into());
    }

    let resp: StatusResponsePayload = decode_json(&payload)?;
    Ok(resp)
}

/// Selects the best candidate agent from a pool of agents.
///
/// 1. Filters candidate agents by `required_tag` (if specified).
/// 2. Probes all remaining candidates in parallel using `join_all`.
/// 3. Selects the responsive candidate with the lowest load (`active_runs + queue_depth`),
///    breaking ties using network latency.
pub async fn select_best_agent(
    agents: &[AgentConfig],
    required_tag: Option<&str>,
    verbose: bool,
) -> Result<AgentConfig, Box<dyn std::error::Error + Send + Sync>> {
    let filtered: Vec<&AgentConfig> = agents
        .iter()
        .filter(|a| required_tag.is_none_or(|t| a.tags.iter().any(|tag| tag == t)))
        .collect();

    if filtered.is_empty() {
        if let Some(t) = required_tag {
            return Err(format!("no configured agents match tag '{}'", t).into());
        } else {
            return Err("no candidate agents configured in pool".into());
        }
    }

    if verbose {
        println!(
            "[farhand] Probing {} candidate agent(s) in pool...",
            filtered.len()
        );
    }

    let probe_tasks: Vec<_> = filtered
        .into_iter()
        .map(|agent| {
            let agent = agent.clone();
            async move {
                let start = Instant::now();
                let token = agent.token.clone().unwrap_or_default();
                let status = probe_agent_status(&agent.host, &token).await.ok();
                let latency = start.elapsed();
                AgentScore {
                    config: agent,
                    status,
                    latency,
                }
            }
        })
        .collect();

    let scores = futures::future::join_all(probe_tasks).await;

    if verbose {
        for s in &scores {
            match &s.status {
                Some(st) => {
                    println!(
                        "  -> Agent {}: reachable in {:?}, active runs: {}/{}, queue: {}, host: '{}', tags: {:?}",
                        s.config.host,
                        s.latency,
                        st.active_runs,
                        st.max_runs,
                        st.queue_depth,
                        st.hostname,
                        st.tags
                    );
                }
                None => {
                    println!("  -> Agent {}: unreachable or unauthorized", s.config.host);
                }
            }
        }
    }

    let best = scores
        .into_iter()
        .filter(|s| s.status.is_some())
        .min_by_key(|s| {
            let st = s.status.as_ref().unwrap();
            (st.active_runs + st.queue_depth, s.latency)
        })
        .ok_or("all candidate agents are unreachable or rejected the probe")?;

    if verbose {
        println!(
            "[farhand] Selected best agent: {} (latency: {:?})",
            best.config.host, best.latency
        );
    }

    Ok(best.config)
}
