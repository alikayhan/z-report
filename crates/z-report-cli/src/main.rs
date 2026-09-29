mod rpc;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::os::unix::process::CommandExt;
use std::{
    io::Read,
    process::{Command, Stdio},
};
use z_report_core::{
    engine::{self, Engine},
    store::Store,
};

fn start(engine: &Engine, owner: &str, automatic: bool, interactive: bool) -> Result<Value> {
    anyhow::ensure!(
        !owner.is_empty() && owner.len() <= 128,
        "Invalid read owner"
    );
    let store = engine.store.lock().unwrap();
    let _launch = engine::lock(&store, "launch")?;
    store.recover_reads()?;
    if automatic
        && (!interactive
            || std::env::var_os("Z_REPORT_EVALUATOR").is_some()
            || !store.auto_due(chrono::Utc::now(), true)?)
    {
        return Ok(json!(null));
    }
    if let Some(read) = store.read_cycle(None)? {
        if matches!(read.status.as_str(), "running" | "queued") {
            return Ok(json!({"read":read,"owned":false}));
        }
    }
    let id = engine::new_id();
    store.queue_read(
        &id,
        if automatic { "auto" } else { "xread" },
        owner,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    let dir = store
        .path
        .parent()
        .context("Invalid data directory")?
        .to_path_buf();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--data-dir")
        .arg(&dir)
        .arg("worker")
        .arg(&id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match command.spawn() {
        Ok(_) => Ok(json!({"read":store.read_cycle(Some(&id))?,"owned":true})),
        Err(e) => {
            store.finish_read(&id, "failed", &format!("Cannot launch read: {e}"))?;
            Err(e.into())
        }
    }
}

fn run() -> Result<Value> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--data-dir") {
        anyhow::ensure!(args.len() >= 3, "Missing data directory or command");
        let path = std::path::PathBuf::from(&args[1]);
        anyhow::ensure!(path.is_absolute(), "Data directory must be absolute");
        std::env::set_var("Z_REPORT_DATA_DIR", path);
        args.drain(..2);
    }
    let command = args.first().map(String::as_str).unwrap_or("info");
    if command == "info" {
        return Ok(
            json!({"engine":"z-report","version":env!("CARGO_PKG_VERSION"),"protocol":engine::PROTOCOL,"database_version":engine::DATABASE_VERSION,"tested_host":"2.1.273","target":format!("{}-{}",std::env::consts::ARCH,std::env::consts::OS)}),
        );
    }
    let request = if command == "rpc" {
        let request: rpc::Request = serde_json::from_reader(std::io::stdin().take(2_000_000))?;
        anyhow::ensure!(
            request.protocol == engine::PROTOCOL,
            "Incompatible protocol; reinstall the matching Z Report package"
        );
        Some(request)
    } else {
        None
    };
    let store = Store::open_without_upgrade(Store::default_path()?)?;
    let engine = Engine::new(store);
    match command {
        "rpc" => rpc::dispatch(&engine, request.unwrap()),
        "worker" => {
            let id = args.get(1).context("Missing read identifier")?;
            let result = engine::run_read(&engine, id, true, &|_| {});
            if let Err(ref e) = result {
                let _ = engine.store.lock().unwrap().finish_read(
                    id,
                    if e.to_string().starts_with("busy:") {
                        "busy"
                    } else {
                        "failed"
                    },
                    &e.to_string(),
                );
            }
            Ok(json!(result?))
        }
        "x-read" => {
            let id = engine::new_id();
            engine.store.lock().unwrap().queue_read(
                &id,
                "xread",
                "cli",
                &chrono::Utc::now().to_rfc3339(),
            )?;
            let read = engine::run_read(&engine, &id, false, &|r| {
                eprintln!("{}: {}", r.status, r.message)
            })?;
            anyhow::ensure!(
                read.status == "completed",
                "Read {}: {}",
                read.status,
                read.message
            );
            Ok(json!(read))
        }
        _ => anyhow::bail!("Usage: z-report [--data-dir PATH] info|rpc|x-read"),
    }
}

fn main() {
    unsafe {
        libc::umask(0o077);
    }
    match run() {
        Ok(data) => println!(
            "{}",
            json!({"protocol":engine::PROTOCOL,"ok":true,"data":data})
        ),
        Err(e) => {
            let message = format!("{e:#}");
            let (code, exit) = if message.starts_with("busy:") {
                ("busy", 3)
            } else if message.starts_with("conflict:") {
                ("conflict", 4)
            } else if message.contains("protocol") || message.contains("schema") {
                ("incompatible", 5)
            } else {
                ("request_failed", 2)
            };
            eprintln!("{message}");
            println!(
                "{}",
                json!({"protocol":engine::PROTOCOL,"ok":false,"error":{"code":code,"message":message}})
            );
            std::process::exit(exit);
        }
    }
}
