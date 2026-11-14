//! Section 5.5: an export from each implementation installed into the other
//! gives equal contents and an identical export. These checks run on the
//! `native` harness, whose registry also holds `mem`.

use crate::model::StateModel;
use crate::state::ApplyBatch;
use crate::support::{Ctx, Gen, build_of, contents, export_now, id, install, place_config_of, put};
use crate::{Applies, Check, Env, Implementation, Outcome};

pub(crate) fn checks() -> Vec<Check> {
    vec![check!(
        "arfc-0006#5.5",
        Applies::Only(Implementation::Native),
        arfc0006_s5_5_export_installs_across_implementations
    )]
}

/// For each direction, a store of one implementation with random contents
/// exports, a store of the other with other contents installs the export,
/// and the target then holds equal contents and position and exports
/// identical bytes.
async fn arfc0006_s5_5_export_installs_across_implementations(env: Env) -> Outcome {
    let mut data = Gen::new(env.seed);
    for (from, to) in [
        (Implementation::Native, Implementation::Mem),
        (Implementation::Mem, Implementation::Native),
    ] {
        let what = format!("{from:?} to {to:?}");
        let source = build_of(&env, from, &place_config_of(&env, from, 0)?)?
            .ctx(&format!("{what}: the source"))?;
        let target = build_of(&env, to, &place_config_of(&env, to, 1)?)?
            .ctx(&format!("{what}: the target"))?;
        let mut ops = Vec::new();
        for n in 0..200u32 {
            let mut key = data.key();
            key.extend_from_slice(&n.to_be_bytes());
            let len = data.below(300) as usize;
            ops.push(put(&key, &data.bytes(len)));
        }
        let at = id(4, 1 + data.below(1_000));
        source
            .apply(ApplyBatch { log_id: at, ops })
            .await
            .ctx(&format!("{what}: apply to the source"))?;
        target
            .apply(ApplyBatch {
                log_id: id(9, 5_000),
                ops: vec![put(b"stale", b"target contents")],
            })
            .await
            .ctx(&format!("{what}: apply to the target"))?;

        let export = export_now(&*source).await?;
        let got = install(&*target, &export, 777)
            .await?
            .ctx(&format!("{what}: install"))?;
        ensure_eq!(got, Some(at), "{what}: the position finish returns");
        ensure_eq!(
            target.applied().await.ctx("applied")?,
            Some(at),
            "{what}: the target's position"
        );
        let want = contents(&*source).await?;
        let held = contents(&*target).await?;
        ensure!(
            held == want,
            "{what}: the target's contents differ from the source's"
        );
        let model = StateModel {
            applied: Some(at),
            map: want,
        };
        let again = export_now(&*target).await?;
        ensure!(
            again == export,
            "{what}: the target exports different bytes"
        );
        ensure!(
            again == model.export(),
            "{what}: the export differs from section 4.1"
        );
    }
    Ok(())
}
