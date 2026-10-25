pub trait Sink {
    fn write(&mut self, line: &str);
}

#[derive(Default)]
pub struct Stdout {
    lines: Vec<String>,
}

impl Sink for Stdout {
    fn write(&mut self, line: &str) {
        self.lines.push(line.to_string());
    }
}

impl Stdout {
    pub fn flush(&mut self) {
        for line in self.lines.drain(..) {
            println!("{line}");
        }
    }
}

pub fn report(out: &mut dyn Sink, err: &str) {
    out.write(&format!("error: {err}"));
}
