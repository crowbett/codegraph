mod log;
mod parse;
mod store;

use log::{Sink, Stdout};
use parse::*;
use store::Store;

const SCRIPT: &str = "set a 1\nset b 2\nget a\ndel b\nget b\ncount\nput c";

fn main() {
    let mut store = Store::new();
    let mut out = Stdout::default();
    let mut errors = Vec::new();
    for line in SCRIPT.lines() {
        match parse_line(line) {
            Ok(cmd) => run(&mut store, &mut out, cmd),
            Err(e) => {
                log::report(&mut out, &e.message);
                errors.push(e);
            }
        }
    }
    for e in &errors {
        if e.is_fatal() {
            out.write("stopped");
        }
    }
    out.flush();
}

fn run(store: &mut Store, out: &mut dyn Sink, cmd: Command) {
    match cmd {
        Command::Set(k, v) => store.set(k, v),
        Command::Get(k) => match store.get(&k) {
            Some(v) => out.write(v),
            None => log::report(out, "missing key"),
        },
        Command::Del(k) => {
            store.delete(&k);
        }
        Command::Count => out.write(&store.len().to_string()),
    }
}
