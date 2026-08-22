use std::io::{self, Write};

use katana_todo::{Priority, Status, Store, Task};

pub fn run(args: &[String]) -> i32 {
    let cmd = args.first().map(String::as_str).unwrap_or("list");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match dispatch(cmd, rest) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("katana todo: {e}");
            1
        }
    }
}

fn open_store() -> Result<Store, Box<dyn std::error::Error>> {
    let (store, _) = Store::open_default()?;
    Ok(store)
}

fn parse_id(s: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(s.parse::<i64>()?)
}

fn print_task(t: &Task) {
    println!(
        "#{:<4} [{:<11}] {:>3}%  {}",
        t.id, t.status, t.progress, t.title
    );
}

fn dispatch(cmd: &str, rest: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_store()?;
    match cmd {
        "add" | "a" => {
            let title = rest.join(" ");
            if title.is_empty() {
                return Err("usage: todo add TITLE".into());
            }
            let t = store.add(&title, "", Priority::Medium, Status::Pending, 0, "[]", None)?;
            println!("Added task #{}: {}", t.id, t.title);
        }
        "list" | "l" => {
            let all = rest.iter().any(|a| a == "-a" || a == "--all");
            let tasks = if all {
                store.list_all()?
            } else {
                store.list_active()?
            };
            if tasks.is_empty() {
                println!("(no tasks)");
            } else {
                for t in &tasks {
                    print_task(t);
                }
            }
        }
        "show" | "s" => {
            let id = parse_id(rest.first().ok_or("usage: todo show ID")?)?;
            match store.get(id)? {
                Some(t) => {
                    println!("#{} {}", t.id, t.title);
                    println!("status={} priority={} progress={}%", t.status, t.priority, t.progress);
                    if !t.description.is_empty() {
                        println!("{}", t.description);
                    }
                }
                None => return Err(format!("Task #{id} not found").into()),
            }
        }
        "edit" | "e" => {
            let id = parse_id(rest.first().ok_or("usage: todo edit ID --title …")?)?;
            let mut title = None;
            let mut i = 1;
            while i < rest.len() {
                if rest[i] == "--title" || rest[i] == "-i" {
                    i += 1;
                    title = rest.get(i).map(|s| s.as_str());
                }
                i += 1;
            }
            let t = store.update_fields(id, title, None, None, None)?;
            println!("Updated task #{}: {}", t.id, t.title);
        }
        "done" | "d" => {
            let id = parse_id(rest.first().ok_or("usage: todo done ID")?)?;
            let t = store.set_status(id, Status::Completed, Some(100))?;
            println!("Completed #{}: {}", t.id, t.title);
        }
        "undone" | "u" => {
            let id = parse_id(rest.first().ok_or("usage: todo undone ID")?)?;
            let t = store.set_status(id, Status::Pending, Some(0))?;
            println!("Reopened #{}: {}", t.id, t.title);
        }
        "start" | "begin" | "b" => {
            let id = parse_id(rest.first().ok_or("usage: todo start ID")?)?;
            let t = store.set_status(id, Status::InProgress, Some(1))?;
            println!("Started #{}: {} ({}%)", t.id, t.title, t.progress);
        }
        "pause" | "z" => {
            let id = parse_id(rest.first().ok_or("usage: todo pause ID")?)?;
            let t = store.set_status(id, Status::Pending, None)?;
            println!("Paused #{}: {}", t.id, t.title);
        }
        "progress" | "p" => {
            let id = parse_id(rest.first().ok_or("usage: todo progress ID N")?)?;
            let n = rest
                .get(1)
                .ok_or("usage: todo progress ID N")?
                .parse::<i64>()?;
            let t = store.set_progress(id, n)?;
            println!("Progress #{} → {}% ({})", t.id, t.progress, t.status);
        }
        "cancel" | "x" => {
            let id = parse_id(rest.first().ok_or("usage: todo cancel ID")?)?;
            let t = store.set_status(id, Status::Cancelled, None)?;
            println!("Cancelled #{}: {}", t.id, t.title);
        }
        "rm" | "r" => {
            let id = parse_id(rest.first().ok_or("usage: todo rm ID")?)?;
            if store.delete(id, true)? {
                println!("Deleted task #{id} — IDs renumbered");
            } else {
                return Err(format!("Task #{id} not found").into());
            }
        }
        "move" | "m" => {
            let id = parse_id(rest.first().ok_or("usage: todo move ID where")?)?;
            let where_to = rest.get(1).ok_or("usage: todo move ID up|down|top|bottom|N")?;
            let t = store.move_task(id, where_to)?;
            println!("Moved to #{}: {}", t.id, t.title);
        }
        "renumber" | "n" => {
            let m = store.renumber()?;
            println!("Renumbered {} task(s)", m.len());
        }
        "in-progress" | "i" => {
            for t in store.list_status(&["in_progress"])? {
                print_task(&t);
            }
        }
        "stats" | "overview" | "o" => {
            println!("{}", store.stats()?);
        }
        "clear" | "c" => {
            let n = store.clear_completed(true)?;
            println!("Removed {n} task(s) — IDs renumbered");
        }
        "search" | "find" | "f" => {
            let q = rest.join(" ");
            for t in store.search(&q)? {
                print_task(&t);
            }
        }
        "version" | "v" => {
            println!("katana todo {}", env!("CARGO_PKG_VERSION"));
            println!("Database: {}", katana_todo::default_db_path().display());
        }
        other => return Err(format!("unknown command '{other}'").into()),
    }
    let _ = io::stdout().flush();
    Ok(())
}
