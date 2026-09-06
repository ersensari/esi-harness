use std::{error::Error, fs, hint::black_box, path::Path, time::Instant};

use fjall::{Database as FjallDb, KeyspaceCreateOptions, PersistMode};
use heed::{
    types::{Bytes, Str},
    Database as HeedDb, EnvOpenOptions,
};
use redb::{Database as RedbDb, ReadableDatabase, TableDefinition};
use rusqlite::{params, Connection};

const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("contexts");

struct Metrics {
    name: &'static str,
    write_ms: f64,
    reopen_ms: f64,
    read_ms: f64,
    scan_ms: f64,
    bytes: u64,
    sum: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let quick = std::env::args().any(|arg| arg == "--quick");
    let (count, reads) = if quick {
        (10_000, 100_000)
    } else {
        (50_000, 500_000)
    };
    let data = make_data(count, 4_096);
    let order = make_order(reads, count);
    let prefixes = (0..1_000)
        .map(|i| format!("s{:04}:", i % 100))
        .collect::<Vec<_>>();
    let root = tempfile::tempdir()?;
    println!(
        "records={count} value_bytes=4096 point_reads={reads}; durable batch write; warm OS cache"
    );
    let mut rows = Vec::new();
    for (name, run) in [
        ("sqlite", sqlite as BenchFn),
        ("redb", redb as BenchFn),
        ("heed", heed as BenchFn),
        ("fjall", fjall as BenchFn),
    ] {
        eprintln!("running {name}...");
        rows.push(run(root.path(), &data, &order, &prefixes)?);
    }
    println!(
        "{:<8} {:>10} {:>10} {:>12} {:>12} {:>10}",
        "engine", "write_ms", "reopen_ms", "point_M/s", "scan_k/s", "disk_MiB"
    );
    for row in rows {
        println!(
            "{:<8} {:>10.2} {:>10.2} {:>12.3} {:>12.3} {:>10.2}",
            row.name,
            row.write_ms,
            row.reopen_ms,
            reads as f64 / row.read_ms / 1_000.0,
            prefixes.len() as f64 / row.scan_ms,
            row.bytes as f64 / 1_048_576.0
        );
        black_box(row.sum);
    }
    Ok(())
}

type BenchFn =
    fn(&Path, &[(String, Vec<u8>)], &[usize], &[String]) -> Result<Metrics, Box<dyn Error>>;

fn make_data(count: usize, size: usize) -> Vec<(String, Vec<u8>)> {
    (0..count)
        .map(|i| {
            let key = format!("s{:04}:m{:08}", i % 100, i / 100);
            let value = (0..size).map(|j| ((i * 31 + j * 17) % 251) as u8).collect();
            (key, value)
        })
        .collect()
}

fn make_order(count: usize, modulo: usize) -> Vec<usize> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize % modulo
        })
        .collect()
}

fn size(path: &Path) -> Result<u64, Box<dyn Error>> {
    if path.is_file() {
        return Ok(path.metadata()?.len());
    }
    fs::read_dir(path)?.try_fold(0, |sum, entry| Ok(sum + size(&entry?.path())?))
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1_000.0
}
fn upper(prefix: &str) -> String {
    format!("{prefix}\u{10ffff}")
}

fn sqlite(
    root: &Path,
    data: &[(String, Vec<u8>)],
    order: &[usize],
    prefixes: &[String],
) -> Result<Metrics, Box<dyn Error>> {
    let path = root.join("sqlite.db");
    let mut db = Connection::open(&path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE contexts (key TEXT PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID;")?;
    let start = Instant::now();
    {
        let tx = db.transaction()?;
        {
            let mut q = tx.prepare("INSERT INTO contexts VALUES (?1,?2)")?;
            for (k, v) in data {
                q.execute(params![k, v])?;
            }
        }
        tx.commit()?;
    }
    let write_ms = ms(start);
    drop(db);
    let start = Instant::now();
    let db = Connection::open(&path)?;
    let reopen_ms = ms(start);
    let mut sum = 0;
    let start = Instant::now();
    {
        let mut q = db.prepare_cached("SELECT value FROM contexts WHERE key=?1")?;
        for &i in order {
            let v: Vec<u8> = q.query_row([&data[i].0], |r| r.get(0))?;
            sum += v[0] as u64;
        }
    }
    let read_ms = ms(start);
    let start = Instant::now();
    {
        let mut q = db.prepare_cached(
            "SELECT value FROM contexts WHERE key>=?1 AND key<?2 ORDER BY key LIMIT 100",
        )?;
        for p in prefixes {
            let mut rows = q.query(params![p, upper(p)])?;
            while let Some(r) = rows.next()? {
                let v: Vec<u8> = r.get(0)?;
                sum += v[0] as u64;
            }
        }
    }
    let scan_ms = ms(start);
    drop(db);
    Ok(Metrics {
        name: "sqlite",
        write_ms,
        reopen_ms,
        read_ms,
        scan_ms,
        bytes: size(&path)?,
        sum,
    })
}

fn redb(
    root: &Path,
    data: &[(String, Vec<u8>)],
    order: &[usize],
    prefixes: &[String],
) -> Result<Metrics, Box<dyn Error>> {
    let path = root.join("redb.db");
    let db = RedbDb::create(&path)?;
    let start = Instant::now();
    {
        let tx = db.begin_write()?;
        {
            let mut t = tx.open_table(TABLE)?;
            for (k, v) in data {
                t.insert(k.as_str(), v.as_slice())?;
            }
        }
        tx.commit()?;
    }
    let write_ms = ms(start);
    drop(db);
    let start = Instant::now();
    let db = RedbDb::open(&path)?;
    let reopen_ms = ms(start);
    let tx = db.begin_read()?;
    let t = tx.open_table(TABLE)?;
    let mut sum = 0;
    let start = Instant::now();
    for &i in order {
        let v = t.get(data[i].0.as_str())?.unwrap();
        sum += v.value()[0] as u64;
    }
    let read_ms = ms(start);
    let start = Instant::now();
    for p in prefixes {
        let hi = upper(p);
        for item in t.range(p.as_str()..hi.as_str())?.take(100) {
            sum += item?.1.value()[0] as u64;
        }
    }
    let scan_ms = ms(start);
    drop(t);
    drop(tx);
    drop(db);
    Ok(Metrics {
        name: "redb",
        write_ms,
        reopen_ms,
        read_ms,
        scan_ms,
        bytes: size(&path)?,
        sum,
    })
}

fn open_heed(path: &Path) -> Result<heed::Env, Box<dyn Error>> {
    fs::create_dir_all(path)?;
    Ok(unsafe {
        EnvOpenOptions::new()
            .map_size(512 * 1024 * 1024)
            .max_dbs(4)
            .open(path)?
    })
}

fn heed(
    root: &Path,
    data: &[(String, Vec<u8>)],
    order: &[usize],
    prefixes: &[String],
) -> Result<Metrics, Box<dyn Error>> {
    let path = root.join("heed");
    let env = open_heed(&path)?;
    let start = Instant::now();
    let _db: HeedDb<Str, Bytes> = {
        let mut tx = env.write_txn()?;
        let db = env.create_database(&mut tx, Some("contexts"))?;
        for (k, v) in data {
            db.put(&mut tx, k.as_str(), v.as_slice())?;
        }
        tx.commit()?;
        db
    };
    let write_ms = ms(start);
    env.prepare_for_closing().wait();
    let start = Instant::now();
    let env = open_heed(&path)?;
    let tx = env.read_txn()?;
    let db: HeedDb<Str, Bytes> = env.open_database(&tx, Some("contexts"))?.unwrap();
    let reopen_ms = ms(start);
    let mut sum = 0;
    let start = Instant::now();
    for &i in order {
        sum += db.get(&tx, data[i].0.as_str())?.unwrap()[0] as u64;
    }
    let read_ms = ms(start);
    let start = Instant::now();
    for p in prefixes {
        for item in db.prefix_iter(&tx, p)?.take(100) {
            sum += item?.1[0] as u64;
        }
    }
    let scan_ms = ms(start);
    drop(tx);
    env.prepare_for_closing().wait();
    Ok(Metrics {
        name: "heed",
        write_ms,
        reopen_ms,
        read_ms,
        scan_ms,
        bytes: size(&path)?,
        sum,
    })
}

fn fjall(
    root: &Path,
    data: &[(String, Vec<u8>)],
    order: &[usize],
    prefixes: &[String],
) -> Result<Metrics, Box<dyn Error>> {
    let path = root.join("fjall");
    let db = FjallDb::builder(&path).open()?;
    let ks = db.keyspace("contexts", KeyspaceCreateOptions::default)?;
    let start = Instant::now();
    for (k, v) in data {
        ks.insert(k, v)?;
    }
    db.persist(PersistMode::SyncAll)?;
    let write_ms = ms(start);
    drop(ks);
    drop(db);
    let start = Instant::now();
    let db = FjallDb::builder(&path).open()?;
    let ks = db.keyspace("contexts", KeyspaceCreateOptions::default)?;
    let reopen_ms = ms(start);
    let mut sum = 0;
    let start = Instant::now();
    for &i in order {
        sum += ks.get(data[i].0.as_bytes())?.unwrap()[0] as u64;
    }
    let read_ms = ms(start);
    let start = Instant::now();
    for p in prefixes {
        for item in ks.prefix(p.as_bytes()).take(100) {
            sum += item.value()?[0] as u64;
        }
    }
    let scan_ms = ms(start);
    drop(ks);
    drop(db);
    Ok(Metrics {
        name: "fjall",
        write_ms,
        reopen_ms,
        read_ms,
        scan_ms,
        bytes: size(&path)?,
        sum,
    })
}
