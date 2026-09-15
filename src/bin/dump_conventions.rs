//! Чтение HASH `telegrams_db` и разбор шага 2 без полного прогона оптимизации.
//!
//! Секреты — те же, что у `./run.sh` (`REDIS_CONV_PASS`).
//! Пишет `data/conventions_from_redis.json` (только действующие телеграммы).
//!
//!   cargo run --release --bin railoptim-dump-conventions

use anyhow::Result;
use railoptim::data::{dump_conventions_stub, ConventionIndex, DUMP_PATH};

fn main() -> Result<()> {
    let probe = dump_conventions_stub(None)?;
    let st = &probe.load.stats;
    println!(
        "conv-redis {}:{} db {} — HASH `{}`: {} записей в Redis, в сервис взято {} действующих → {DUMP_PATH}",
        probe.host, probe.port, probe.db, probe.hash, probe.hash_fields, st.active
    );
    println!(
        "  не взяты: истекло {}, ещё не началось {}, без дат {}; битый JSON {}; Others/ошибка {}; только КЗХ {}",
        st.expired, st.not_yet, st.empty_dates, st.bad_json, st.skipped_class, st.skipped_kzh,
    );
    println!(
        "  действующих: Empty {}, All {}, Grain {}; из них промывка/ремонт/отстой {}",
        st.active_empty, st.active_all, st.active_grain, st.active_service,
    );
    let index = ConventionIndex::build(probe.load.active);
    println!("  {}", index.summary_line());
    index.log_geography();
    Ok(())
}
