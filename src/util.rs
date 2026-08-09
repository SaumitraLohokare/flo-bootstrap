#[derive(Debug, Clone, Copy)]
pub struct Iota {
    counter: usize,
}

impl Iota {
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    /// An iota that starts past `n`, so its ids cannot collide with those of an
    /// earlier one that handed out `n` of them.
    pub fn seeded(n: usize) -> Self {
        Self { counter: n }
    }

    pub fn next(&mut self) -> usize {
        let n = self.counter;
        self.counter += 1;
        n
    }

    /// How many ids have been handed out. Anything from here up is unused.
    pub fn count(&self) -> usize {
        self.counter
    }
}
