//! Suite `rng` (ARFC 0005 section 5): the `Rng` seam, clauses 2.1, 3.1.3,
//! and 3.1.5.

use std::collections::BTreeSet;

use alloy_rt::{Rng, RngSeam};

use crate::support::{Ctx, Gen, fresh, name};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![
        check!(
            "arfc-0005#3.1.5",
            Applies::All,
            arfc0005_s3_1_5_fill_with_empty_dest_is_ok
        ),
        check!(
            "arfc-0005#2.1",
            Applies::All,
            arfc0005_s2_1_fill_fills_all_of_dest
        ),
        check!(
            "arfc-0005#3.1.5",
            Applies::All,
            arfc0005_s3_1_5_successive_fills_differ
        ),
        check!(
            "arfc-0005#3.1.5",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_5_mem_instances_draw_distinct_streams
        ),
        check!(
            "arfc-0005#3.1.5",
            Applies::Only(Implementation::Mem),
            arfc0005_s3_1_5_mem_fill_never_fails
        ),
        check!(
            "arfc-0005#3.1.3",
            Applies::All,
            arfc0005_s3_1_3_fill_may_be_called_concurrently
        ),
    ]
}

/// The length of the windows `filled` checks. A window of 32 random bytes
/// equals a fixed value with probability 2^-256.
const WINDOW: usize = 32;

/// Whether every window of `WINDOW` bytes at a multiple of `WINDOW`, and the
/// last one, holds a byte other than `sentinel`.
fn filled(buf: &[u8], sentinel: u8) -> bool {
    if buf.len() < WINDOW {
        return true;
    }
    let mut starts: Vec<usize> = (0..=buf.len() - WINDOW).step_by(WINDOW).collect();
    starts.push(buf.len() - WINDOW);
    starts.into_iter().all(|start| {
        buf[start..start + WINDOW]
            .iter()
            .any(|&byte| byte != sentinel)
    })
}

/// `fill` with an empty `dest` returns `Ok(())`.
async fn arfc0005_s3_1_5_fill_with_empty_dest_is_ok(env: Env) -> Outcome {
    let result = env.runtime.rng().fill(&mut []);
    ensure_eq!(result, Ok(()), "fill of an empty slice");
    Ok(())
}

/// `fill` writes all of `dest`: no window of the buffer keeps the value it
/// held before.
async fn arfc0005_s2_1_fill_fills_all_of_dest(env: Env) -> Outcome {
    let rng = env.runtime.rng().clone();
    let mut lengths: Vec<usize> = (1..=70).collect();
    lengths.extend([100, 255, 256, 4_095, 4_096, 4_097, 65_536, 1 << 20]);
    for len in lengths {
        for sentinel in [0x00u8, 0xFF] {
            let mut buf = vec![sentinel; len];
            rng.fill(&mut buf).ctx("fill")?;
            ensure!(
                filled(&buf, sentinel),
                "fill of {len} bytes left a {WINDOW}-byte window at {sentinel:#04x}"
            );
        }
    }
    Ok(())
}

/// Successive fills on one instance do not repeat their output.
async fn arfc0005_s3_1_5_successive_fills_differ(env: Env) -> Outcome {
    let rng = env.runtime.rng().clone();
    let mut seen = BTreeSet::new();
    for _ in 0..256 {
        let mut buf = [0u8; WINDOW];
        rng.fill(&mut buf).ctx("fill")?;
        ensure!(
            seen.insert(buf),
            "a fill returned bytes an earlier fill returned"
        );
    }
    Ok(())
}

/// `mem`: no two instances on one host draw the same stream.
async fn arfc0005_s3_1_5_mem_instances_draw_distinct_streams(env: Env) -> Outcome {
    let mut instances: Vec<std::sync::Arc<dyn Rng>> = vec![env.runtime.rng().clone()];
    for _ in 0..8 {
        instances.push(fresh::<RngSeam>(&env, &env.config.rng)?);
    }
    let mut seen = BTreeSet::new();
    for instance in &instances {
        let mut buf = [0u8; WINDOW];
        instance.fill(&mut buf).ctx("fill")?;
        ensure!(
            seen.insert(buf),
            "two rng instances on one host returned the same bytes"
        );
    }
    Ok(())
}

/// `mem`: `fill` never fails, whatever the length.
async fn arfc0005_s3_1_5_mem_fill_never_fails(env: Env) -> Outcome {
    let rng = env.runtime.rng().clone();
    let mut generator = Gen::new(env.seed);
    for _ in 0..1_000 {
        let len = generator.below(65_537) as usize;
        let mut buf = vec![0u8; len];
        ensure_eq!(rng.fill(&mut buf), Ok(()), "mem fill of {len} bytes");
    }
    let mut large = vec![0u8; 16 << 20];
    ensure_eq!(rng.fill(&mut large), Ok(()), "mem fill of 16 MiB");
    Ok(())
}

/// `fill` may be called concurrently from several tasks, and no two calls
/// return the same bytes.
async fn arfc0005_s3_1_3_fill_may_be_called_concurrently(env: Env) -> Outcome {
    let mut handles = Vec::new();
    for task in 0..8 {
        let rng = env.runtime.rng().clone();
        let spawner = env.runtime.spawner().clone();
        let body = async move {
            let mut out = Vec::new();
            for _ in 0..64 {
                let mut buf = [0u8; WINDOW];
                let result = rng.fill(&mut buf);
                out.push((result, buf));
                spawner.yield_now().await;
            }
            out
        };
        handles.push(
            env.runtime
                .spawn(name(&format!("rng-{task}"))?, body)
                .ctx("spawn")?,
        );
    }
    let mut seen = BTreeSet::new();
    for handle in handles {
        for (result, buf) in handle.join().await.ctx("join")? {
            result.ctx("concurrent fill")?;
            ensure!(
                seen.insert(buf),
                "two concurrent fills returned the same bytes"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_rt::RngError;

    #[test]
    fn arfc0005_s2_1_rng_error_text() {
        assert_eq!(
            RngError::Unavailable.to_string(),
            "the platform random generator is unavailable"
        );
    }
}
