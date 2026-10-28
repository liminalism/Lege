// GPU layout inference session pool.
//
// Each LayoutEngine/CompiledGraph remains single-flight because it owns
// activation and readback buffers. The pool creates K sibling sessions on the
// process-wide shared WGPU device and checks one out per page. This removes the
// serial InferenceActor and its image-cloning batch path while allowing pages
// to overlap until the GPU saturates.

use crate::engine::{Detection, LayoutEngine, LayoutEngineConfig};
use crate::pipeline::config::PipelineConfig;
use anyhow::{Context, Result, anyhow};
use image::RgbImage;
use log::info;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::Semaphore;

const DEFAULT_GPU_SESSIONS: usize = 2;
const MIN_GPU_SESSIONS: usize = 1;
const MAX_GPU_SESSIONS: usize = 4;
const DEFAULT_VRAM_BUDGET_MB: usize = 2 * 1024;
// Conservative admission estimate for one PP-DocLayout-M inference. Resident
// session buffers are allocated at pool creation; this covers transient
// command/readback/input pressure while a checked-out session is in flight.
const SESSION_TRANSIENT_MB: usize = 384;

struct SessionPool {
    sessions: Mutex<Vec<LayoutEngine>>,
    available: Arc<Semaphore>,
    vram: Arc<Semaphore>,
    vram_budget_mb: usize,
    session_count: usize,
}

impl SessionPool {
    fn new(config: &PipelineConfig) -> Result<Self> {
        let engine_config = LayoutEngineConfig {
            confidence_threshold: config.confidence_threshold(),
            nms_threshold: config.nms_threshold(),
            iou_threshold: config.nms_threshold(),
            batch_size: 1,
        };

        let requested_sessions = std::env::var("LEGE_GPU_SESSIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_GPU_SESSIONS);
        let vram_budget_mb = std::env::var("LEGE_VRAM_BUDGET_MB")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_VRAM_BUDGET_MB)
            .max(1);
        // Every pipeline derives its page concurrency from this same knob and
        // clamps it down further, so sessions beyond it could never be checked
        // out concurrently. Capping here keeps a one-worker run to one
        // resident session even when LEGE_GPU_SESSIONS is unset.
        let policy_cap = config.max_parallel_pages().unwrap_or(4).max(1);
        let session_count = session_count_for(requested_sessions, vram_budget_mb, policy_cap);

        info!(
            "InferenceSessionPool: creating {} session(s), VRAM budget={} MiB",
            session_count, vram_budget_mb
        );
        let first = LayoutEngine::new(config.model_path(), engine_config)
            .context("failed to create the first layout inference session")?;
        let provider = first.provider_name().to_string();
        let mut sessions = Vec::with_capacity(session_count);
        sessions.push(first);
        while sessions.len() < session_count {
            let sibling = sessions[0]
                .build_sibling()
                .with_context(|| format!("failed to create layout session {}", sessions.len()))?;
            sessions.push(sibling);
        }
        info!(
            "InferenceSessionPool: initialized {} {} session(s) on the shared device",
            sessions.len(),
            provider
        );
        crate::runtime_stats::record_gpu_sessions(sessions.len());

        Ok(Self {
            sessions: Mutex::new(sessions),
            available: Arc::new(Semaphore::new(session_count)),
            vram: Arc::new(Semaphore::new(vram_budget_mb)),
            vram_budget_mb,
            session_count,
        })
    }

    fn transient_mb_for(image: &RgbImage) -> usize {
        let input_bytes = (image.width() as usize)
            .saturating_mul(image.height() as usize)
            .saturating_mul(3);
        SESSION_TRANSIENT_MB.saturating_add(input_bytes.div_ceil(1024 * 1024))
    }

    /// Lock the session pool, recovering rather than propagating a poisoned
    /// lock. The guarded value is a plain free list: a panic elsewhere cannot
    /// leave it logically torn, so refusing to hand out an engine would turn a
    /// survivable page failure into a dead pipeline.
    fn lock_sessions(&self) -> MutexGuard<'_, Vec<LayoutEngine>> {
        match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn run(&self, image: &RgbImage) -> Result<Vec<Detection>> {
        // The semaphore admits at most `sessions.len()` callers, so the pool is
        // never empty here; treat a violation as an error rather than a panic.
        let Some(mut engine) = self.lock_sessions().pop() else {
            return Err(anyhow!(
                "layout session pool was empty despite a granted permit"
            ));
        };
        let result = catch_unwind(AssertUnwindSafe(|| engine.detect_single_blocking(image)))
            .unwrap_or_else(|payload| {
                Err(anyhow!(
                    "layout inference panicked: {}",
                    panic_payload_message(payload)
                ))
            });
        self.lock_sessions().push(engine);
        result
    }
}

#[derive(Clone)]
pub struct InferenceHandle {
    pool: Arc<SessionPool>,
}

impl InferenceHandle {
    pub fn new(config: &PipelineConfig) -> Result<Self> {
        Ok(Self {
            pool: Arc::new(SessionPool::new(config)?),
        })
    }

    /// Check out one GPU session and run a single page. Permit order is VRAM
    /// first, then session, matching the global byte-before-GPU ordering rule.
    pub async fn detect(&self, page_index: usize, image: Arc<RgbImage>) -> Result<Vec<Detection>> {
        let requested_mb =
            SessionPool::transient_mb_for(&image).min(self.pool.vram_budget_mb) as u32;
        let _vram = Arc::clone(&self.pool.vram)
            .acquire_many_owned(requested_mb.max(1))
            .await
            .map_err(|_| anyhow!("layout VRAM admission semaphore closed"))?;
        let _session = Arc::clone(&self.pool.available)
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("layout inference session pool closed"))?;

        let pool = Arc::clone(&self.pool);
        crate::runtime_stats::spawn_blocking_stage(
            crate::runtime_stats::Stage::Inference,
            move || {
                pool.run(&image)
                    .with_context(|| format!("layout inference failed on page {page_index}"))
            },
        )
        .await
        .map_err(|error| anyhow!("layout inference worker panicked: {error}"))?
    }

    pub fn has_capacity(&self) -> bool {
        self.pool.available.available_permits() > 0
    }

    pub fn session_count(&self) -> usize {
        self.pool.session_count
    }
}

/// How many resident sessions to construct: the request clamped to
/// `[MIN_GPU_SESSIONS, MAX_GPU_SESSIONS]`, then capped by what the VRAM
/// budget can plausibly support and by what the processing policy can check
/// out concurrently. A deliberately tiny VRAM budget reduces the pool to one
/// session instead of failing initialization.
fn session_count_for(requested: usize, vram_budget_mb: usize, policy_cap: usize) -> usize {
    let requested = requested.clamp(MIN_GPU_SESSIONS, MAX_GPU_SESSIONS);
    let budget_sessions = (vram_budget_mb.max(1) / SESSION_TRANSIENT_MB).max(1);
    requested.min(budget_sessions).min(policy_cap.max(1)).max(1)
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

pub fn is_layout_software_adapter_error(error: &(dyn std::error::Error + 'static)) -> bool {
    lege_gpu::vision::is_layout_software_adapter_error(error)
}

pub fn is_gpu_device_error(error: &(dyn std::error::Error + 'static)) -> bool {
    lege_gpu::vision::is_layout_gpu_device_error(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_session_request_selects_one_session() {
        assert_eq!(session_count_for(1, DEFAULT_VRAM_BUDGET_MB, 4), 1);
        assert_eq!(session_count_for(2, DEFAULT_VRAM_BUDGET_MB, 4), 2);
    }

    #[test]
    fn session_count_never_exceeds_what_policy_can_use() {
        assert_eq!(session_count_for(4, DEFAULT_VRAM_BUDGET_MB, 1), 1);
        assert_eq!(session_count_for(4, DEFAULT_VRAM_BUDGET_MB, 2), 2);
    }

    #[test]
    fn tiny_vram_budget_reduces_to_one_session() {
        assert_eq!(session_count_for(4, 100, 4), 1);
        assert_eq!(session_count_for(4, SESSION_TRANSIENT_MB * 3, 4), 3);
    }

    #[test]
    fn session_count_clamps_hold() {
        assert_eq!(session_count_for(0, DEFAULT_VRAM_BUDGET_MB, 4), 1);
        assert_eq!(session_count_for(99, DEFAULT_VRAM_BUDGET_MB, 99), 4);
        assert_eq!(session_count_for(4, 0, 0), 1);
    }
}
