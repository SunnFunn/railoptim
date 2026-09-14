//! Пробное чтение HASH `telegrams_db` без полного прогона оптимизации.
//!
//! Секреты — те же, что у `./run.sh` (`REDIS_CONV_PASS` и опционально host/port).
//! Пишет `data/conventions_from_redis.json`.
//!
//!   ./run.sh prod   # полный сервис тоже делает снимок на старте
//!   cargo run --release --bin railoptim-dump-conventions

use anyhow::Result;
use railoptim::data::{dump_conventions_stub, DUMP_PATH};

fn main() -> Result<()> {
    let probe = dump_conventions_stub(None)?;
    println!(
        "conv-redis {}:{} db {} — HASH `{}`: {} записей",
        probe.host, probe.port, probe.db, probe.hash, probe.hash_fields
    );
    println!(
        "снимок телеграммы №{} сохранён в {DUMP_PATH}",
        probe.sample_rzd_number
    );
    println!(
        "  {} | {} | {} → {}",
        probe.sample.cargo_class,
        probe.sample.cargo_name,
        probe.sample.departure_st,
        probe.sample.destination_st
    );
    Ok(())
}
