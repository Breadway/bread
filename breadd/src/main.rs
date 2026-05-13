mod adapters;
mod core;
mod ipc;
mod lua;

use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use anyhow::Result;
use bread_shared::{AdapterSource, BreadEvent, RawEvent};
use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::core::config::Config;
use crate::core::normalizer::EventNormalizer;
use crate::core::state_engine::{run_state_engine, StateHandle};
use crate::core::types::RuntimeState;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(config.daemon.log_level.clone()))
        .init();

    info!("starting breadd");

    let state = Arc::new(RwLock::new(RuntimeState::default()));

    let (raw_tx, mut raw_rx) = mpsc::channel::<RawEvent>(2048);
    let (normalized_tx, normalized_rx) = mpsc::unbounded_channel::<BreadEvent>();
    let (state_cmd_tx, state_cmd_rx) = mpsc::unbounded_channel();
    let (event_stream_tx, _) = broadcast::channel(2048);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let subscription_count = Arc::new(AtomicU64::new(0));
    let state_handle = StateHandle::new(state.clone(), state_cmd_tx);

    let lua_runtime =
        lua::spawn_runtime(config.clone(), state_handle.clone(), normalized_tx.clone())?;
    let lua_tx = lua_runtime.sender();

    tokio::spawn(run_state_engine(
        normalized_rx,
        state_cmd_rx,
        state.clone(),
        lua_tx,
        event_stream_tx.clone(),
        subscription_count.clone(),
        shutdown_rx.clone(),
    ));

    let normalizer = Arc::new(EventNormalizer::new(config.events.dedup_window_ms));
    {
        let normalizer = normalizer.clone();
        let normalized_tx = normalized_tx.clone();
        let mut shutdown_rx = shutdown_rx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    maybe_raw = raw_rx.recv() => {
                        let Some(raw) = maybe_raw else {
                            break;
                        };
                        for event in normalizer.normalize(&raw) {
                            if normalized_tx.send(event).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    let adapter_manager = adapters::Manager::new(raw_tx, config.clone(), shutdown_rx.clone());
    adapter_manager.start_all().await?;

    let adapter_status = adapter_manager.status_handle();

    let event_buffer = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(1000)));
    {
        let mut rx = event_stream_tx.subscribe();
        let event_buffer = event_buffer.clone();
        tokio::spawn(async move {
            loop {
                let evt = match rx.recv().await {
                    Ok(evt) => evt,
                    Err(_) => break,
                };
                if let Ok(mut buf) = event_buffer.lock() {
                    if buf.len() >= 1000 {
                        buf.pop_front();
                    }
                    buf.push_back(evt);
                }
            }
        });
    }

    let _ = normalized_tx.send(BreadEvent::new(
        "bread.system.startup",
        AdapterSource::System,
        serde_json::json!({}),
    ));

    let ipc_server = ipc::Server::new(
        config.socket_path(),
        state_handle,
        event_stream_tx,
        lua_runtime.clone(),
        normalized_tx,
        adapter_status,
        subscription_count,
        event_buffer,
    );

    info!("breadd fully started");
    tokio::select! {
        result = ipc_server.serve(shutdown_rx.clone()) => {
            if let Err(err) = result {
                error!(error = %err, "ipc server failed");
            }
        }
        _ = wait_for_shutdown() => {
            info!("shutdown signal received");
        }
    }

    let _ = shutdown_tx.send(true);

    lua_runtime.shutdown();
    Ok(())
}

async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
