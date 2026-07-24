#[derive(Debug, Clone, Copy)]
pub struct Iota {
    counter: usize,
}

impl Iota {
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    pub fn next(&mut self) -> usize {
        let n = self.counter;
        self.counter += 1;
        n
    }
}
